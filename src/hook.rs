//! 低レベルキーボードフック(WH_KEYBOARD_LL)による空打ち判定とIME切替。
//!
//! 仕組み:
//! - 左/右AltのKeyDownで対応する空打ちフラグをtrueにする。
//! - Alt以外のキーのKeyDownで両方のフラグをfalseにする(複合キー除外)。
//! - AltのKeyUpでフラグがtrueのまま(=空打ち)ならIMEを切替える。
//!
//! また、Alt押下でメニューバーがアクティブになる問題を、参照元(AHK)と同様に
//! 抑制キー(F13)の入力を注入してキャンセルする。
//!
//! トレイメニューで無効化されている間は本モジュールの処理全体をスキップし、全キーを素通りさせる。
//!
//! Why F13か(参照元 AHK の vk07 から変更):
//! - Alt押下中に非Altキーを1発打てばWin32メニュー活性化はキャンセルできる。参照元は
//!   未割当キー vk07 でこれを行うが、Chrome 等のブラウザは vk07 をDOMイベントとして
//!   ページへ表面化しないため、Web版Excel(Office on the web)のJSレベルの「Alt単押し」
//!   検知(KeyTips表示)を打ち消せない。
//! - F13(VK 0x7C)は実際のキーボードに存在しないLegacyキーで、DOM keydown/keyup
//!   (Chrome実測 keyCode 124)としてページへ配信される。Excel webは「Alt押下中に別キーがあった
//!   =複合キー」と認識しKeyTipsを出さなくなる。Win32メニュー抑制も同一仕組みで効く。
//!   代償として F13 に割当のあるアプリ(実用上稀)では空打ち時にゴーストF13が見える。
//!   また自己注入F13と実F13を区別しないため、Alt押下中に物理F13を打っても空打ち判定は
//!   解除されない(LLKHF_INJECTED判定は行わない設計)。
//! - F13はAltのKeyDown時と空打ち確定のKeyUp時の2箇所で注入する。KeyDown時の注入は
//!   注入イベントが物理Alt KeyDownの配送に先行してしまうためWebアプリには「Altより前の
//!   キー」として届き複合キー扱いされない(メニュー抑制には効く)。KeyUp時の注入は配信中の
//!   Alt KeyUpより先行して「Alt押下中のキー」として届くため、WebアプリのAlt単押し判定を
//!   確実に打ち消せる(CDP計測による順序検証済み)。
//!
//! Why F13注入を別スレッド + Event 駆動で行うか:
//! - 参照元 AHK の *~LAlt::Send {Blind}{vk07} はホットキー発火で即座に注入キーを送る。
//!   ホットキーは LLフックプロシージャとは別の専用スレッドで動き、そこから SendInput する。
//! - LLフックプロシージャ内で SendInput すると注入キーが Alt と同一スタックで処理され
//!   抑制が効かなくなる(初版 f7bc0c8、順序制御でも解決せず案A で実証)。
//! - PostMessage 経由(ea129c4)だと機能はするが、メッセージポンプのサイクル待ちが
//!   レイテンシとなり、早い Alt+Space 操作で注入が Space 押下より後にずれ込んで
//!   システムメニューが開く(本セッション修正の直接原因)。
//! - そこで F13 注入だけを担う専用スレッドを立ち上げ、LLフックからは SetEvent で
//!   即座に起床させる(参照元 AHK のホットキースレッドと同アーキテクチャ)。SetEvent は
//!   カーネルオブジェクトのシグナル操作でナノ秒オーダー、かつ別スタックで SendInput
//!   されるため F13 は独立した入力として処理され、抑制が効きつつレイテンシも最小化する。

use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::thread;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    CreateEventW, SetEvent, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

// 仮想キーコード(左右Alt)。LLフックの vkCode で直接取得できる。
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
// メニュー/Webアプリ抑制用のキー(AHKのvk07相当をF13へ変更)。
// SendInputの再帰フックを無視するため特別扱いする。
// Constraint: 0x7C (F13) は実際のキーボードに存在しないがDOMイベントとしては配信されるキー。
//   Win32メニュー活性化のキャンセルと、Web版ExcelのKeyTips抑制(複合キー偽装)を1つの注入で兼ねる。
const VK_SUPPRESS: u32 = 0x7C;

