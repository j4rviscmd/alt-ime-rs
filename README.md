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
- トレイに常駐
  - 右クリックで次回OS起動時に自動起動メニュー
- exeでビルド & 配布する

## 謝辞・出典

本プロジェクトは、以下のプロジェクトのアイデアとアルゴリズムを参考に Rust で再実装したものです。

- [alt-ime-ahk](https://github.com/karakaram/alt-ime-ahk) — [karakaram](https://github.com/karakaram) 氏による、左右 Alt キー空打ちでの IME 切替のオリジナル実装（AutoHotkey 版）。本プロジェクトの設計の土台となっています。
- **IME.ahk**（eamat 氏）— alt-ime-ahk が利用している IME 制御ライブラリ。IME の ON/OFF 切替アルゴリズムの出典です。

## ライセンス

MIT
