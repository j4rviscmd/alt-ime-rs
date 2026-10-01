//! セルフアップデート適用: 検知済みタグの asset を DL して実行中 exe を差し替え、
//! 即時再起動する。更新の検知は update.rs、UI 政策(ダイアログ)は tray.rs が担う。
//!
//! 仕組み:
//! - apply_async(hwnd, tag) が別スレッドで DL・差し替えを行う。
//!   Why 別スレッド: update.rs check_async と同じく、メインスレッドは LL キーボード
//!   フックのメッセージポンプを回すスレッドのため、通信・ファイルIOでブロックすると
//!   キーボード入力が効かなくなる。
//! - 差し替えは Windows の制約(実行中 exe は削除・上書き不可、リネームは可)に従い、
//!   「実行中 exe を .old へリネーム → DL 済みの .new を正規名へリネーム」の2段で行う。
//! - 差し替え後は新 exe を起動し、結果を Box<ApplyOutcome> で PostMessageW
//!   (WM_APP_UPDATE_APPLIED)によりトレイウィンドウ(メインスレッド)へ受け渡す。
//!   成功時のプロセス終了と失敗時のフォールバック表示は tray.rs が行う。
//! - .old(旧exe)の削除は自プロセスでは不可能(自分のイメージのため)。次回起動時の
//!   cleanup_old() で存在をチェックして1回だけ削除を試み、ロック中(旧プロセス退出前)
//!   なら次回起動に持ち越す。
//!
//! Why ハッシュ検証をしない: 配信の正当性は TLS(github.com の証明書検証)に委ねる。
//!   未署名 exe のチェックサムを同一 Release に添えても鯖側改竄の検知にはならず、
//!   手動DLと同一の信任境界のため追加しない。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::update;

// asset の DL 元(HTTPS)。
// Why 決定論的URL: GitHub Releases の asset URL は /releases/download/{tag}/{asset} 形式で
//   安定しているため、Releases API の assets JSON を解析せず組立てられる。タグは
//   latest_semver_tag が数値3部形式と検証済みの値のみ流通させるため経路の注入も無い。
const DL_HOST: &str = "github.com";
const DL_PATH_PREFIX: &str = "/j4rviscmd/alt-ime-rs/releases/download/";
const ASSET_NAME: &str = "alt-ime-rs.exe";
// DL のバイト上限。実 exe は数百KB程度。異常応答での無限読み・書込防止用。
const MAX_DOWNLOAD_BYTES: usize = 32 * 1024 * 1024;

// 適用中(スレッド起動済みで結果未着)かを示すガード(update.rs CHECKING と同パターン)。
// Why: 同意ダイアログの二重表示や適用中の再適用を防ぐ。最初の1件だけ受け付ける。
static APPLYING: AtomicBool = AtomicBool::new(false);

/// 適用の結果。
pub(crate) enum ApplyOutcome {
    /// 差し替えと新 exe の起動に成功。呼び出し側プロセスは終了して新旧を交代させる。
    Applied,
    /// 失敗。exe は無傷(または復元済み)。手動導線(配布ページ)へフォールバック表示する。
    ApplyFailed,
}

// PostMessage でトレイウィンドウへ受け渡す結果は Box<ApplyOutcome> をそのまま使う
// (update.rs CheckResult と同パターン。単一フィールドのためラップ構造体は持たない)。

/// 適用中か。適用中の「アップデートを確認する」受付抑制など UI 側の判定に使う。
pub(crate) fn is_applying() -> bool {
    APPLYING.load(Ordering::SeqCst)
}

/// 同意済みの更新(tag)を別スレッドで適用する。既に適用中の場合は無視する(多重適用防止)。
/// Why 別スレッド: DL は秒単位かかり、メインスレッド(=LLフックのポンプ)で実行すると
///   フックを剥がされる。即座にスレッドを起動してリターンする構造(check_async と同一)。
pub(crate) fn apply_async(hwnd: HWND, tag: String) {
    // CAS で既存の適用中を弾く
    if APPLYING.swap(true, Ordering::SeqCst) {
        return;
    }
    // HWND(*mut c_void) は Send でないため isize 経由でスレッドへ受け渡す
    // (PostMessageW 用途のみなら安全。update.rs check_async と同じ定石)。
    let hwnd_raw = hwnd as isize;
    std::thread::spawn(move || unsafe {
        let outcome = apply(&tag);
        // 成功時も解除するが、程なくプロセスが終了するため再入は実質起きない。
        APPLYING.store(false, Ordering::SeqCst);
        let result = Box::new(outcome);
        let raw = Box::into_raw(result) as isize;
        // Why: PostMessage 失敗時(キュー満杯等、極めて稀)は受け手が居ないため
        //   ここで解放してメモリリークを防ぐ。
        if PostMessageW(hwnd_raw as HWND, crate::WM_APP_UPDATE_APPLIED, 0, raw) == 0 {
            let _ = Box::from_raw(raw as *mut ApplyOutcome);
        }
    });
}

