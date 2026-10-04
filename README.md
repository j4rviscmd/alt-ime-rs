<!-- markdownlint-disable MD036 -->
# alt-ime-rs

Windows向けアプリ。左右の`ALT`キーでIME ON/OFF切り替えを行う。Rust製で軽量・高速

このプロジェクトでは [alt-ime-ahk](https://github.com/karakaram/alt-ime-ahk) のような機能をrustにて実現して、exe配布を行う

*連続的な空打ちでIMEロックされる問題をfix*

## 要件

- 対象OSはWindowsのみ(動作保証はwin11のみ)
- 左altでIME OFF
  - すでにoffの状態で押下しても何もしない
- 右altでIME ON
  - すでにonの状態で押下しても何もしない
- `alt+tab`など複合キー入力の場合は何もしない
- メニューバー活性化抑制のためのF13注入は、フォーカスが端末(Windows Terminal・mintty・VSCode内蔵端末等)の場合は行わない
  - 端末に転送されたF13がエスケープシーケンスとなり、nvim等で`<F13>`が入力されるのを防ぐため
  - 対象端末の一覧は`src/terminal.rs`参照
- トレイに常駐
  - 右クリックで次回OS起動時に自動起動メニュー
  - exe移動等で自動起動登録の導線が切れている場合、起動時に現在のパスへ自動修復(明示的に無効化している場合は再登録しない)
- アップデートに対応
  - 起動時とメニュー「アップデートを確認する」でGitHub Releasesを確認し、更新があれば同意ダイアログを表示
  - 同意するとexeのDL・差し替え・再起動まで自動で行う(失敗時は配布ページへの手動導線を案内)
- exeでビルド & 配布する

## 謝辞・出典

本プロジェクトは、以下のプロジェクトのアイデアとアルゴリズムを参考に Rust で再実装したものです。

- [alt-ime-ahk](https://github.com/karakaram/alt-ime-ahk) — [karakaram](https://github.com/karakaram) 氏による、左右 Alt キー空打ちでの IME 切替のオリジナル実装（AutoHotkey 版）。本プロジェクトの設計の土台となっています。
- **IME.ahk**（eamat 氏）— alt-ime-ahk が利用している IME 制御ライブラリ。IME の ON/OFF 切替アルゴリズムの出典です。

## ライセンス

MIT
