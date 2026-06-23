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

use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, MSG};

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
