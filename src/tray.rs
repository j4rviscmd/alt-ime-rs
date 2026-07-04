//! タスクトレイ常駐とコンテキストメニュー。
//!
//! 不可視のメッセージ専用ウィンドウを作成し、Shell_NotifyIconW で
//! トレイアイコンを登録する。右/左クリックでポップアップメニューを表示し、
//! 「自動起動」の切替、「実行ファイルの場所を開く」、「アップデートの確認」、「終了」を提供する。

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, LoadIconW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetForegroundWindow, TrackPopupMenu, HWND_MESSAGE, IDYES, MB_ICONINFORMATION, MB_ICONWARNING,
    MB_OK, MB_YESNO, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, SW_SHOWNORMAL,
    TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
};

use crate::{hook, ime, startup, update, wide};

// カスタムメッセージ(トレイアイコンのコールバック)
const WM_APP: u32 = 0x8000;
const WM_TRAYICON: u32 = WM_APP + 1;
// コンテキストメニューの項目ID
const IDM_AUTOSTART: usize = 1001;
const IDM_EXIT: usize = 1002;
const IDM_OPEN_LOCATION: usize = 1003;
// Constraint: IDM_OPEN_LOCATION(1003) と衝突しない値。1004 を割り当てる。
const IDM_CHECKUPDATE: usize = 1004;

/// windows_sys 0.59 が公開していない MAKEINTRESOURCEW マクロ相当。
/// 整数リソースID を名前ではなく番号として解釈させるため、整数を LPCWSTR へキャストする。
const fn makeintresource(w: u16) -> *const u16 {
    w as usize as *const u16
}

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

    // トレイアイコンを追加(exe に埋め込んだアイコンリソース ID=1 を使用)
    let icon = LoadIconW(hinst, makeintresource(1));
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
        // フックコールバックからの非同期要求 → IME 切替(ime::set_on)
        // Why: フックコールバック内で ime::set_on を呼ぶと SendMessageW(WM_IME_CONTROL) が IME 側スレッドの
        //   応答を待ってメインスレッドをブロックし、Alt KeyDown 時に投稿済みの WM_APP_SUPPRESS(vk07注入)が
        //   処理されず Alt KeyUp 伝播後にずれ込むため、PostMessageW でメッセージキューへ非同期積みし、この
        //   ハンドラをメッセージループ経由で呼ぶことでブロック影響を LL コールバック外へ局所化する(詳細は hook.rs のモジュールdoc)。
        crate::WM_APP_IME_TOGGLE => {
            // wParam: 0=IME OFF, 1=IME ON
            let on = wparam != 0;
            ime::set_on(on);
            0
        }
        // アップデート確認スレッドからの結果受領 → トリガ/結果に応じてダイアログ表示
        // Why: 通信は別スレッドで行い、MessageBox はメインスレッドで出す。PostMessage の
        //   lparam に Box<CheckResult> の生ポインタを載せて受け渡す(同パターンは WM_APP_* 系)。
        crate::WM_APP_UPDATE_RESULT => {
            handle_update_result(lparam);
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
                IDM_OPEN_LOCATION => {
                    open_file_location();
                    0
                }
                IDM_CHECKUPDATE => {
                    // 別スレッドでGitHub APIへ問い合わせ(メインスレッドをブロックしない)
                    update::check_async(hwnd, update::Trigger::Manual);
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
    AppendMenuW(
        menu,
        MF_STRING,
        IDM_OPEN_LOCATION,
        wide("実行ファイルの場所を開く").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING,
        IDM_CHECKUPDATE,
        wide("アップデートを確認する").as_ptr(),
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

/// アップデート確認の結果(lparam に Box<CheckResult> の生ポインタ)を受け取り、
/// トリガと結果に応じたダイアログを表示する。
unsafe fn handle_update_result(lparam: isize) {
    if lparam == 0 {
        return;
    }
    // 生ポインタから Box を再構築し、所有権を受け取って解放する
    let update::CheckResult { trigger, outcome } =
        *Box::from_raw(lparam as *mut update::CheckResult);
    show_update_dialog(trigger, outcome);
}

/// トリガと結果に応じてダイアログを表示する。
unsafe fn show_update_dialog(trigger: update::Trigger, outcome: update::Outcome) {
    // 起動時(Startup)は「更新あり」の時だけ表示。最新時・失敗時は静かに抜ける。
    // Why: 起動のたびにMessageBoxが出るのはユーザ体験を損ねるため、必要最小限(更新あり)に抑える。
    let silent_unless_update = matches!(trigger, update::Trigger::Startup);
    match outcome {
        update::Outcome::UpdateAvailable(latest) => {
            let msg = format!(
                "新しいバージョンがあります。\n\n現在: v{}\n最新: {}\n\n配布ページを開きますか？",
                update::APP_VERSION,
                latest
            );
            let rc = MessageBoxW(
                core::ptr::null_mut(),
                wide(&msg).as_ptr(),
                wide("アップデート").as_ptr(),
                MB_YESNO | MB_ICONINFORMATION,
            );
            if rc == IDYES as i32 {
                open_releases_page();
            }
        }
        update::Outcome::UpToDate(latest) => {
            if !silent_unless_update {
                let msg = format!("最新のバージョンを使用しています。\n({})", latest);
                MessageBoxW(
                    core::ptr::null_mut(),
                    wide(&msg).as_ptr(),
                    wide("アップデート").as_ptr(),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
        }
        update::Outcome::Failed => {
            if !silent_unless_update {
                let rc = MessageBoxW(
                    core::ptr::null_mut(),
                    wide("アップデートの確認に失敗しました。\nネットワーク環境を確認し、配布ページを開きますか？")
                        .as_ptr(),
                    wide("アップデート").as_ptr(),
                    MB_YESNO | MB_ICONWARNING,
                );
                if rc == IDYES as i32 {
                    open_releases_page();
                }
            }
        }
    }
}

/// 既定ブラウザで配布ページ(Releases/latest)を開く。
unsafe fn open_releases_page() {
    // Why: DL・実行はユーザ任せ(実行中exeの置換問題を避け、ブラウザで案内するだけに留める)。
    let _ = ShellExecuteW(
        core::ptr::null_mut(),
        wide("open").as_ptr(),
        wide(update::RELEASES_URL).as_ptr(),
        core::ptr::null(),
        core::ptr::null(),
        SW_SHOWNORMAL,
    );
}

/// 実行ファイルの場所をエクスプローラで開く(exe を選択状態にする)。
unsafe fn open_file_location() {
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return,
    };
    // Constraint: explorer.exe の /select,"パス" 構文。引用符でパス全体を囲み、空白・特殊文字を含むパスでも1つの引数として解釈させる。
    let params = format!("/select,\"{}\"", exe);
    ShellExecuteW(
        core::ptr::null_mut(),
        wide("open").as_ptr(),
        wide("explorer.exe").as_ptr(),
        wide(&params).as_ptr(),
        core::ptr::null(),
        SW_SHOWNORMAL,
    );
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
