//! 同時押し中のF13注入をスキップすべき「XAMLメニューバー系アプリ」の判定。
//!
//! Why この判定が必要か: 「Alt同時押しでIME切替しない」有効時、同時押しへのF13注入の要否は
//! アプリ種別で逆向きになる(実機計測に基づく)。
//! - 古典Win32系(Officeデスクトップのリボン等): 素の同時押しでアクセスキー(KeyTips)が
//!   活性化してしまう → KeyUp時のF13注入で打ち消す必要がある。
//! - XAML(WinUI)メニューバー系(Win11のメモ帳等): 素の同時押しは一瞬のメニューバー聚焦で
//!   完結するが、Alt押下中にF13が届くと「Alt+キー」のメニューアクセスと解釈されて
//!   メニューバーが停滞聚焦してしまう → F13を注入せず素の挙動に任せる方が良い。
//!
//! 判定はフォアグラウンドのルートウィンドウクラス(terminal.rsと同一手法)。
//! 対象アプリを追加したい場合: 当該アプリを前面に出してPowerShellのGetClassName等で
//! クラス名を確認し、リストへ追記する。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;

use crate::terminal::{eq_ignore_case, focus_root};

// XAMLメニューバー系アプリのトップレベルウィンドウクラス(大文字小文字は無視して比較)。
// Constraint: Win11メモ帳はXAMLアイランド構成だがトップレベルクラスは"Notepad"(実測)。
//   動作保証はwin11のみ(README要件)のため、Win10の古典メモ帳との同名衝突は考慮しない。
const XAML_MENU_CLASSES: &[&str] = &["Notepad"];

/// フォーカスウィンドウがXAMLメニューバー系アプリならtrue。
/// 呼び出しは同時押しのKeyUp時(ジェスチャ単位)のみで、キー入力のたびに走るわけではない。
pub fn focused_is_xaml_menu_app() -> bool {
    unsafe { is_xaml_menu_app(focus_root()) }
}

/// ルートウィンドウのクラス名がXAMLメニューバー系リストと一致するか。
/// Why 判定不能時はfalse: F13注入(古典Win32系の抑制)を既定動作として維持する方を安全側とする。
unsafe fn is_xaml_menu_app(root: HWND) -> bool {
    if root.is_null() {
        return false;
    }
    let mut class = [0u16; 64];
    let len = GetClassNameW(root, class.as_mut_ptr(), class.len() as i32);
    len > 0 && matches_class(&class[..len as usize])
}

/// UTF-16のクラス名がリストと一致するか(テストで直接叩くため分離)。
fn matches_class(class: &[u16]) -> bool {
    XAML_MENU_CLASSES.iter().any(|c| eq_ignore_case(c, class))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Win11メモ帳のクラス名は一致、Excel(XLMAIN)等は不一致であること。
    #[test]
    fn class_matching() {
        assert!(matches_class(&w("Notepad")), "Win11メモ帳のクラスに一致");
        assert!(matches_class(&w("notepad")), "大文字小文字は無視される");
        assert!(!matches_class(&w("XLMAIN")), "Excelは不一致");
        assert!(!matches_class(&w("Notepad2")), "前方一致はしない");
        assert!(!matches_class(&w("")), "空は不一致");
    }
}
