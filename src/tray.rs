//! タスクトレイ常駐とコンテキストメニュー。
//!
//! 不可視のメッセージ専用ウィンドウを作成し、Shell_NotifyIconW で
//! トレイアイコンを登録する。右/左クリックでポップアップメニューを表示し、
//! 「自動起動」の切替と「終了」を提供する。

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadIconW, PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    TrackPopupMenu, HWND_MESSAGE, IDI_APPLICATION, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    MF_UNCHECKED, TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSEXW,
};

use crate::{hook, startup, wide};

// カスタムメッセージ(トレイアイコンのコールバック)
const WM_APP: u32 = 0x8000;
const WM_TRAYICON: u32 = WM_APP + 1;
// コンテキストメニューの項目ID
const IDM_AUTOSTART: usize = 1001;
const IDM_EXIT: usize = 1002;

/// トレイウィンドウを作成し、アイコンを登録する。失敗時はNone。
pub unsafe fn create() -> Option<HWND> {
    let hinst = GetModuleHandleW(core::ptr::null());
    let class_name = wide("AltImeRsTray");

    let mut wc: WNDCLASSEXW = core::mem::zeroed();
    wc.cbSize = core::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.lpfnWndProc = Some(wnd_proc);
    wc.hInstance = hinst;
    wc.lpszClassName = class_name.as_ptr();
    if RegisterClassExW(&wc) == 0 {
        return None;
    }

    let window_name = wide("alt-ime-rs");
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        window_name.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        // Constraint: HWND_MESSAGE を指定してメッセージ専用ウィンドウにする。タスクバー等に表示されず、トレイのコールバック受信のみに使える。
        HWND_MESSAGE,
        core::ptr::null_mut(),
        hinst,
        core::ptr::null_mut(),
    );
    if hwnd.is_null() {
        return None;
    }

    // トレイアイコンを追加(標準アプリケーションアイコンを使用)
    let icon = LoadIconW(core::ptr::null_mut(), IDI_APPLICATION);
    let mut nid: NOTIFYICONDATAW = core::mem::zeroed();
    nid.cbSize = core::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = icon;
    set_tip(&mut nid.szTip, "alt-ime-rs");
    if Shell_NotifyIconW(NIM_ADD, &mut nid) == 0 {
        DestroyWindow(hwnd);
        return None;
    }
    Some(hwnd)
}

/// トレイアイコンを削除し、ウィンドウを破棄する。
pub unsafe fn destroy(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = core::mem::zeroed();
    nid.cbSize = core::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &mut nid);
    DestroyWindow(hwnd);
}

/// メッセージウィンドウのプロシージャ。
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    match msg {
        // フックコールバックからの非同期要求 → vk07 でメニュー抑制を注入
        // Why: フックコールバック内で直接 SendInput すると vk07 が Alt 伝播より先に処理される。PostMessageW でメッセージキューへ非同期積みし、このハンドラをメッセージループ経由で呼ぶことで、CallNextHookEx(Alt伝播)の後に vk07 が注入される順序を保証する(詳細は hook.rs のモジュールdoc)。
        crate::WM_APP_SUPPRESS => {
            hook::suppress_menu();
            0
        }
        WM_TRAYICON => {
            // lParam の下位ワードがマウスメッセージ
            let mouse = (lparam & 0xFFFF) as u32;
            if mouse == WM_RBUTTONUP || mouse == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            match id {
                IDM_AUTOSTART => {
                    toggle_autostart();
                    0
                }
                IDM_EXIT => {
                    DestroyWindow(hwnd);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// コンテキストメニューを表示する。
unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let autostart_flag = if startup::is_enabled() {
        MF_CHECKED
    } else {
        MF_UNCHECKED
    };
    AppendMenuW(
        menu,
        MF_STRING | autostart_flag,
        IDM_AUTOSTART,
        wide("自動起動").as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, core::ptr::null());
    AppendMenuW(menu, MF_STRING, IDM_EXIT, wide("終了").as_ptr());

    let mut pt: POINT = core::mem::zeroed();
    GetCursorPos(&mut pt);
    // メニューを確実に閉じるための標準イディオム
    // Note: TrackPopupMenu のMSDN仕様。呼出前に所有者をフォアグラウンド化し、直後にダミーメッセージを送らないとメニュー外クリックで閉じなくなる。
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        core::ptr::null_mut(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

/// 自動起動の有効/無効を切替える。
unsafe fn toggle_autostart() {
    if startup::is_enabled() {
        startup::disable();
    } else {
        startup::enable();
    }
}

/// NOTIFYICONDATAW.szTip (UTF-16配列) にチップ文字列を設定する。
/// 最後の1要素は null終端用に予約し、文字列長に関わらず null終端を保証する。
unsafe fn set_tip(tip: &mut [u16], s: &str) {
    let max = tip.len() - 1;
    for (i, c) in s.encode_utf16().enumerate() {
        if i >= max {
            break;
        }
        tip[i] = c;
    }
    // tip は zeroed 初期化されているため、未設定部分(末尾含む)は 0 のままで null終端される
}
