#!/usr/bin/env bash
# alt-ime-rs リリーススクリプト
#
# 以下を行う:
#   1. 当日日付(yyyy.mm.dd)でバージョンを生成
#      ※当日に既にリリース済みの場合は ".N" のsuffixを付与
#   2. release ビルドで exe を生成
#   3. GitHub Release を作成し exe をアセットとして配布
#
# 使い方:
#   bash release.sh
#
# 前提:
#   - Rust ツールチェーン (cargo)
#   - GitHub CLI (gh) で認証済みであること

set -euo pipefail

# スクリプト自身のディレクトリ(リポジトリルート)へ移動
cd "$(dirname "$0")"

# 当日のバージョン(例: 2026.06.23)
DATE_VER="$(date +%Y.%m.%d)"
BASE_TAG="v${DATE_VER}"

# 既存のタグ一覧を取得し、当日リリース済みか確認
EXISTING="$(gh api repos/:owner/:repo/tags --paginate --jq '.[].name' 2>/dev/null || true)"

VERSION="${DATE_VER}"
TAG="${BASE_TAG}"
if printf '%s\n' "${EXISTING}" | grep -qx "${BASE_TAG}"; then
    # 当日すでにリリース済み → 未使用のsuffix番号(.1, .2, ...)を決定
    N=1
    while printf '%s\n' "${EXISTING}" | grep -qx "${BASE_TAG}.${N}"; do
        N=$((N + 1))
    done
    VERSION="${DATE_VER}.${N}"
    TAG="${BASE_TAG}.${N}"
fi

ASSET="alt-ime-rs.exe"

echo "リリースバージョン: ${VERSION} (タグ: ${TAG})"

# release ビルド
cargo build --release

# 配布用にリネームしてコピー
cp "target/release/alt-ime-rs.exe" "${ASSET}"

# GitHub Release を作成しアセットをアップロード
gh release create "${TAG}" "${ASSET}" \
    --title "${TAG}" \
    --notes "alt-ime-rs ${TAG}

Windows 11 向けビルド。ダウンロードして実行してください。
左右のAltキーの空打ちでIMEを切り替えます。"

# 配布用 exe を削除(リポジトリを汚さないため)
rm -f "${ASSET}"

echo ""
echo "リリース完了: ${TAG}"
echo "  アセット: ${ASSET}"
