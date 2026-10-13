//! 低レベルキーボードフック(WH_KEYBOARD_LL)による空打ち判定とIME切替。
//!
//! 仕組み:
//! - 左/右AltのKeyDownで対応する空打ちフラグをtrueにする。
//! - Alt以外のキーのKeyDownで両方のフラグをfalseにする(複合キー除外)。
//! - AltのKeyUpでフラグがtrueのまま(=空打ち)ならIMEを切替える。
//! - トレイメニューの「Alt同時押しでIME切替しない」有効時は、同時押し(押下の重なり)で
//!   IME切替とKeyDown時F13注入を行わない。KeyUp時F13注入は古典Win32系(Office等)のみ行う
//!   (XAMLメニューバー系では注入が停滞聚焦を誘発するためスキップ。対象はxamlmenuモジュール参照)。
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
//!   代償として F13 に割当のあるアプリ(実用上稀)では空打ち時にゴーストF13が見える
//!   (該当最大の端末は後述の通りスキップ対象とし、実際に稀になった)。
//!   また自己注入F13と実F13を区別しないため、Alt押下中に物理F13を打っても空打ち判定は
//!   解除されない(LLKHF_INJECTED判定は行わない設計)。
//! - F13はAltのKeyDown時と空打ち確定のKeyUp時の2箇所で注入する。KeyDown時の注入は
//!   注入イベントが物理Alt KeyDownの配送に先行してしまうためWebアプリには「Altより前の
//!   キー」として届き複合キー扱いされない(メニュー抑制には効く)。KeyUp時の注入は配信中の
//!   Alt KeyUpより先行して「Alt押下中のキー」として届くため、WebアプリのAlt単押し判定を
//!   確実に打ち消せる(CDP計測による順序検証済み)。
//!
//! Why 端末ではF13注入をスキップするか:
//! - 端末はメニューバーを持たずF13注入の利益がゼロ。一方で端末はF13をエスケープシーケンスとして
//!   シェルへ転送するため、nvim等で<F13>が入力されてしまう。そのため
//!   terminal::focused_is_terminal がtrueの間は注入だけを行わない(IME切替の空打ち判定は
//!   影響を受けない)。対象端末と判定方法は terminal モジュールのdoc参照。
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

use crate::terminal;
use crate::xamlmenu;

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
// 左右Altの物理的な押下中フラグ。同時押し(押下の重なり)の判定に使う。
static LALT_HELD: AtomicBool = AtomicBool::new(false);
static RALT_HELD: AtomicBool = AtomicBool::new(false);
// 左右Altの押下が重なってから両方離れるまで立つフラグ。この間のAlt操作は一連の同時押しジェスチャ。
static SIMULT_PRESS: AtomicBool = AtomicBool::new(false);
// トレイメニュー「Alt同時押しでIME切替しない」。有効時は同時押しでIME切替とKeyDown時F13注入を行わない
//   (KeyUp時F13注入は古典Win32系のみ。詳細はモジュールdocとalt_event参照)。
// 既定は有効(未設定はON)。main が起動時に設定値を反映するまでの初期値としても true を保つ。
static PASSTHROUGH: AtomicBool = AtomicBool::new(true);
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
        // Why: 押下中/同時押しフラグも同様に掃落とす。残ると無効化をまたいだ「幽霊押下中」として
        //   再有効化後の最初のAlt押下が同時押しと誤判定されるため。
        LALT_HELD.store(false, Ordering::SeqCst);
        RALT_HELD.store(false, Ordering::SeqCst);
        SIMULT_PRESS.store(false, Ordering::SeqCst);
    }
}

/// IME切替機能が有効かどうか。トレイメニューのチェック状態の表示に使う。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// 「Alt同時押しでIME切替しない」の有効/無効を設定する。main が起動時に設定値を反映し、
/// トレイメニューから切替わる。有効時は左右Altの同時押し(押下の重なり)でIME切替と
/// KeyDown時F13注入を行わない(KeyUp時注入は古典Win32系のみ。モジュールdoc参照)。
pub fn set_passthrough(on: bool) {
    PASSTHROUGH.store(on, Ordering::SeqCst);
}