/// 適用の本体(ワーカースレッド実行)。DL→.new書出→差し替え→新起動の順で行う。
/// Why この順序: 新 exe の起動を 成功通知の前 に行うことで、起動失敗時も旧プロセスが
///   生存したまま失敗を報告できる(ユーザを放置しない)。
/// Why panic-free: panic = "abort" でワーカがプロセスを道連れにするため、失敗は
///   Result/Option の伝播のみで扱う(update.rs と同じ規約)。
unsafe fn apply(tag: &str) -> ApplyOutcome {
    let Ok(exe) = std::env::current_exe() else {
        return ApplyOutcome::ApplyFailed;
    };
    let new_exe = sibling(&exe, ".new");
    let old_exe = sibling(&exe, ".old");
    // DL。github.com→objects.githubusercontent.com へのリダイレクトは
    // update::http_get_bytes(WinHTTP 既定ポリシー)が追従し、最終 200 を検査する。
    let Some(bytes) = update::http_get_bytes(DL_HOST, &download_path(tag), MAX_DOWNLOAD_BYTES)
    else {
        return ApplyOutcome::ApplyFailed;
    };
    // .new へ書出。fs::write は既存 .new を上書きするため前回中断残骸の回収を兼ねる。
    if std::fs::write(&new_exe, bytes).is_err() {
        return ApplyOutcome::ApplyFailed;
    }
    if !swap_files(&exe, &new_exe, &old_exe) {
        // swap_files 内でロールバック済み。取り残した .new は次回適用の上書きで回収。
        let _ = std::fs::remove_file(&new_exe);
        return ApplyOutcome::ApplyFailed;
    }
    // 新 exe を起動する(GUI サブシステム同士のためコンソールは出ない)。
    // Why std::process::Command: CreateProcessW 相当の std API でハンドル管理が不要。
    //   子は独立プロセスとして動き、Child を drop しても生存し続ける。
    if std::process::Command::new(&exe).spawn().is_err() {
        // この時点でディスクは新 exe。旧プロセスは継続稼働し、次回起動で新が読み込まれる。
        return ApplyOutcome::ApplyFailed;
    }
    ApplyOutcome::Applied
}

/// exe を .new で差し替える。失敗時は元の exe へロールバックする。
/// Why リネーム2段: 実行中 exe は削除・上書きできないがリネームは可能なため、
///   正規名を一時的に空けてから .new を入り込ませる。
/// Why .old が既にあってもよい: std::fs::rename は Windows で MOVEFILE_REPLACE_EXISTING
///   相当のため、前回更新の掃き残しをそのまま置換する。
fn swap_files(exe: &Path, new_exe: &Path, old_exe: &Path) -> bool {
    if std::fs::rename(exe, old_exe).is_err() {
        return false;
    }
    if std::fs::rename(new_exe, exe).is_err() {
        // 正規名が消えた中間状態を残さないため、.old から戻す(ベストエフォート)。
        let _ = std::fs::rename(old_exe, exe);
        return false;
    }
    true
}

/// 前回セルフアップデートの .old(旧exe)を、起動時に1回だけ削除する。
/// Why 1回だけ: 更新直後の起動では旧プロセス退出前でロックされている可能性が高く、
///   リトライの利益は薄い。失敗時は次回起動で再度試みるか、次回適用のリネームで
///   置換される。残っても数百KBの無害なファイルです。
pub(crate) fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let old_exe = sibling(&exe, ".old");
    if old_exe.exists() {
        let _ = std::fs::remove_file(&old_exe);
    }
}

/// DL 要求パスを組む(例: /j4rviscmd/alt-ime-rs/releases/download/v1.2.3/alt-ime-rs.exe)。
fn download_path(tag: &str) -> String {
    format!("{DL_PATH_PREFIX}{tag}/{ASSET_NAME}")
}

