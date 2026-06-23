# alt-ime-rs

Windows向けアプリ。左右の`ALT`キーでIME on/off切り替えを行う。rust製で軽量・高速

このプロジェクトでは <https://github.com/karakaram/alt-ime-ahk> のような機能をrustにて実現して、exe配布を行う

## 要件

- 対象OSはWindowsのみ(動作保証はwin11のみ)
- 左altでime off
  - すでにoffの状態で押下しても何もしない
- 右altでime on
  - すでにonの状態で押下しても何もしない
- alt+tabなど複合キー入力の場合は何もしない
- トレイに常駐
- exeでビルド & 配布する

## 謝辞

- [alt-ime-ahk](https://github.com/karakaram/alt-ime-ahk)

## ライセンス

MIT
