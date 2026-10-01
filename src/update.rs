//! アップデート確認: GitHub Releases API で最新版を取得し現在版と比較する。
//!
//! 仕組み:
//! - check_async(hwnd, trigger) が別スレッドで WinHTTP GET を行う。
//!   Why メインスレッドで通信しない: LL キーボードフックはメインスレッドの
//!   メッセージポンプに依存する。同期的に通信するとポンプが止まり、Windows が
//!   フックを LowLevelHooksTimeout で強制解除してキーボード入力が効かなくなる。
//! - 結果を Box<CheckResult> でヒープ確保し PostMessageW(WM_APP_UPDATE_RESULT) で
//!   トレイウィンドウ(メインスレッド)へ受け渡す。UI 表示は tray.rs が行う。
//! - 現在版は build.rs が焼き込んだ APP_VERSION。CIのリリースビルドでは Release PR が
//!   バンプした Cargo.toml の version が使われる(ALT_IME_VERSION はローカル上書き用)。
//!
//! Why Token を使わない: 公開リポジトリの /releases/latest は未認証で取得でき、
//!   未認証でも 60req/時/IP あり手動+起動時の 1req/起動 ではまず到達しない。
//!   逆に公開 exe への Token 埋め込みは抽出・悪用リスクを招くため行わない。

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT,
    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::wide;

// GitHub API のエンドポイント(HTTPS)
// Why Releases一覧API: /releases/latest は旧CalVer版利用者への案内用sentinel
//   (v9999.99.99)を常に返す運用のため、そこからSemVerタグを選ぶことができない。
//   10件=数年分(sentinelとCalVerタグを差し引いても実用上十分)。
const API_HOST: &str = "api.github.com";
const API_PATH: &str = "/repos/j4rviscmd/alt-ime-rs/releases?per_page=10";
// 配布ページ(ブラウザで開く)。/releases/latest は最新Releaseへリダイレクトされる。
pub(crate) const RELEASES_URL: &str = "https://github.com/j4rviscmd/alt-ime-rs/releases/latest";
// User-Agent。GitHub API は User-Agent 無しの要求を 403 で弾くため必須。
const USER_AGENT: &str = "alt-ime-rs";

// ビルド時に焼き込まれた現在版(CIではRelease PRがバンプしたCargo.tomlのversion。
// ALT_IME_VERSION 環境変数はローカルで版を明示したいときの上書き経路)。
pub(crate) const APP_VERSION: &str = env!("ALT_IME_VERSION");

// 通信タイムアウト(ミリ秒)。resolve / connect / send / receive すべてに適用。
// Why: 通信先の異常時やオフライン時にワーカースレッドが永続滞留し、多重起動ガードが
//   張りっぱなしになるのを防ぐため、現実的な上限を設ける。
const TIMEOUT_MS: i32 = 15_000;
// レスポンス本文の読み込み上限。異常応答での無限読み防止用。
const MAX_BODY_BYTES: usize = 512 * 1024;

// 確認中(スレッド起動済みで結果未着)かを示すガード。
// Why: メニュー連打や起動時チェックと手動チェックの重複で、複数スレッド・複数ダイアログが
//   出るのを防ぐ。最初の1件だけ受け付ける。
static CHECKING: AtomicBool = AtomicBool::new(false);

/// 確認のトリガ。表示政策を分ける。
#[derive(Clone, Copy)]
pub(crate) enum Trigger {
    /// ユーザ操作による手動確認。最新時・失敗時もダイアログを出す。
    Manual,
    /// 起動時の自動確認。更新ありの時だけダイアログを出し、最新時・失敗時は静か。
    Startup,
}

/// 通信・比較の結果。
pub(crate) enum Outcome {
    /// 新しいバージョンあり(タグ名、例 "v2026.07.04")。
    UpdateAvailable(String),
    /// 既に最新(タグ名)。
    UpToDate(String),
    /// 通信・解析失敗。
    Failed,
}

/// PostMessage でトレイウィンドウへ受け渡す結果。
pub(crate) struct CheckResult {
    pub trigger: Trigger,
    pub outcome: Outcome,
}

