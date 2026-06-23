//! IMEのON/OFF状態の取得・設定。
//!
//! 参照元(karakaram/alt-ime-ahk)と同一手法を採用:
//!   GetGUIThreadInfo でフォーカスを持つウィンドウを特定し、
//!   ImmGetDefaultIMEWnd でデフォルトIMEウィンドウを取得、
//!   WM_IME_CONTROL メッセージで状態を取得・設定する。
//!
//! 補足: IMC_GETOPENSTATUS / IMC_SETOPENSTATUS は imm.h で定義されているが
//! windows-sys には定数として公開されていないため本ファイルで定義する。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::Ime::ImmGetDefaultIMEWnd;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, SendMessageW, GUITHREADINFO,
    WM_IME_CONTROL,
};

// WM_IME_CONTROL のコマンド値(imm.h 準拠)。windows-sys 非公開のため手動定義。
// Constraint: imm.h のマクロ値をそのまま転記。windows-sys 0.59 がこれらを公開していないため値がずれるとIME制御が壊れる。
const IMC_GETOPENSTATUS: usize = 0x0005;
const IMC_SETOPENSTATUS: usize = 0x0006;

/// IMEのON/OFFを設定する。
/// 現在の状態と同じ場合は何もしない(要件: すでにその状態なら何もしない)。
pub unsafe fn set_on(on: bool) {
    let ime_wnd = default_ime_window();
    if ime_wnd.is_null() {
        return;
    }
    // 既に目標状態ならスキップ
    let current = SendMessageW(ime_wnd, WM_IME_CONTROL, IMC_GETOPENSTATUS, 0);
    // Constraint: IMC_SETOPENSTATUS の lParam は imm.h 仕様で BOOL(0/1)。true/false を 1/0 に写す必要がある。
    let target: isize = if on { 1 } else { 0 };
    if current == target {
        return;
    }
    SendMessageW(ime_wnd, WM_IME_CONTROL, IMC_SETOPENSTATUS, target);
}

/// フォアグラウンドのフォーカスウィンドウに対応するデフォルトIMEウィンドウを取得する。
/// フォーカスの取得に失敗した場合はフォアグラウンドウィンドウ自体を使用する。
unsafe fn default_ime_window() -> HWND {
    let fg = GetForegroundWindow();
    if fg.is_null() {
        return core::ptr::null_mut();
    }
    let thread_id = GetWindowThreadProcessId(fg, core::ptr::null_mut());

    let mut info: GUITHREADINFO = core::mem::zeroed();
    info.cbSize = core::mem::size_of::<GUITHREADINFO>() as u32;
    let focus = if GetGUIThreadInfo(thread_id, &mut info) != 0 {
        info.hwndFocus
    } else {
        fg
    };
    // Why: コンソール等では hwndFocus が空になることがある。フォアグラウンドウィンドウへフォールバックしないとIME状態を取得できない。
    ImmGetDefaultIMEWnd(if focus.is_null() { fg } else { focus })
}
