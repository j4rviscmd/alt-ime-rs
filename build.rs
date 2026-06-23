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
}
