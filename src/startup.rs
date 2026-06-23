//! Windowsスタートアップ(ログオン時の自動起動)の登録・解除。
//!
//! レジストリ HKCU\Software\Microsoft\Windows\CurrentVersion\Run に
//! 実行ファイルのパスを登録することで実現する。

use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

use crate::wide;

// Runキーのパス
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
// 登録する値の名前
const VALUE_NAME: &str = "alt-ime-rs";
// ERROR_MORE_DATA(サイズ取得の合図)
const ERROR_MORE_DATA: u32 = 234;
// ERROR_FILE_NOT_FOUND
const ERROR_FILE_NOT_FOUND: u32 = 2;

/// 自動起動が有効(登録済み)かを返す。
pub fn is_enabled() -> bool {
    unsafe { read_value().is_some() }
}

/// 自動起動を有効にする(自exeのパスをRunキーへ登録)。成功時はtrue。
pub fn enable() -> bool {
    let path = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return false,
    };
    unsafe { write_value(&path) }
}

/// 自動起動を無効にする(Runキーから値を削除)。成功時はtrue。
pub fn disable() -> bool {
    unsafe { delete_value() }
}

/// 指定アクセス権でRunキーを開く。
unsafe fn open_key(sam: u32) -> Option<HKEY> {
    let subkey = wide(RUN_KEY);
    let mut hkey: HKEY = core::ptr::null_mut();
    let result = RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, sam, &mut hkey);
    if result == 0 {
        Some(hkey)
    } else {
        None
    }
}

/// Runキーから値を読み取る(存在しない場合はNone)。
unsafe fn read_value() -> Option<Vec<u16>> {
    let hkey = open_key(KEY_QUERY_VALUE)?;
    let name = wide(VALUE_NAME);

    // 1回目: サイズ取得
    // Note: RegQueryValueExW はバッファ不足時に ERROR_MORE_DATA(234) を返しつつ必要サイズを len に設定する仕様。これもサイズ取得の正常系として扱う。
    let mut len: u32 = 0;
    let ret = RegQueryValueExW(
        hkey,
        name.as_ptr(),
        core::ptr::null(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        &mut len,
    );
    if ret != 0 && ret != ERROR_MORE_DATA {
        RegCloseKey(hkey);
        return None;
    }
    if len == 0 {
        RegCloseKey(hkey);
        return None;
    }

    // 2回目: 実データ取得
    // Note: +1 は null終端の余裕確保。len はバイト単位(UTF-16は1要素2バイト)のため /2 で要素数へ変換。
    let mut buf = vec![0u16; (len as usize / 2) + 1];
    let ret = RegQueryValueExW(
        hkey,
        name.as_ptr(),
        core::ptr::null(),
        core::ptr::null_mut(),
        buf.as_mut_ptr() as *mut u8,
        &mut len,
    );
    RegCloseKey(hkey);
    if ret == 0 {
        Some(buf)
    } else {
        None
    }
}

/// Runキーへパスを書き込む。パスに空白が含まれる可能性を考慮し引用符で囲む。
// Constraint: Runキー値はコマンドラインとして解釈される。引用符なしでは "C:\Program Files\..." の空白で実行ファイル名が途切れるため必須。
unsafe fn write_value(path: &str) -> bool {
    let hkey = match open_key(KEY_SET_VALUE) {
        Some(h) => h,
        None => return false,
    };
    let name = wide(VALUE_NAME);
    let quoted = format!("\"{}\"", path);
    let data = wide(&quoted);
    let bytes = (data.len() * 2) as u32;
    let ret = RegSetValueExW(
        hkey,
        name.as_ptr(),
        0,
        REG_SZ,
        data.as_ptr() as *const u8,
        bytes,
    );
    RegCloseKey(hkey);
    ret == 0
}

/// Runキーから値を削除する。値が存在しなかった場合も成功扱い。
unsafe fn delete_value() -> bool {
    let hkey = match open_key(KEY_SET_VALUE) {
        Some(h) => h,
        None => return false,
    };
    let name = wide(VALUE_NAME);
    let ret = RegDeleteValueW(hkey, name.as_ptr());
    RegCloseKey(hkey);
    // Why: 無効化操作の冪等性。既に未登録(ERROR_FILE_NOT_FOUND)なら目標状態に合致するため成功扱いにする。
    ret == 0 || ret == ERROR_FILE_NOT_FOUND
}
