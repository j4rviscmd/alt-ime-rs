#!/usr/bin/env bash
# alt-ime-rs リリーススクリプト
#
# 以下を行う:
#   1. 当日日付(yyyy.mm.dd)でバージョンを生成
#      ※当日に既にリリース済みの場合は ".N" のsuffixを付与
#   2. release ビルドで exe を生成
#   3. コミット履歴からリリースノート(変更内容)を生成
#   4. GitHub Release を作成し exe をアセットとして配布
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

# バージョンをバイナリへ焼き込む(アップデート確認機能が現在版として使用)
# Why: 焼き込まないと exe は常に Cargo.toml の 0.1.0 を報告し、最新版との比較が成立しないため。
export ALT_IME_VERSION="${VERSION}"

# release ビルド
cargo build --release

# 配布用にリネームしてコピー
cp "target/release/alt-ime-rs.exe" "${ASSET}"

# リリースノート本文を生成(定型文 + 前回リリース以降のコミット一覧)
# Why: 変更内容(feat=追加/fix=修正)を自動で列挙し、手書きノートの作成漏れを防ぐため。
#   対象はConventional Commitsのsubjectのみ。コミット本文の詳細は載せない(冗長のため)。
# Note: docs:/chore: のコミットは載せない。定型文がexe利用者向けなので意図的な除外であり、
#   抜け漏れではない(git log に "docs: 公開に向けたドキュメント整備" 等の実績あり)。
# Why: gh release create はリモートにのみタグを作成しローカルには反映されないため、
#   fetchしないと PREV_TAG が前々回のタグになり、リリース済みコミットをノートへ再掲する。
git fetch --tags --quiet
PREV_TAG="$(git describe --tags --abbrev=0 2>/dev/null || true)"

NOTES="alt-ime-rs ${TAG}

Windows 11 向けビルド。ダウンロードして実行してください。
左右のAltキーの空打ちでIMEを切り替えます。"

if [ -n "${PREV_TAG}" ]; then
    # git describe --abbrev=0: HEADから到達可能な直近のタグ(初回リリース時は空)
    # 正規表現は feat/fix + 任意のスコープ + 任意の! + ':' に厳密一致させる
    # Why: 緩い "feat[^:]*:" だと "featuring:"/"fixed:" 等の非Conventionalなsubjectまで誤分類するため。
    ADDED="$(git log "${PREV_TAG}"..HEAD --format='%s' | sed -En 's/^feat(\([^:]*\))?!?: ?/- /p')"
    FIXED="$(git log "${PREV_TAG}"..HEAD --format='%s' | sed -En 's/^fix(\([^:]*\))?!?: ?/- /p')"
    if [ -n "${ADDED}" ] || [ -n "${FIXED}" ]; then
        NOTES="${NOTES}

## 変更内容"
        if [ -n "${ADDED}" ]; then
            NOTES="${NOTES}

### 追加
${ADDED}"
        fi
        if [ -n "${FIXED}" ]; then
            NOTES="${NOTES}

### 修正
${FIXED}"
        fi
    fi
fi

# GitHub Release を作成しアセットをアップロード
gh release create "${TAG}" "${ASSET}" \
    --title "${TAG}" \
    --notes "${NOTES}"

# 配布用 exe を削除(リポジトリを汚さないため)
rm -f "${ASSET}"

echo ""
echo "リリース完了: ${TAG}"
echo "  アセット: ${ASSET}"