// 空打ち判定状態。Alt押下時にtrue、押下中に別キーが打たれたらfalse。
static LALT_CLEAN: AtomicBool = AtomicBool::new(false);
static RALT_CLEAN: AtomicBool = AtomicBool::new(false);
// 機能の有効/無効。トレイメニューから切替わる。無効の間はフック手続きは何もせず全キーを素通りさせる。
static ENABLED: AtomicBool = AtomicBool::new(true);
// フックハンドル(解除用)。プロシージャと同じスレッドで触る。
static HHOOK: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());
// IME 切替要求の PostMessage 先。main がトレイウィンドウ生成後に登録する。
static TRAY_HWND: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());
// F13 抑制注入スレッドの起床用 Event。start_suppress_thread が生成後に登録する。
static SUPPRESS_EVENT: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());

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

/// フックコールバックから IME 切替を依頼するトレイウィンドウを登録する。
pub unsafe fn set_tray_hwnd(hwnd: HWND) {
    TRAY_HWND.store(hwnd, Ordering::SeqCst);
}

/// IME切替機能(空打ち判定とF13抑制)の有効/無効を設定する。
/// Why: ゲーム全画面など一時的に本ツールの挙動を止めたい場面向け。無効の間は
///   low_level_proc が何もしないため Alt は通常のキーとして動作する(メニューバー
///   活性化の抑制も効かなくなる)。状態はプロセス内のみで保持し、起動時は常に有効。
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    if !on {
        // Why: Alt押下中に無効化した場合に空打ちフラグが true のまま残っていると、
        //   再有効化直後の KeyUp で無効化期間をまたいだ空打ちと誤判定されるため掃落とす。
        LALT_CLEAN.store(false, Ordering::SeqCst);
        RALT_CLEAN.store(false, Ordering::SeqCst);
    }
}

/// IME切替機能が有効かどうか。トレイメニューのチェック状態の表示に使う。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// F13 抑制注入だけを担う専用スレッドを起動する。
/// Why: LLフックプロシージャ内で SendInput すると F13 が Alt と同一スタックで処理され
///   抑制が効かなくなる(初版 f7bc0c8、順序制御でも解決せず)。そこで F13 注入専用の別
///   スレッドを立ち上げ、LLフックからは SetEvent で即座に起床させる(参照元 AHK の
///   ホットキースレッドと同アーキテクチャ)。詳細はモジュールdoc参照。
pub unsafe fn start_suppress_thread() {
    // 自動リセット(bManualReset=0)・初期非シグナル(bInitialState=0)。WaitForSingleObject
    // の待機解除と同時に非シグナルへ戻り、連続 SetEvent でも1回の起床=1回の注入になる。
    let event = CreateEventW(core::ptr::null(), 0, 0, core::ptr::null());
    if event.is_null() {
        return;
    }
    SUPPRESS_EVENT.store(event, Ordering::SeqCst);
    // Why: プロセス終了でスレッドも消えるため、明示的な終了処理は持たない(常駐ツールの性質上、Event/スレッドのライフサイクルはプロセスに一致させる)。
    // Why: event(HANDLE=*mut c_void) は Send ではないためクロージャへ move できず、static から読み出して使う。
    thread::spawn(|| unsafe {
        let event = SUPPRESS_EVENT.load(Ordering::SeqCst);
        if event.is_null() {
            return;
        }
        loop {
            // WAIT_OBJECT_0(=0) は Event がシグナル状態になったことを示す。
            if WaitForSingleObject(event, INFINITE) == 0 {
                suppress_menu();
            }
        }
    });
}

