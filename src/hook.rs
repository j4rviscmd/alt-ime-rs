//! 低レベルキーボードフック(WH_KEYBOARD_LL)による空打ち判定とIME切替。
//!
//! 仕組み:
//! - 左/右AltのKeyDownで対応する空打ちフラグをtrueにする。
//! - Alt以外のキーのKeyDownで両方のフラグをfalseにする(複合キー除外)。
//! - AltのKeyUpでフラグがtrueのまま(=空打ち)ならIMEを切替える。
//!
//! また、Alt押下でメニューバーがアクティブになる問題を、参照元(AHK)と同様に
//! 未割当キー(0x07)の入力を注入してキャンセルする。

use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
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

/// 低レベルキーボードフックのプロシージャ。
unsafe extern "system" fn low_level_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    if code >= 0 {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);

        // 抑制用キー自身は無視(SendInputの再帰呼出で空打ち判定を壊さないため)
        if kb.vkCode != VK_SUPPRESS {
            let down = wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize;
            let up = wparam == WM_KEYUP as usize || wparam == WM_SYSKEYUP as usize;

            match kb.vkCode {
                VK_LMENU => {
                    if down {
                        LALT_CLEAN.store(true, Ordering::SeqCst);
                        suppress_menu();
                    } else if up && LALT_CLEAN.swap(false, Ordering::SeqCst) {
                        // 左Alt空打ち → IME OFF
                        ime::set_on(false);
                    }
                }
                VK_RMENU => {
                    if down {
                        RALT_CLEAN.store(true, Ordering::SeqCst);
                        suppress_menu();
                    } else if up && RALT_CLEAN.swap(false, Ordering::SeqCst) {
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
    CallNextHookEx(core::ptr::null_mut(), code, wparam, lparam)
}

/// Alt押下でメニューバーがアクティブになるのを防ぐため、未割当キーを注入する。
unsafe fn suppress_menu() {
    // Why: メニュー活性化を確実にキャンセルするには押下/解放の完全な対が必要。downのみ残すと未解放状態が残るためupまで注入。
    let mut inputs: [INPUT; 2] = [core::mem::zeroed(); 2];
    inputs[0].r#type = INPUT_KEYBOARD;
    inputs[0].Anonymous.ki.wVk = VK_SUPPRESS as u16;
    inputs[1].r#type = INPUT_KEYBOARD;
    inputs[1].Anonymous.ki.wVk = VK_SUPPRESS as u16;
    inputs[1].Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(2, inputs.as_ptr(), core::mem::size_of::<INPUT>() as i32);
}
