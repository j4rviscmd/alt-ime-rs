//! アプリ設定の永続化。
//!
//! レジストリ HKCU\Software\alt-ime-rs に REG_DWORD で保存する。
//! Why: 自動起動(startup.rs)と同じ HKCU レジストリ運用で追加依頼なしに永続化できる。
//!   設定ファイルを別途作るほどの設定数ではない。
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
};

use crate::wide;

// 設定保存先のキーと値の名前
const SETTINGS_KEY: &str = "Software\\alt-ime-rs";
const VALUE_UPDATE_CHECK: &str = "UpdateCheckOnStartup";

/// 起動時のアップデート確認が有効か。値が無い/読めない場合は有効(既定)。
/// Why: 既存ユーザーの挙動(起動時に確認する)を変えないため、未設定は ON 扱い。
pub fn update_check_on_startup() -> bool {
    unsafe {
        let Some(hkey) = open_key() else {
            return true;
        };
        let mut value: u32 = 0;
        // Why: REG_DWORD は4バイト固定でバッファ不足(ERROR_MORE_DATA)が起こり得ないため、
        //   可変長 REG_SZ をサイズ取得→読み取りの2段階で扱う startup.rs と違い1回の読み取りで済む。
        let mut size: u32 = 4;
        let rc = RegQueryValueExW(
            hkey,
            wide(VALUE_UPDATE_CHECK).as_ptr(),
            core::ptr::null(),
            core::ptr::null_mut(),
            &mut value as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(hkey);
        // ERROR_SUCCESS(0) 以外は値なし/読み取り失敗 → 既定値(有効)
        rc != 0 || value != 0
    }
}

/// 起動時のアップデート確認の有効/無効を保存する。
pub fn set_update_check_on_startup(enabled: bool) {
    unsafe {
        let Some(hkey) = open_key() else {
            return;
        };
        let value: u32 = u32::from(enabled);
        let rc = RegSetValueExW(
            hkey,
            wide(VALUE_UPDATE_CHECK).as_ptr(),
            0,
            REG_DWORD,
            &value as *const u32 as *const u8,
            4,
        );
        RegCloseKey(hkey);
        debug_assert_eq!(rc, 0, "設定の保存に失敗: rc={}", rc);
    }
}

/// 設定キーを開く(無ければ作成する)。失敗時はNone。
unsafe fn open_key() -> Option<HKEY> {
    let mut hkey: HKEY = core::ptr::null_mut();
    // Why: Software\alt-ime-rs は自作キーで初回起動時に存在しないため作成を兼ねて開く。
    //   OSが用意する Runキーを開くだけの startup.rs と違い、開くのみだと最初の保存が失敗する。
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        wide(SETTINGS_KEY).as_ptr(),
        0,
        core::ptr::null(),
        REG_OPTION_NON_VOLATILE,
        KEY_QUERY_VALUE | KEY_SET_VALUE,
        core::ptr::null(),
        &mut hkey,
        core::ptr::null_mut(),
    );
    (rc == 0).then_some(hkey)
}

/// 値を削除する(テストで既定値に戻すために使用)。
#[cfg(test)]
pub(crate) fn reset_update_check_on_startup() {
    use windows_sys::Win32::System::Registry::RegDeleteValueW;
    unsafe {
        if let Some(hkey) = open_key() {
            RegDeleteValueW(hkey, wide(VALUE_UPDATE_CHECK).as_ptr());
            RegCloseKey(hkey);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 保存→読み取りの往復と、未設定時の既定値(有効)を確認する。
    #[test]
    fn update_check_roundtrip_and_default() {
        // 実レジストリを扱うため事前値を保存し、テスト後に復元する
        // Why: 開発マシンで当該設定を変更済みでも cargo test が黙ってリセットしないようにするため。
        let saved = update_check_on_startup();

        reset_update_check_on_startup();
        assert!(update_check_on_startup(), "未設定時は既定で有効");

        set_update_check_on_startup(false);
        assert!(!update_check_on_startup());

        set_update_check_on_startup(true);
        assert!(update_check_on_startup());

        reset_update_check_on_startup();
        assert!(update_check_on_startup(), "削除後は既定に戻る");

        set_update_check_on_startup(saved);
    }
}
