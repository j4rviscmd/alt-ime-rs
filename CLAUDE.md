# CLAUDE.md

このファイルは Claude Code（AI コーディングアシスタント）向けの開発ガイドです。

## 言語

- issue / PR / ソースコード内コメントはすべて日本語で記述すること

## 開発

- Windows 向けのデスクトップ常駐アプリ（トレイ常駐・左右 Alt キー空打ちによる IME 切替）
- 動作確認は `cargo run --release` で行う
  - すでに起動している場合はキルしてから起動すること

## リリース

- `release-please` による自動化(`.github/workflows/release-please.yml`)
  - mainにfeat/fixコミットがマージされるとRelease PR(バージョン バンプ+Cargo.toml+CHANGELOG.md)が自動作成される
  - Release PRをマージするとタグ+GitHub Releaseが作成され、Actionsがexeをビルドして添付する
  - 旧CalVer版利用者への案内としてsentinel(v9999.99.99)をLatest固定している(詳細はワークフロー内コメント)