/// 低レベルキーボードフックのプロシージャ。
unsafe extern "system" fn low_level_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    // Why: トレイメニューで無効化されている間は空打ち判定・F13抑制の両方を行わない。
    //   フック自体は残したままでプロシージャ先頭で抜けるため、全キーはそのまま次へ渡る。
    if code >= 0 && is_enabled() {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);

        // 抑制用キー自身は無視(SendInputの再帰呼出で空打ち判定を壊さないため)
        // Why: F13 を SendInput で注入すると本フックへ再帰的に流入する。ここを通過させると「Alt以外のキー」扱いされて空打ちフラグが即falseに落ち、IME切替が発火しなくなるため除外が必要(commit f7bc0c8)。
        if kb.vkCode != VK_SUPPRESS {
            // Constraint: Alt押下中はシステムが WM_SYSKEYDOWN/UP を発行する。WM_KEY* 系だけだと Alt 単独打鍵を取りこぼすため、両系統を OR で網羅する必要がある(MSDN WM_SYSKEYDOWN 仕様)。
            let down = wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize;
            let up = wparam == WM_KEYUP as usize || wparam == WM_SYSKEYUP as usize;

            match kb.vkCode {
                VK_LMENU => {
                    if down {
                        LALT_CLEAN.store(true, Ordering::SeqCst);
                        // Why: F13 抑制は別スレッドへ SetEvent で即座に依頼する。本コールバックは最後の CallNextHookEx で Alt を伝播して抜ける(詳細はモジュールdoc)。
                        request_suppress();
                    } else if up && LALT_CLEAN.swap(false, Ordering::SeqCst) {
                        // Why: 読み出しと同時にフラグを落とすことで、KeyUp〜IME切替の間に別キーが割り込んでも二重切替を防ぐ。単純な load+store だと判定後に状態が変わり得るため swap 一発でatomicに処理する。
                        // 左Alt空打ち → IME OFF をメインスレッドへ非同期依頼
                        // Why: フックコールバック内で ime::set_on を同期的に呼ぶと、その中の SendMessageW(WM_IME_CONTROL) が IME 側スレッドの応答を待ってメインスレッドをブロックし、フック設置スレッドが止まって抑制注入の配送まで遅れるため、PostMessage でコールバック外へ追い出す。
                        request_ime_toggle(false);
                        // Why: 空打ち確定の KeyUp 時にも F13 を注入する。KeyDown 時の注入は物理 Alt KeyDown の配送より
                        // 先に割り込んでしまうためページには「Alt より前の F13」として届き、Webアプリは複合キーと
                        // 認識しない(Excel web の KeyTips が出る)。KeyUp コールバック中に注入すれば F13 は配信中の
                        // Alt KeyUp より先に割り込み「Alt 押下中の F13」として届くため、Webアプリが Alt 単押しと
                        // 誤判定するのを確実に打ち消せる(CDP計測: Alt↓…F13↓↑…Alt↑ の順で KeyTips 抑制を確認)。
                        request_suppress();
                    }
                }
                VK_RMENU => {
                    if down {
                        RALT_CLEAN.store(true, Ordering::SeqCst);
                        // Why: LALT_CLEAN のdownブロックと同一(request_suppress で別スレッドへ依頼)。
                        request_suppress();
                    } else if up && RALT_CLEAN.swap(false, Ordering::SeqCst) {
                        // Why: 上記 LALT_CLEAN と同様、読み出しとリセットを swap 一発でatomicに行い二重切替を防ぐ。
                        // 右Alt空打ち → IME ON をメインスレッドへ非同期依頼(request_ime_toggle の詳細は LAlt 側コメント参照)
                        request_ime_toggle(true);
                        // Why: LALT_CLEAN の KeyUp ブロックと同一(空打ち確定時の F13 注入で Webアプリの Alt 単押し誤判定を打ち消す)。
                        request_suppress();
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

/// F13 抑制注入を専用スレッドへ即座に依頼する。
/// Why: SetEvent はカーネルオブジェクトのシグナル操作でナノ秒オーダー。PostMessage の
///   メッセージポンプサイクルを経由せず、LLフックプロシージャをブロックしない。
unsafe fn request_suppress() {
    let event = SUPPRESS_EVENT.load(Ordering::SeqCst);
    // Why: start_suppress_thread 完了前は Event 未登録で null。この間は抑制を諦め、登録後の Alt 押下から有効化する(初回数発の抑制漏れは許容)。
    if !event.is_null() {
        // Why: SetEvent の失敗は Event 破損時のみでリカバリ手段が無いため無視する。
        let _ = SetEvent(event);
    }
}

/// IME 切替(ime::set_on)をメッセージループ側へ非同期で依頼する。
/// Why: フックコールバック内で ime::set_on を呼ぶと、その中の SendMessageW(WM_IME_CONTROL)
///   が IME 側スレッドの応答を待ってメインスレッドをブロックする。LLフックは
///   LowLevelHooksTimeout(規定300ms) を超えるとOSに無効化されるリスクがあるため、コールバック
///   からは依頼だけ PostMessage で投げて即リターンする(F13抑制の別スレッド化とは独立に必要)。
unsafe fn request_ime_toggle(on: bool) {
    let hwnd = TRAY_HWND.load(Ordering::SeqCst);
    // Why: 起動直後など tray::create 完了前は HWND 未登録で null になる。この間は IME 切替を諦め、登録後の Alt 押下から有効化する(初回数発の切替漏れは許容)。
    if !hwnd.is_null() {
        // wParam: 0=IME OFF, 1=IME ON
        let wparam: usize = if on { 1 } else { 0 };
        // Why: 失敗(キュー満杯等)時でもフックコールバック内でリトライ/ブロックは禁止されるため諦める。切替漏れは影響限定。
        let _ = PostMessageW(hwnd, crate::WM_APP_IME_TOGGLE, wparam, 0);
    }
}

/// Alt押下でメニューバー/Webアプリ(Excel webのKeyTips等)がアクティブになるのを防ぐため、
/// F13を注入する。start_suppress_thread で起動した F13 注入専用スレッドから呼ばれる。
unsafe fn suppress_menu() {
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
