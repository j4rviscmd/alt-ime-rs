//! ビルドスクリプト: exe にアイコンリソースを埋め込む。
//!
//! winres で assets/icon.ico をリソース ID=1 の ICON として exe へ埋め込む。
//! Why ID=1: Windows はリソース順序で最も小さいアイコンを exe のデフォルトアイコン
//!   (エクスプローラ/タスクバー表示) に使う。トレイアイコンも tray::create が
//!   LoadIconW(hinst, MAKEINTRESOURCEW(1)) で同一リソースを読み込むため、両者が一致する。

fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    if let Err(e) = res.compile() {
        // Why 続行するか: rc.exe が無いクロスビルド環境等でも本体ビルドを止めないため。
        //   ローカル(Windows+MSVC)では成功し、アイコンが埋め込まれる。
        println!("cargo:warning=アイコンリソースのコンパイルに失敗しました(ビルドは続行): {e}");
    }

    // バージョンをバイナリへ焼き込む(アップデート確認機能が現在版として使用)。
    // Why: release.sh が環境変数 ALT_IME_VERSION に日付版(例: 2026.07.04)を設定する。
    //   未設定の通常 cargo build では Cargo.toml の version(0.1.0) にフォールバックし、
    //   比較ロジックが常に成立するようにする。
    println!("cargo:rerun-if-env-changed=ALT_IME_VERSION");
    let version = std::env::var("ALT_IME_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rustc-env=ALT_IME_VERSION={}", version);
}