/// アップデート確認を別スレッドで開始する。既に確認中の場合は無視する(多重起動防止)。
/// Why: 呼び出し元(トレイのメッセージハンドラ)はメインスレッドで、ここから同期的に
///   通信すると LL フックがブロックされてキーボード入力が止まる。即座にスレッドを
///   起動してリターンする構造。
pub(crate) fn check_async(hwnd: HWND, trigger: Trigger) {
    // CAS で既存の確認中を弾く
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    // HWND(*mut c_void) は Send でないため isize 経由でスレッドへ受け渡す。
    // Why: トレイウィンドウのHWNDはメインスレッドが所有するが、別スレッドからの
    //   PostMessageW(スレッドセーフ)にだけ使うため受け渡しは安全。Send 制約を満たすため
    //   isize へ変換してからキャプチャし、クロージャ内で HWND へ戻す(生ポインタ横流しの定石)。
    let hwnd_raw = hwnd as isize;
    std::thread::spawn(move || unsafe {
        let outcome = fetch_latest_outcome();
        // ガードは結果にかかわらず解除(次回の確認を許可)
        CHECKING.store(false, Ordering::SeqCst);
        let result = Box::new(CheckResult { trigger, outcome });
        let raw = Box::into_raw(result) as isize;
        // Why: PostMessage 失敗時(キュー満杯等、極めて稀)は受け手が居ないため
        //   ここで解放してメモリリークを防ぐ。
        if PostMessageW(hwnd_raw as HWND, crate::WM_APP_UPDATE_RESULT, 0, raw) == 0 {
            // 受け手が無いので Box を破棄
            let _ = Box::from_raw(raw as *mut CheckResult);
        }
    });
}

/// WinHTTP で GitHub Releases API を GET し、最新版と比較した結果を返す。
unsafe fn fetch_latest_outcome() -> Outcome {
    let mut outcome = Outcome::Failed;
    let session = WinHttpOpen(
        wide(USER_AGENT).as_ptr(),
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
        core::ptr::null(),
        core::ptr::null(),
        0,
    );
    if !session.is_null() {
        WinHttpSetTimeouts(session, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);
        let connect = WinHttpConnect(
            session,
            wide(API_HOST).as_ptr(),
            INTERNET_DEFAULT_HTTPS_PORT as u16,
            0,
        );
        if !connect.is_null() {
            let request = WinHttpOpenRequest(
                connect,
                wide("GET").as_ptr(),
                wide(API_PATH).as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                WINHTTP_FLAG_SECURE,
            );
            if !request.is_null() {
                outcome = send_and_read(request);
                WinHttpCloseHandle(request);
            }
            WinHttpCloseHandle(connect);
        }
        WinHttpCloseHandle(session);
    }
    outcome
}

/// 要求を送信し、応答ステータスと本文を検査する。
unsafe fn send_and_read(request: *mut core::ffi::c_void) -> Outcome {
    // GitHub API 必須ヘッダ。User-Agent を明示し、Accept で JSON を要求する。
    let headers = wide("User-Agent: alt-ime-rs\r\nAccept: application/vnd.github+json\r\n");
    // 第3引数に u32::MAX を渡すと null終端文字列として長さ自動計算(WinHTTP 仕様)。
    WinHttpAddRequestHeaders(
        request,
        headers.as_ptr(),
        u32::MAX,
        WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
    );
    if WinHttpSendRequest(request, core::ptr::null(), 0, core::ptr::null(), 0, 0, 0) == 0 {
        return Outcome::Failed;
    }
    if WinHttpReceiveResponse(request, core::ptr::null_mut()) == 0 {
        return Outcome::Failed;
    }
    // HTTP 200 以外は失敗扱い(レート制限 403/429 等もここで除外)
    if !status_is_ok(request) {
        return Outcome::Failed;
    }
    let Some(body) = read_body(request) else {
        return Outcome::Failed;
    };
    parse_and_compare(&body)
}

/// ステータスコードが 200(OK) かを数値で問い合わせる。
unsafe fn status_is_ok(request: *mut core::ffi::c_void) -> bool {
    let mut status: u32 = 0;
    let mut len: u32 = core::mem::size_of::<u32>() as u32;
    let ok = WinHttpQueryHeaders(
        request,
        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
        core::ptr::null(),
        &mut status as *mut u32 as *mut core::ffi::c_void,
        &mut len,
        core::ptr::null_mut(),
    );
    ok != 0 && status == 200
}

/// 応答本文を全て読み込んで UTF-8 文字列で返す。上限を超えたら打ち切る。
unsafe fn read_body(request: *mut core::ffi::c_void) -> Option<String> {
    let mut body: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let mut avail: u32 = 0;
        if WinHttpQueryDataAvailable(request, &mut avail) == 0 {
            return None;
        }
        if avail == 0 {
            break; // 読み込み完了
        }
        let to_read = core::cmp::min(avail as usize, buf.len());
        let mut read: u32 = 0;
        if WinHttpReadData(
            request,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            to_read as u32,
            &mut read,
        ) == 0
        {
            return None;
        }
        if read == 0 {
            break;
        }
        // 念のため要求バイト数でクリップ(WinHTTP が read<=to_read を保証するが防御的に)
        let n = core::cmp::min(read as usize, buf.len());
        body.extend_from_slice(&buf[..n]);
        if body.len() >= MAX_BODY_BYTES {
            break;
        }
    }
    String::from_utf8(body).ok()
}

