//! 低レベルキーボードフック(WH_KEYBOARD_LL)による空打ち判定とIME切替。
//!
//! 仕組み:
//! - 左/右AltのKeyDownで対応する空打ちフラグをtrueにする。
//! - Alt以外のキーのKeyDownで両方のフラグをfalseにする(複合キー除外)。
//! - AltのKeyUpでフラグがtrueのまま(=空打ち)ならIMEを切替える。
//!
//! また、Alt押下でメニューバーがアクティブになる問題を、参照元(AHK)と同様に
//! 未割当キー(0x07)の入力を注入してキャンセルする。
//!
//! Why vk07注入を非同期化するか:
//! - フックコールバック内で同期的に SendInput すると、注入した vk07 が
//!   CallNextHookEx(Alt伝播) より先に処理され、"Alt→別キー" のキャンセル条件を
//!   満たさずメニュー抑制が効かない。
//! - 参照元 AHK はホットキー機構で非同期に Send するためこの問題が起きない。
//! - そこでコールバックからは PostMessageW でトレイウィンドウへ要求だけ投げ、
//!   メッセージループ側で vk07 を注入する。これで Alt 伝播後に vk07 が届く。

use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::ime;

// 仮想キーコード(左右Alt)。LLフックの vkCode で直接取得できる。
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
// メニュー抑制用の未割当キー(AHKのvk07相当)。
// SendInputの再帰フックを無視するため特別扱いする。
// Constraint: 0x07 はWindows仮想キーコード表の未定義領域。アプリへ入力として伝わらずメニュー活性化だけ打ち消せるため参照元(karakaram/alt-ime-ahk)と同一選択。
const VK_SUPPRESS: u32 = 0x07;

// 空打ち判定状態。Alt押下時にtrue、押下中に別キーが打たれたらfalse。
static LALT_CLEAN: AtomicBool = AtomicBool::new(false);
static RALT_CLEAN: AtomicBool = AtomicBool::new(false);
// フックハンドル(解除用)。プロシージャと同じスレッドで触る。
static HHOOK: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());
// vk07 抑制注入の PostMessage 先。main がトレイウィンドウ生成後に登録する。
static TRAY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());

/// キーボードフックをインストールする。失敗時はfalse。
pub unsafe fn install() -> bool {
    let hinst = GetModuleHandleW(core::ptr::null());
    let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_proc), hinst, 0);
    if hook.is_null() {
        return false;
    }
    HHOOK.store(hook, Ordering::SeqCst);
    true
}

/// キーボードフックを解除する。
pub unsafe fn uninstall() {
    let hook = HHOOK.swap(core::ptr::null_mut(), Ordering::SeqCst);
    if !hook.is_null() {
        UnhookWindowsHookEx(hook);
    }
}

/// フックコールバックから vk07 抑制注入を依頼するトレイウィンドウを登録する。
pub unsafe fn set_tray_hwnd(hwnd: HWND) {
    TRAY_HWND.store(hwnd, Ordering::SeqCst);
}

