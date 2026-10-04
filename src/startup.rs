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

/// 登録パスの実行ファイルが存在しない(=自動起動の導線が壊れている)場合、現在のexeパスへ書き直す。
/// Why: ユーザーがexeを移動・リネームするとRunキーの登録パスが実態と乖離し、OS起動時に
///   黙って起動しなくなる。手動起動を機に壊れた登録だけを現在地へ修復する。
/// Constraint: 値が存在しない場合は明示的な無効化の可能性があるため再登録しない。
///   登録先のファイルが実在する場合(別フォルダのコピー試用等、登録自体は機能している)も触らない。
pub fn repair_if_stale() {
    let Some(raw) = (unsafe { read_value() }) else {
        return;
    };
    let registered = exe_path_from_value(&String::from_utf16_lossy(&raw));
    if std::path::Path::new(&registered).exists() {
        return;
    }
    if let Ok(current) = std::env::current_exe() {
        unsafe { write_value(&current.to_string_lossy()) };
    }
}

/// Runキー値(コマンドラインとして解釈される)から実行ファイルのパス部分を取り出す。
/// Why: write_value は引用符付きパスを書き込むが、レジストリを手書き編集されると引用符なし・
///   引数付きの形式も取り得るため、先頭トークンを寛容に解釈する。
fn exe_path_from_value(value: &str) -> String {
    let s = value.trim_end_matches('\0').trim();
    if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or_default().to_string()
    } else {
        s.split_whitespace().next().unwrap_or_default().to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::exe_path_from_value;

    // write_value が書く形式: 引用符付きパス+レジストリ値のnull終端
    #[test]
    fn parses_quoted_path_with_nul() {
        assert_eq!(
            exe_path_from_value("\"C:\\Apps\\alt-ime-rs.exe\"\u{0}"),
            "C:\\Apps\\alt-ime-rs.exe"
        );
    }

    // 引用符付きパス+引数: 閉じ引用符までをパスとして取り出す
    #[test]
    fn parses_quoted_path_with_args() {
        assert_eq!(
            exe_path_from_value("\"C:\\Apps\\alt-ime-rs.exe\" --option"),
            "C:\\Apps\\alt-ime-rs.exe"
        );
    }

    // 引用符なし(手書き編集時): 最初の空白までをパスとみなす(コマンドライン解釈と同じ曖昧性)
    #[test]
    fn parses_unquoted_path() {
        assert_eq!(
            exe_path_from_value("C:\\Apps\\alt-ime-rs.exe"),
            "C:\\Apps\\alt-ime-rs.exe"
        );
    }

    // 空文字・空白のみ: パスなしとして空文字を返す(呼び出し側は存在チェックで修復へ向かう)
    #[test]
    fn parses_empty_to_empty() {
        assert_eq!(exe_path_from_value("\"\u{0}"), "");
        assert_eq!(exe_path_from_value("   "), "");
    }

    // 引用符なし+引数(手書き編集時): 最初の空白までをパスとして切り捨てる
    #[test]
    fn parses_unquoted_path_with_args() {
        assert_eq!(
            exe_path_from_value("C:\\Apps\\alt-ime-rs.exe --option\u{0}"),
            "C:\\Apps\\alt-ime-rs.exe"
        );
    }

    // 閉じ引用符なし(手書き編集の壊れ方): 残り全体をパスとみなす(存在チェックfalse→修復へ向かう)
    #[test]
    fn parses_unclosed_quote_as_remainder() {
        assert_eq!(
            exe_path_from_value("\"C:\\Apps\\alt-ime-rs.exe"),
            "C:\\Apps\\alt-ime-rs.exe"
        );
    }
}