/// JSON 本文(Releases一覧)から移行後のSemVerタグを抽出し、現在版と比較する。
/// JSON パーサを依存に加えず、既知の単一フィールドを素朴に走査して取り出す。
fn parse_and_compare(body: &str) -> Outcome {
    let Some(tag) = latest_semver_tag(body) else {
        return Outcome::Failed;
    };
    if version_newer(&tag, APP_VERSION) {
        Outcome::UpdateAvailable(tag)
    } else {
        Outcome::UpToDate(tag)
    }
}

/// 本文に現れる全 tag_name のうち、SemVer形式(メジャー3桁以内)で最大のものを返す。
/// Why 最大を選ぶ: 一覧は作成降順だがsentinelのような非SemVerタグが先頭に来るため、
///   位置ではなく値で最新を決める。
/// Why メジャー3桁制限: 旧CalVer(v2026.*)とsentinel(v9999.*)を1つの条件で除外するため。
fn latest_semver_tag(body: &str) -> Option<String> {
    const KEY: &str = "\"tag_name\"";
    let mut best: Option<(Vec<u32>, String)> = None;
    let mut rest = body;
    while let Some(pos) = rest.find(KEY) {
        rest = &rest[pos + KEY.len()..];
        if let Some((parts, tag)) =
            parse_tag_value(rest).and_then(|t| parse_version(&t).map(|p| (p, t)))
        {
            let newer = best.as_ref().is_none_or(|(b, _)| parts > *b);
            if newer {
                best = Some((parts, tag));
            }
        }
    }
    best.map(|(_, tag)| tag)
}

/// `"tag_name":"vXXXX"` の値部分を抽出する。失敗時は None。
/// Why panic-free: ? 伝播と境界チェック済みのスライスのみ使用。ワーカースレッドは
///   panic = "abort" でプロセスを道連れにするため、ここでパニックしてはならない。
fn parse_tag_value(after: &str) -> Option<String> {
    // ':' を挟み、空白を読み飛ばして値の開始ダブルクォートを探す
    let rest = after
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// タグ("v1.2.3")を数値列へパースする。3部構成でない・メジャー3桁超は None。
/// Why v接頭辞は省略可: 現在版APP_VERSIONはCARGO_PKG_VERSION(="1.0.0"等、v無し)から
///   焼き込まれるため、v無しでもパースできないと更新検知が常に不成立になる。
fn parse_version(tag: &str) -> Option<Vec<u32>> {
    let body = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<u32> = body
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()?;
    if parts.len() != 3 || parts[0] > 999 {
        return None;
    }
    Some(parts)
}

/// latest が current より新しいか。ドット区切りを数値として比較する。
/// Why 辞書順不可: SemVerでは "v1.10.0" > "v1.9.0" が辞書順では逆転するため。
fn version_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // sentinel(v9999.*)と旧CalVer(v2026.*)を除外し、SemVerタグの最大値を選ぶ
    #[test]
    fn picks_max_semver_tag() {
        let body = r#"[{"tag_name":"v9999.99.99"},{"tag_name":"v1.9.0"},{"tag_name":"v2026.09.30.1"},{"tag_name":"v1.10.0"}]"#;
        assert_eq!(latest_semver_tag(body).as_deref(), Some("v1.10.0"));
    }

    // SemVerタグが1つも無いときはNone(=比較失敗扱い)
    #[test]
    fn no_semver_tag_is_none() {
        let body = r#"[{"tag_name":"v9999.99.99"},{"tag_name":"v2026.09.30.1"}]"#;
        assert_eq!(latest_semver_tag(body), None);
    }

    // 実APIの応答形式(コロン後にスペースあり)でも抽出できる
    #[test]
    fn parses_spaced_json_format() {
        let body = r#"[{ "tag_name": "v1.0.0", "draft": false }]"#;
        assert_eq!(latest_semver_tag(body).as_deref(), Some("v1.0.0"));
    }

    // 辞書順で逆転するケースを数値比較で正しく判定する
    #[test]
    fn compares_numerically() {
        assert!(version_newer("v1.10.0", "v1.9.0"));
        assert!(!version_newer("v1.9.0", "v1.10.0"));
        assert!(!version_newer("v1.0.0", "v1.0.0"));
        assert!(version_newer("v2.0.0", "v1.99.99"));
    }
    // 現在版が非SemVer(パース失敗)なら更新通知しない。
    // v無し("0.1.0"=CARGO_PKG_VERSION由来)は正当な現在版なのでパース対象。
    #[test]
    fn unparseable_current_is_not_newer() {
        assert!(version_newer("v1.0.0", "0.1.0"));
        assert!(!version_newer("v1.0.0", "dev"));
    }
}
