#!/usr/bin/env python3
# /// script
# dependencies = ["pillow>=11"]
# ///
"""alt-ime-rs のトレイアイコン(.ico)を生成する。

Pillow で高解像度(512px)のアイコンを描画し、各サイズへダウンスケールして
複数サイズ内包の ICO として保存する。配色バリエーションのプレビュー PNG も
併せに出力し、採用案を選びやすくする。

使い方:
  uv run scripts/generate_icon.py             # 全バリアントのプレビューPNGを生成
  uv run scripts/generate_icon.py <variant>   # 指定バリアントを assets/icon.ico へ出力

# Why uv: PEP 723 ヘッダ(上部 # /// script)により依存(pillow)を自動解決する。
#   システムPythonを汚さず、pip install 不要で実行できる。
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# リポジトリルート(このスクリプトは scripts/ 配下)
ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
PREVIEW = ROOT / "scripts" / "_preview"

# ICO に内包するサイズ。
# Constraint: トレイ表示は 16/20/24/32px、エクスプローラ等は 48/64/128/256px。
#             両方を覆盖するためこれら全てを内包させる。
SIZES = [16, 20, 24, 32, 48, 64, 128, 256]
# 高解像度で描画してからダウンスケール(アンチエイリアスを効かせる)。
RENDER_SIZE = 512

# Windows 標準の太字フォント(Segoe UI Bold)。存在しない環境向けにフォールバックも用意。
FONT_CANDIDATES = [
    "C:/Windows/Fonts/segoeuib.ttf",  # Segoe UI Bold
    "C:/Windows/Fonts/arialbd.ttf",   # Arial Bold
    "C:/Windows/Fonts/arial.ttf",     # Arial
]


def load_font(size: int) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    for path in FONT_CANDIDATES:
        if Path(path).exists():
            return ImageFont.truetype(path, size)
    # Why: フォントが無い稀な環境でも描画だけは成立させる。見栄えは劣る。
    return ImageFont.load_default()


# バリアント定義: (名前, 背景色RGBA, 文字色RGBA, アクセントバー色RGBA or None)
VARIANTS = [
    # 純黒地 + 白文字。最もコントラストが高くトレイ(16px)でも判読しやすい。
    ("kuro-white", (6, 6, 10, 255), (245, 245, 250, 255), None),
    # 深藍地 + 白文字。黒より柔らかいダーク感。
    ("navy-white", (15, 16, 32, 255), (240, 240, 250, 255), None),
    # 純黒地 + シアン文字。アクセント色で IME ツールらしさ。
    ("kuro-cyan", (6, 6, 10, 255), (0, 212, 255, 255), None),
    # 純黒地 + 白文字 + 下部シアンバー。状態表示の余地を持たせる。
    ("kuro-white-bar", (6, 6, 10, 255), (245, 245, 250, 255), (0, 180, 255, 255)),
]


def render(bg, fg, accent, size: int = RENDER_SIZE) -> Image.Image:
    """1バリアントの高解像度アイコン画像を描画する。"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # 角丸四角の背景
    radius = int(size * 0.22)
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=bg)

    # アクセントバー(下部)。文字を少し上にずらすためのオフセット根拠にもする。
    accent_h = 0
    if accent is not None:
        bar_h = int(size * 0.10)
        margin = int(size * 0.14)
        accent_h = bar_h + margin
        d.rounded_rectangle(
            [int(size * 0.18), size - accent_h, int(size * 0.82), size - margin],
            radius=int(bar_h * 0.45),
            fill=accent,
        )

    # "Alt" 文字を中央寄せ(アクセントバーがある分だけ上へ)
    font = load_font(int(size * 0.46))
    text = "Alt"
    bbox = d.textbbox((0, 0), text, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    tx = (size - tw) / 2 - bbox[0]
    ty = (size - th) / 2 - bbox[1] - accent_h / 2
    d.text((tx, ty), text, font=font, fill=fg)

    return img


def save_ico(big: Image.Image, path: Path) -> None:
    """高解像度画像から複数サイズ内包の ICO を保存する。

    Pillow は sizes= に与えた各サイズへ BICUBIC リサイズして内包する。
    """
    big.save(path, format="ICO", sizes=[(s, s) for s in SIZES])


def main() -> None:
    ASSETS.mkdir(exist_ok=True)
    PREVIEW.mkdir(exist_ok=True)

    target = sys.argv[1] if len(sys.argv) > 1 else None

    for name, bg, fg, accent in VARIANTS:
        big = render(bg, fg, accent)
        # プレビュー用 PNG(256px) を常に出力(採用案の比較用)
        big.resize((256, 256), Image.LANCZOS).save(PREVIEW / f"{name}.png")
        print(f"preview -> {PREVIEW / (name + '.png')}")

        if name == target:
            save_ico(big, ASSETS / "icon.ico")
            print(f"ICO出力 -> {ASSETS / 'icon.ico'} (variant={name})")

    if target and not any(v[0] == target for v in VARIANTS):
        print(f"エラー: 不明なバリアント '{target}'", file=sys.stderr)
        print(f"選択肢: {[v[0] for v in VARIANTS]}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