/// 「Alt同時押しでIME切替しない」が有効かどうか。トレイメニューのチェック状態の表示に使う。
pub fn is_passthrough() -> bool {
    PASSTHROUGH.load(Ordering::SeqCst)
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
                // 左Altの空打ち確定はIME OFF、右AltはIME ONへ切替
                VK_LMENU | VK_RMENU => alt_event(kb.vkCode, down, up),
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

/// 左右Altで共通の押下/解放処理。vk は処理中のAltの仮想キーコード(VK_LMENU/VK_RMENU)。
/// 空打ち確定時の切替方向は左Alt=IME OFF、右Alt=IME ON。
/// Why: VK_LMENU/VK_RMENU はロジックが完全に鏡像のため、両辺を別々に保守すると挙動が
///   割れる。左右の違いは冒頭の組替えだけに集約し、以降の処理を1箇所にまとめる。
unsafe fn alt_event(vk: u32, down: bool, up: bool) {
    // 処理中のAlt(own)と反対側(other)の状態を左右で組み替える
    let (own_held, other_held, own_clean, other_clean, ime_on) = if vk == VK_LMENU {
        (&LALT_HELD, &RALT_HELD, &LALT_CLEAN, &RALT_CLEAN, false)
    } else {
        (&RALT_HELD, &LALT_HELD, &RALT_CLEAN, &LALT_CLEAN, true)
    };
    if down {
        // 相手側Altが押下中なら「同時押し(押下の重なり)」と判定し、
        // 両方離れるまでの間この一連の操作を同時押しジェスチャとして扱う
        if other_held.load(Ordering::SeqCst) {
            SIMULT_PRESS.store(true, Ordering::SeqCst);
        }
        own_held.store(true, Ordering::SeqCst);
        let passthrough = is_passthrough();
        if passthrough && SIMULT_PRESS.load(Ordering::SeqCst) {
            // Why: 「Alt同時押しでIME切替しない」有効時は両Altの空打ちフラグを落とし、
            //   KeyUpでのIME切替を発火させない(同時押しで二重に切替わるのを防ぐ)。
            own_clean.store(false, Ordering::SeqCst);
            other_clean.store(false, Ordering::SeqCst);
        } else {
            own_clean.store(true, Ordering::SeqCst);
            // Why: F13 抑制は別スレッドへ SetEvent で即座に依頼する。本コールバックは最後の CallNextHookEx で Alt を伝播して抜ける(詳細はモジュールdoc)。
            // Why オプション有効時はKeyDown時のF13注入を省く: 同時押しの先頭Altに先行する
            //   この1発だけが残ると、WinUI(XAML)のMenuBarが同時押しを「メニューアクセス」と
            //   誤解釈して聚焦することが実機メモ帳計測で判明している。単独空打ちのメニュー抑制は
            //   KeyUp時の注入(F13がAlt押下中として届く)だけで古典Win32/Web両方に効くため、
            //   Down時注入は二重掛けの保険であり省ける。
            if !passthrough {
                request_suppress();
            }
        }
    } else if up {
        // Why: 同時押しジェスチャー中かはSIMULT_PRESS解除前の値で判定する(最後に離れたAltの
        //   KeyUpでフラグを落とすため、先に読んでおかないと注入要否を取りこぼす)。
        let was_simult = SIMULT_PRESS.load(Ordering::SeqCst) && is_passthrough();
        own_held.store(false, Ordering::SeqCst);
        // Why: 両Altが離れた時点で同時押しジェスチャー終了(自分側は直上で解放済み)。フラグを
        //   残すと次の単独押下が同時押しと誤判定されて切替できなくなるため、ここで確実に落とす。
        if !other_held.load(Ordering::SeqCst) {
            SIMULT_PRESS.store(false, Ordering::SeqCst);
        }
        if own_clean.swap(false, Ordering::SeqCst) {
            // Why: 読み出しと同時にフラグを落とすことで、KeyUp〜IME切替の間に別キーが割り込んでも二重切替を防ぐ。単純な load+store だと判定後に状態が変わり得るため swap 一発でatomicに処理する。
            // このAltの空打ち → IME 切替(左=OFF/右=ON)をメインスレッドへ非同期依頼
            // Why: フックコールバック内で ime::set_on を同期的に呼ぶと、その中の SendMessageW(WM_IME_CONTROL) が IME 側スレッドの応答を待ってメインスレッドをブロックし、フック設置スレッドが止まって抑制注入の配送まで遅れるため、PostMessage でコールバック外へ追い出す。
            request_ime_toggle(ime_on);
            // Why: 空打ち確定の KeyUp 時にも F13 を注入する。KeyDown 時の注入は物理 Alt KeyDown の配送より
            // 先に割り込んでしまうためページには「Alt より前の F13」として届き、Webアプリは複合キーと
            // 認識しない(Excel web の KeyTips が出る)。KeyUp コールバック中に注入すれば F13 は配信中の
            // Alt Key Up より先に割り込み「Alt 押下中の F13」として届くため、Webアプリが Alt 単押しと
            // 誤判定するのを確実に打ち消せる(CDP計測: Alt↓…F13↓↑…Alt↑ の順で KeyTips 抑制を確認)。
            request_suppress();
        } else if was_simult && !xamlmenu::focused_is_xaml_menu_app() {
            // Why: 古典Win32系(Officeリボン等)では素の同時押しでもアクセスキー(KeyTips)が
            //   活性化するため、単独空打ちと同一のKeyUp時F13注入で打ち消す(実機Excel計測)。
            //   XAMLメニューバー系(Win11メモ帳等)では逆にF13が停滞聚焦を誘発するため注入しない
            //   (実機メモ帳計測。対象リストと理由はxamlmenuモジュールdoc参照)。
            request_suppress();
        }
    }
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
/// フォーカスが端末の場合はF13がシェルへ転送されてnvim等に<F13>が入力されるため注入をスキップする。
unsafe fn suppress_menu() {
    // Why: 端末はメニューバーを持たずF13注入の利益がない一方、F13をエスケープシーケンスとして
    //   シェルへ転送してしまうためスキップする。判定は注入直前(=配送先が確定した瞬間)のフォーカスで行う。
    if terminal::focused_is_terminal() {
        return;
    }
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

/// テスト間でフック系statics(プロセスグローバル)へのアクセスを排他するロック。
/// Why: cargo test はテストを並列実行するため、本モジュールのstaticsを触るテスト
///   (tray.rs のトグルテストを含む)が互いに状態を汚染し合わないよう共有して直列化する。
#[cfg(test)]
pub(crate) static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::ResetEvent;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, PeekMessageW, HWND_MESSAGE, MSG, PM_REMOVE,
    };

    /// フック系staticsを初期状態へ戻す(各テストの事前準備)。
    fn reset_state() {
        ENABLED.store(true, Ordering::SeqCst);
        PASSTHROUGH.store(false, Ordering::SeqCst);
        LALT_CLEAN.store(false, Ordering::SeqCst);
        RALT_CLEAN.store(false, Ordering::SeqCst);
        LALT_HELD.store(false, Ordering::SeqCst);
        RALT_HELD.store(false, Ordering::SeqCst);
        SIMULT_PRESS.store(false, Ordering::SeqCst);
        TRAY_HWND.store(core::ptr::null_mut(), Ordering::SeqCst);
        SUPPRESS_EVENT.store(core::ptr::null_mut(), Ordering::SeqCst);
    }

    /// LLフックプロシージャへキーイベントを流す。Alt単独打鍵はWM_SYSKEY系で配信される。
    fn send_key(vk: u32, down: bool) {
        let msg = if down { WM_SYSKEYDOWN } else { WM_SYSKEYUP };
        send(vk, msg as usize);
    }

    /// LLフックプロシージャへキーイベントを流す(メッセージ種別は WM_KEY/WM_SYSKEY 系から指定)。
    fn send(vk: u32, wparam: usize) {
        let mut kb: KBDLLHOOKSTRUCT = unsafe { core::mem::zeroed() };
        kb.vkCode = vk;
        unsafe {
            low_level_proc(0, wparam, &kb as *const KBDLLHOOKSTRUCT as isize);
        }
    }

    /// IME切替要求のPostMessage先として、メッセージ専用ウィンドウを作って登録する。
    /// 戻り値のハンドルはテスト末尾で destroy_test_window へ渡すこと。
    fn create_test_window() -> HWND {
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                crate::wide("STATIC").as_ptr(),
                crate::wide("").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                core::ptr::null_mut(),
                GetModuleHandleW(core::ptr::null()),
                core::ptr::null(),
            );
            assert!(!hwnd.is_null(), "テスト用メッセージウィンドウの作成に失敗");
            TRAY_HWND.store(hwnd, Ordering::SeqCst);
            hwnd
        }
    }

    /// テスト用ウィンドウを破棄し、staticsを初期状態へ戻す。
    fn destroy_test_window(hwnd: HWND) {
        unsafe {
            DestroyWindow(hwnd);
        }
        reset_state();
    }

    /// F13抑制注入スレッドの代わりにEventを登録し、抑制要求(SetEvent)を観測可能にする。
    fn arm_suppress_event() {
        unsafe {
            let ev = CreateEventW(core::ptr::null(), 0, 0, core::ptr::null());
            assert!(!ev.is_null(), "テスト用Eventの作成に失敗");
            SUPPRESS_EVENT.store(ev, Ordering::SeqCst);
        }
    }

    /// F13抑制要求が発火したかを確認し、シグナルを消費する。
    fn suppress_requested() -> bool {
        unsafe {
            let ev = SUPPRESS_EVENT.load(Ordering::SeqCst);
            let signaled = WaitForSingleObject(ev, 0) == WAIT_OBJECT_0;
            let _ = ResetEvent(ev);
            signaled
        }
    }

    /// IME切替要求(WM_APP_IME_TOGGLE)をキューから1件取り出す。wparamは切替方向(0=OFF/1=ON)。
    fn pop_toggle_request() -> Option<usize> {
        unsafe {
            let mut msg: MSG = core::mem::zeroed();
            let found = PeekMessageW(
                &mut msg,
                TRAY_HWND.load(Ordering::SeqCst),
                crate::WM_APP_IME_TOGGLE,
                crate::WM_APP_IME_TOGGLE,
                PM_REMOVE,
            );
            (found != 0).then_some(msg.wParam)
        }
    }

    /// 「Alt同時押しでIME切替しない」有効時でも、単独のAlt空打ちは従来どおりIME切替する。
    /// F13抑制はKeyUp時のみ要求する(オプション有効時はKeyDown時の注入を省くため)。
    /// 左Alt=IME OFF(wparam 0)、右Alt=IME ON(wparam 1)の切替方向も確認する。
    #[test]
    fn single_alt_tap_still_toggles_ime_and_suppresses() {
        let _g = TEST_LOCK.lock();
        reset_state();
        set_passthrough(true);
        arm_suppress_event();
        let hwnd = create_test_window();

        send_key(VK_LMENU, true);
        assert!(
            !suppress_requested(),
            "オプション有効時はKeyDown時のF13注入を省く"
        );
        send_key(VK_LMENU, false);
        assert_eq!(
            pop_toggle_request(),
            Some(0),
            "左Alt空打ちはIME OFF(0)を要求"
        );
        assert!(
            suppress_requested(),
            "空打ち確定のKeyUpではF13抑制を要求する"
        );

        send_key(VK_RMENU, true);
        send_key(VK_RMENU, false);
        assert_eq!(
            pop_toggle_request(),
            Some(1),
            "右Alt空打ちはIME ON(1)を要求"
        );
        assert!(
            pop_toggle_request().is_none(),
            "1回の空打ちで切替要求は1件だけ"
        );

        destroy_test_window(hwnd);
    }

    /// 「Alt同時押しでIME切替しない」有効時、左右Altの同時押し(押下の重なり)は
    /// IME切替とKeyDown時F13注入を行わない。KeyUp時F13注入は古典Win32系
    /// (テスト実行中のフォアグラウンド=非XAMLアプリ)でのみ要求する。
    /// XAMLメニューバー系ではxamlmenuの判定で注入がスキップされる(実機検証側で担保)。
    #[test]
    fn simultaneous_press_with_passthrough_skips_toggle_but_suppresses_classic_menu() {
        let _g = TEST_LOCK.lock();
        reset_state();
        set_passthrough(true);
        arm_suppress_event();
        let hwnd = create_test_window();

        send_key(VK_LMENU, true);
        assert!(!suppress_requested(), "先頭AltのKeyDown時もF13を注入しない");
        send_key(VK_RMENU, true);
        assert!(
            !suppress_requested(),
            "同時押しの2発目KeyDownではF13を注入しない"
        );
        assert!(
            SIMULT_PRESS.load(Ordering::SeqCst),
            "押下の重なりで同時押しフラグが立つ"
        );

        send_key(VK_RMENU, false);
        assert_eq!(
            pop_toggle_request(),
            None,
            "同時押し中のKeyUpでは切替しない"
        );
        assert!(
            suppress_requested(),
            "古典Win32系では同時押しのKeyUpでもF13抑制を要求する"
        );
        send_key(VK_LMENU, false);
        assert_eq!(pop_toggle_request(), None, "同時押しの終了でも切替しない");
        assert!(
            suppress_requested(),
            "最後に離したAltのKeyUpでもF13抑制を要求する"
        );
        assert!(
            !SIMULT_PRESS.load(Ordering::SeqCst),
            "両方離れたら同時押しフラグは落ちる"
        );

        // 同時押しジェスチャ後にフラグが残らず、通常の空打ちに復帰する
        send_key(VK_LMENU, true);
        send_key(VK_LMENU, false);
        assert_eq!(
            pop_toggle_request(),
            Some(0),
            "同時押し後の単独空打ちは切替できる"
        );

        destroy_test_window(hwnd);
    }

    /// 「Alt同時押しでIME切替しない」無効時は従来挙動のまま: 同時押し中の各Alt解放も空打ち切替として扱う。
    #[test]
    fn simultaneous_press_without_passthrough_toggles_each_release() {
        let _g = TEST_LOCK.lock();
        reset_state();
        set_passthrough(false);
        let hwnd = create_test_window();

        send_key(VK_LMENU, true);
        send_key(VK_RMENU, true);
        send_key(VK_RMENU, false);
        assert_eq!(pop_toggle_request(), Some(1), "右Alt解放でIME ON");
        send_key(VK_LMENU, false);
        assert_eq!(pop_toggle_request(), Some(0), "左Alt解放でIME OFF");
        assert!(
            !SIMULT_PRESS.load(Ordering::SeqCst),
            "両方離れたら同時押しフラグは落ちる"
        );

        destroy_test_window(hwnd);
    }

    /// Alt押下中の非Altキーで空打ちが取り消される。F13(VK_SUPPRESS)自身は空打ち判定に
    /// 影響せず、WM_KEY系(非SYSKEY)のAlt単独打鍵も取りこぼさない。
    #[test]
    fn other_key_cancels_tap_and_suppress_key_is_ignored() {
        let _g = TEST_LOCK.lock();
        reset_state();
        let hwnd = create_test_window();

        send_key(VK_LMENU, true);
        send_key(VK_SUPPRESS, true);
        send_key(VK_SUPPRESS, false);
        assert!(
            LALT_CLEAN.load(Ordering::SeqCst),
            "抑制キー自身では空打ちが解除されない"
        );

        send_key(0x41, true);
        assert!(
            !LALT_CLEAN.load(Ordering::SeqCst),
            "非Altキー押下で空打ちが取り消される"
        );
        send_key(VK_LMENU, false);
        assert_eq!(
            pop_toggle_request(),
            None,
            "複合キー後のKeyUpでは切替しない"
        );

        send(VK_RMENU, WM_KEYDOWN as usize);
        send(VK_RMENU, WM_KEYUP as usize);
        assert_eq!(pop_toggle_request(), Some(1), "WM_KEY系でも空打ち切替する");

        destroy_test_window(hwnd);
    }

    /// 一時無効(set_enabled(false))で押下中/同時押しフラグを掃き捨て、無効中はキーで
    /// 状態が変化しない。再有効化直後の最初のAlt押下が同時押しと誤判定されない。
    #[test]
    fn set_enabled_resets_simultaneous_state_and_gates_events() {
        let _g = TEST_LOCK.lock();
        reset_state();

        LALT_HELD.store(true, Ordering::SeqCst);
        RALT_HELD.store(true, Ordering::SeqCst);
        SIMULT_PRESS.store(true, Ordering::SeqCst);
        set_enabled(false);
        assert!(!LALT_HELD.load(Ordering::SeqCst));
        assert!(!RALT_HELD.load(Ordering::SeqCst));
        assert!(
            !SIMULT_PRESS.load(Ordering::SeqCst),
            "無効化時に同時押し系フラグを掃き捨てる"
        );

        send_key(VK_LMENU, true);
        send_key(0x41, true);
        assert!(
            !LALT_HELD.load(Ordering::SeqCst),
            "無効中のキーで状態が変わらない"
        );
        assert!(!LALT_CLEAN.load(Ordering::SeqCst));

        set_enabled(true);
        send_key(VK_LMENU, true);
        assert!(
            LALT_CLEAN.load(Ordering::SeqCst),
            "再有効化直後の単独押下は空打ち対象"
        );
        assert!(
            !SIMULT_PRESS.load(Ordering::SeqCst),
            "幽霊押下中が残らず同時押しと誤判定しない"
        );
        send_key(VK_LMENU, false);

        reset_state();
    }
}