/// パスへ接尾辞を連結した兄弟パス(exe → exe.new / exe.old)。
/// Why 拡張子置換でなく連結: ユーザが exe をリネームしていても
///   「正規名+固定接尾辞」の対応が保たれるため。
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 決定論的DLパスの形式を固定(タグ検証済み前提の URL 組立のデグレ検知)
    #[test]
    fn builds_download_path() {
        assert_eq!(
            download_path("v1.2.3"),
            "/j4rviscmd/alt-ime-rs/releases/download/v1.2.3/alt-ime-rs.exe"
        );
    }

    // 接尾辞は拡張子置換でなく単純連結(exe.new / exe.old)
    #[test]
    fn sibling_appends_suffix() {
        let p = Path::new(r"C:\tools\alt-ime-rs.exe");
        assert_eq!(
            sibling(p, ".new"),
            PathBuf::from(r"C:\tools\alt-ime-rs.exe.new")
        );
        assert_eq!(
            sibling(p, ".old"),
            PathBuf::from(r"C:\tools\alt-ime-rs.exe.old")
        );
    }

    // 差し替え成功: 正規名が新内容に、.old が旧内容になる(リネーム順序のデグレ検知)
    #[test]
    fn swaps_files() {
        let dir = std::env::temp_dir().join(format!("alt-ime-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("alt-ime-rs.exe");
        let new_exe = sibling(&exe, ".new");
        let old_exe = sibling(&exe, ".old");
        // 前回実行の残骸を掃いてから準備
        let _ = std::fs::remove_file(&old_exe);
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new_exe, b"new").unwrap();

        assert!(swap_files(&exe, &new_exe, &old_exe));
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(&old_exe).unwrap(), b"old");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // .new が欠けている場合は失敗し、exe は旧内容のまま復元される(中間状態を残さない)
    #[test]
    fn swap_rolls_back_when_new_missing() {
        let dir = std::env::temp_dir().join(format!("alt-ime-rollback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("alt-ime-rs.exe");
        let new_exe = sibling(&exe, ".new");
        let old_exe = sibling(&exe, ".old");
        let _ = std::fs::remove_file(&new_exe);
        let _ = std::fs::remove_file(&old_exe);
        std::fs::write(&exe, b"old").unwrap();

        assert!(!swap_files(&exe, &new_exe, &old_exe));
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!old_exe.exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // exe が欠けている場合は第一リネームで失敗する(false)。例外やパニックではなく
    // 呼び出し側(apply)の失敗扱いに伝播する
    #[test]
    fn swap_fails_when_exe_missing() {
        let dir = std::env::temp_dir().join(format!("alt-ime-noexe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("alt-ime-rs.exe");
        let new_exe = sibling(&exe, ".new");
        let old_exe = sibling(&exe, ".old");
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::remove_file(&old_exe);
        std::fs::write(&new_exe, b"new").unwrap();

        assert!(!swap_files(&exe, &new_exe, &old_exe));
        // .new は手つかずで残る(apply 側の remove_file で回収される)
        assert_eq!(std::fs::read(&new_exe).unwrap(), b"new");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // 前回適用の掃き残し .old が既にあっても、rename の置換(MOVEFILE_REPLACE_EXISTING
    // 相当)で差し替えが成功する。「.old が既にあってよい」設計のデグレ検知
    #[test]
    fn swap_replaces_stale_old() {
        let dir = std::env::temp_dir().join(format!("alt-ime-staleold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("alt-ime-rs.exe");
        let new_exe = sibling(&exe, ".new");
        let old_exe = sibling(&exe, ".old");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(&new_exe, b"new").unwrap();
        std::fs::write(&old_exe, b"stale").unwrap();

        assert!(swap_files(&exe, &new_exe, &old_exe));
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(&old_exe).unwrap(), b"old");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // 起動時の掃除: current_exe の兄弟 .old を削除する。存在しなければ何もしない
    // (実行中のテストバイナリ自身は Windows 上削除できないため .old の作成・削除のみ検証)
    #[test]
    fn cleanup_old_removes_stale_sibling() {
        let exe = std::env::current_exe().unwrap();
        let old_exe = sibling(&exe, ".old");
        // 存在しない状態での呼び出し(no-op)で誤削除しないことを先に確認
        let _ = std::fs::remove_file(&old_exe);
        cleanup_old();
        assert!(!old_exe.exists());

        std::fs::write(&old_exe, b"stale").unwrap();
        cleanup_old();
        assert!(!old_exe.exists());
    }
}
