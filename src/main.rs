//! alt-ime-rs: 左右Altキーの空打ちでIMEを切り替えるWindows常駐ツール。
//!
//! - 左Alt空打ち → IME OFF
//! - 右Alt空打ち → IME ON
//! - Alt押下中に別キーを打つ(複合キー) → IME切替しない
//! - メニューバーの活性化は抑制、タスクトレイに常駐する

// Why: ダブルクリック起動時にコンソールウィンドウを開かないため GUI サブシステムを指定する。
#![windows_subsystem = "windows"]

mod hook;
mod ime;
mod startup;
mod tray;
mod update;

use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG};

// フックコールバックからトレイウィンドウへ vk07 抑制注入を依頼するカスタムメッセージ。
// Constraint: tray.rs の WM_APP 系(0x8000/0x8001)と衝突しない値。0x8002 を割り当てる。
pub(crate) const WM_APP_SUPPRESS: u32 = 0x8002;

// フックコールバックからトレイウィンドウへ IME 切替を依頼するカスタムメッセージ。
// Why: LLフックコールバック内で ime::set_on を呼ぶと SendMessageW(WM_IME_CONTROL) が IME 側スレッドの
//   応答を待ってメインスレッドをブロックし、Alt KeyDown 時に投稿済みの WM_APP_SUPPRESS(vk07注入)が
//   処理されず Alt KeyUp 伝播後にずれ込むため、PostMessage でコールバック外へ追い出す。
// Constraint: WM_APP_SUPPRESS(0x8002) と衝突しない値。0x8003 を割り当てる。
pub(crate) const WM_APP_IME_TOGGLE: u32 = 0x8003;

// アップデート確認スレッドからトレイウィンドウへ結果を受け渡すカスタムメッセージ。
// Why: 通信は別スレッドで行い、UI(MessageBox)はメインスレッドで出すため、PostMessage で
//   結果をメインスレッドへ受け渡す(WM_APP_SUPPRESS / WM_APP_IME_TOGGLE と同じパターン)。
// Constraint: 既存の WM_APP 系(0x8000〜0x8003)と衝突しない値。0x8004 を割り当てる。
pub(crate) const WM_APP_UPDATE_RESULT: u32 = 0x8004;

fn main() {
    unsafe {
        // キーボードフックを設置
        if !hook::install() {
            eprintln!("エラー: キーボードフックの設置に失敗しました。");
            std::process::exit(1);
        }

        // トレイウィンドウを作成しアイコンを登録
        let Some(hwnd) = tray::create() else {
            eprintln!("エラー: トレイの初期化に失敗しました。");
            hook::uninstall();
            std::process::exit(1);
        };

        // フックコールバックが vk07 抑制注入を PostMessage する宛先を登録
        // Why: install() より後、かつメッセージループ開始より前で登録する。install 前だとフックはまだ来ず、ループ開始後だと初回 Alt 押下の抑制要求が null 宛になり取りこぼされるため、この順序が必須。
        hook::set_tray_hwnd(hwnd);

        // 起動時のアップデート確認を非同期で開始
        // Why: トレイウィンドウ生成後・メッセージループ直前に起動する。通信は別スレッドで
        //   メインスレッドをブロックしないため起動遅延なし。結果は PostMessage でキューへ
        //   積まれ、ループ開始後に WM_APP_UPDATE_RESULT として処理される(hwnd 有効なら即Post可能)。
        update::check_async(hwnd, update::Trigger::Startup);

        // メッセージループ(WH_KEYBOARD_LL はメッセージポンプが必須)
        // Constraint: LLフックのコールバックはフック設置スレッドのメッセージキューへ配送される。そのため設置したスレッド(=メイン)でポンプを回し続ける必要がある。
        let mut msg: MSG = core::mem::zeroed();
        while GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }

        // 終了処理
        tray::destroy(hwnd);
        hook::uninstall();
    }
}

/// UTF-16 のワイド文字列(null終端)を生成する。各モジュールから利用。
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