/// 低レベルキーボードフックのプロシージャ。
unsafe extern "system" fn low_level_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);

        // 抑制用キー自身は無視(SendInputの再帰呼出で空打ち判定を壊さないため)
        // Why: vk07 を SendInput で注入すると本フックへ再帰的に流入する。ここを通過させると「Alt以外のキー」扱いされて空打ちフラグが即falseに落ち、IME切替が発火しなくなるため除外が必要(commit f7bc0c8)。
        if kb.vkCode != VK_SUPPRESS {
            // Constraint: Alt押下中はシステムが WM_SYSKEYDOWN/UP を発行する。WM_KEY* 系だけだと Alt 単独打鍵を取りこぼすため、両系統を OR で網羅する必要がある(MSDN WM_SYSKEYDOWN 仕様)。
            let down = wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize;
            let up = wparam == WM_KEYUP as usize || wparam == WM_SYSKEYUP as usize;

            match kb.vkCode {
                VK_LMENU => {
                    if down {
                        LALT_CLEAN.store(true, Ordering::SeqCst);
                        // Why: フックコールバック内で同期的 SendInput すると vk07 が CallNextHookEx(Alt伝播) より先に処理されメニュー抑制が効かなくなるため、PostMessage で非同期化する(本セッション変更、参照元 karakaram/alt-ime-ahk)。
                        request_suppress();
                    } else if up && LALT_CLEAN.swap(false, Ordering::SeqCst) {
                        // Why: 読み出しと同時にフラグを落とすことで、KeyUp〜IME切替の間に別キーが割り込んでも二重切替を防ぐ。単純な load+store だと判定後に状態が変わり得るため swap 一発でatomicに処理する。
                        // 左Alt空打ち → IME OFF
                        ime::set_on(false);
                    }
                }
                VK_RMENU => {
                    if down {
                        RALT_CLEAN.store(true, Ordering::SeqCst);
                        request_suppress();
                    } else if up && RALT_CLEAN.swap(false, Ordering::SeqCst) {
                        // Why: 上記 LALT_CLEAN と同様、読み出しとリセットを swap 一発でatomicに行い二重切替を防ぐ。
                        // 右Alt空打ち → IME ON
                        ime::set_on(true);
                    }
                }
                _ => {
                    // Alt以外のキー押下 → 両Altの空打ちを取り消す(複合キー除外)
                    if down {
                        LALT_CLEAN.store(false, Ordering::SeqCst);
                        RALT_CLEAN.store(false, Ordering::SeqCst);
                    }
                }
            }
        }
    }
    // Constraint: LLフックはイベントを握りつぶさず必ず次へ渡す必要がある。非0を返すと当該キーが破棄され、全キーが本フックを通るため結果的にキーボード入力が効かなくなる(MSDN LowLevelKeyboardProc 仕様)。
    CallNextHookEx(core::ptr::null_mut(), code, wparam, lparam)
}

/// vk07 抑制注入をメッセージループ側へ非同期で依頼する。
/// Why: トレイウィンドウへ PostMessage することで、本コールバックが戻って
///   CallNextHookEx(Alt伝播) が処理された後に vk07 が注入される順序を保証する。
unsafe fn request_suppress() {
    let hwnd = TRAY_HWND.load(Ordering::SeqCst);
    // Why: 起動直後など tray::create 完了前は HWND 未登録で null になる。この間は抑制を諦め、登録後の Alt 押下から有効化する(初回数発の抑制漏れは許容)。
    if !hwnd.is_null() {
        // Why: 失敗(キュー満杯等)時でもフックコールバック内でリトライ/ブロックは禁止されるため諦める。抑制漏れはメニューが一時的に出る程度の影響に留まる。
        let _ = PostMessageW(hwnd, crate::WM_APP_SUPPRESS, 0, 0);
    }
}

/// Alt押下でメニューバーがアクティブになるのを防ぐため、未割当キーを注入する。
/// トレイウィンドウのメッセージ処理(メインスレッド)から呼ばれる。
pub(crate) unsafe fn suppress_menu() {
    // Why: メニュー活性化を確実にキャンセルするには押下/解放の完全な対が必要。downのみ残すと未解放状態が残るためupまで注入。
    let mut inputs: [INPUT; 2] = [core::mem::zeroed(); 2];
    inputs[0].r#type = INPUT_KEYBOARD;
    inputs[0].Anonymous.ki.wVk = VK_SUPPRESS as u16;
    inputs[1].r#type = INPUT_KEYBOARD;
    inputs[1].Anonymous.ki.wVk = VK_SUPPRESS as u16;
    inputs[1].Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
    // Why: 戻り値(挿入数)は失敗時でもリカバリ手段が無いため無視する。
    let _ = SendInput(2, inputs.as_ptr(), core::mem::size_of::<INPUT>() as i32);
}
