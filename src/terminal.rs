//! F13注入をスキップする「端末ウィンドウ」の判定。
//!
//! Why 端末を除外するか: F13注入はWin32メニューバー活性化の抑制とWeb版ExcelのKeyTips抑制が
//! 目的で、メニューバーを持たない端末では利益がゼロ。一方、端末はF13をエスケープシーケンスと
//! してシェルへ転送するため、nvim等で<F13>が入力されてしまう。そこでフォーカスが端末のときは
//! F13注入だけをスキップする(IME切替の空打ち判定には影響しない)。
//!
//! 判定はウィンドウクラスとプロセス名(exeベース名)の2系統:
//! - クラス: ネイティブ実装の主要端末。クラスが安定しており第一優先。
//! - exe名: WezTermは--class起動オプションでクラスを上書きでき、Alacrittyはwinitの既定
//!   クラス"Window Class"が他のwinitアプリと衝突する。VSCode等のElectron製はクラスが
//!   Chrome系ブラウザ(Chrome_WidgetWin_1)と同一で、F13注入が必要なExcel webと区別できない。
//!   この3系統はexe名で判定する。
//!
//! 端末を追加したい場合: 当該アプリを前面に出してPowerShellやAutoHotkeyのWindow Spyで
//! クラス名/exe名を確認し、該当リストへ追記する。

use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
    GA_ROOT, GUITHREADINFO,
};

// 端末のトップレベルウィンドウクラス(大文字小文字は無視して比較)。
// Constraint: 出典はAutoHotkeyコミュニティの端末グループ定義等で実績のある値
//   (rcmdnk/windows の AutoHotkey.ahk、Stack Overflow の ahk_class 利用例)。
const TERMINAL_CLASSES: &[&str] = &[
    "CASCADIA_HOSTING_WINDOW_CLASS", // Windows Terminal(Previewも同一クラス)
    "ConsoleWindowClass",            // 従来コンソール(conhost)
    "PuTTY",
    "mintty",              // Git for Windows / Cygwin
    "VTWin32",             // Tera Term
    "VirtualConsoleClass", // ConEmu / Cmder
    "TMobaXtermForm",      // MobaXterm
];

// 端末のプロセス名(ベース名・大文字小文字は無視して比較)。
// Why exe名判定: WezTerm/Alacritty/Electron製はクラスが使えないため(モジュールdoc参照)。
const TERMINAL_EXES: &[&str] = &[
    "windowsterminal.exe",
    "wezterm-gui.exe",
    "alacritty.exe",
    // VSCode系: 内蔵端末へのF13転送を防ぐ。クラスがChromeと同一のためexe名判定が必須。
    "code.exe",
    "code - insiders.exe",
    "vscodium.exe",
    "hyper.exe",
];

/// フォーカスウィンドウが端末ならtrue。
/// 呼び出しはAlt押下ごと(hook::suppress_menu)で、キーボード入力のたびに走るわけではない。
pub fn focused_is_terminal() -> bool {
    unsafe { is_terminal(focus_root()) }
}

/// フォーカスのルート(トップレベル)ウィンドウを取得する。特定不能ならnull。
/// Why GA_ROOTか: F13の配送先は入力フォーカスを持つ子ウィンドウ(Windows Terminalでは
///   XAMLアイランド配下)で、クラス判定はトップレベルウィンドウで行う必要があるため。
unsafe fn focus_root() -> HWND {
    let fg = GetForegroundWindow();
    if fg.is_null() {
        return fg;
    }
    let thread_id = GetWindowThreadProcessId(fg, core::ptr::null_mut());
    let mut info: GUITHREADINFO = core::mem::zeroed();
    info.cbSize = core::mem::size_of::<GUITHREADINFO>() as u32;
    let focus = if GetGUIThreadInfo(thread_id, &mut info) != 0 && !info.hwndFocus.is_null() {
        info.hwndFocus
    } else {
        fg
    };
    let root = GetAncestor(focus, GA_ROOT);
    if root.is_null() {
        fg
    } else {
        root
    }
}

/// ルートウィンドウのクラス名または所属プロセスのexe名が端末リストと一致するか。
/// Why 判定不能時はfalse: F13注入は既定動作(メニュー抑制)を維持する方を安全側とする。
unsafe fn is_terminal(root: HWND) -> bool {
    if root.is_null() {
        return false;
    }
    let mut class = [0u16; 64];
    let len = GetClassNameW(root, class.as_mut_ptr(), class.len() as i32);
    if len > 0
        && TERMINAL_CLASSES
            .iter()
            .any(|c| eq_ignore_case(c, &class[..len as usize]))
    {
        return true;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(root, &mut pid);
    pid != 0 && process_is_terminal(pid)
}

/// プロセスのexeベース名(最後の'\'以降)が端末リストと一致するか。
/// Why ベース名比較か: フルパスはインストール位置で変わるため。
unsafe fn process_is_terminal(pid: u32) -> bool {
    // Why: PROCESS_QUERY_LIMITED_INFORMATION は昇格済み(高整合性)プロセスでもイメージ名照会目的なら
    //   開ける最小権限のアクセスマスク(MSDN OpenProcess の仕様)。PROCESS_QUERY_INFORMATION だと
    //   非昇格の本プロセスから管理者権限で起動した端末が開けず、判定だけ取りこぼすため。
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if process.is_null() {
        // Why: 保護されたプロセス等で開けない場合は判定不能とし既定動作(F13注入)を維持する。
        return false;
    }
    let mut path = [0u16; 512];
    let mut len = path.len() as u32;
    let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len);
    CloseHandle(process);
    if ok == 0 || len == 0 {
        return false;
    }
    let full = &path[..len as usize];
    let name = match full.iter().rposition(|&c| c == b'\\' as u16) {
        Some(i) => &full[i + 1..],
        None => full,
    };
    TERMINAL_EXES.iter().any(|e| eq_ignore_case(e, name))
}

/// ASCIIパターン(&str)とUTF-16文字列の大文字小文字無視比較。
/// Why 独自実装か: Stringへ変換するとAlt押下のたびにヒープ確保が走るため確保なしで済ませる。
///   リストはすべてASCII前提で、非ASCII文字(>0xFF)は不一致として扱う。
fn eq_ignore_case(pattern: &str, text: &[u16]) -> bool {
    let pat = pattern.as_bytes();
    if pat.len() != text.len() {
        return false;
    }
    pat.iter()
        .zip(text)
        .all(|(&p, &c)| u8::try_from(c).is_ok_and(|c| p.eq_ignore_ascii_case(&c)))
}
