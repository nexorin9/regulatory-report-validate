#!/usr/bin/env bash
# 打院内离线安装包：把「引擎 + 宏 + 口径规则 + 样例 + 安装说明」打成一份可拷贝的归档。
#
# 用法：
#   ./scripts/package-offline.sh                    只打运行时包（二进制 + 宏 + 规则 + 样例 + 说明）
#   ./scripts/package-offline.sh --with-source      连源码与依赖目录一起打（专机上现场编译用）
#   ./scripts/package-offline.sh --out-dir /tmp/out 指定输出目录（默认 dist/）
#
# 产物：dist/regval-offline-<日期>-<平台>.tar.gz + 同名 .sha256
#
# 包里**不含任何凭据**：.env 不进包，只带 .env.example。

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"

WITH_SOURCE=0
OUT_DIR="dist"
PROFILE="release"

while [ $# -gt 0 ]; do
  case "$1" in
    --with-source) WITH_SOURCE=1 ;;
    --out-dir)     OUT_DIR="$2"; shift ;;
    --debug)       PROFILE="debug" ;;
    -h|--help)     sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "不认识的参数：$1" >&2; exit 2 ;;
  esac
  shift
done

if ! command -v cargo >/dev/null 2>&1 && [ -x "$HOME/.cargo/bin/cargo" ]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

# 校验和工具：Linux 常见 sha256sum，macOS 是 shasum
if command -v sha256sum >/dev/null 2>&1; then
  SHA_TOOL="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
  SHA_TOOL="shasum -a 256"
else
  echo "[缺失] 找不到 sha256sum 或 shasum，无法生成校验和" >&2
  exit 1
fi

# ---- 构建引擎二进制 ----
BIN_SRC="target/$PROFILE/regval"
[ -x "$BIN_SRC" ] || BIN_SRC="target/$PROFILE/regval.exe"
if [ ! -x "$BIN_SRC" ]; then
  echo "== 构建引擎二进制（${PROFILE}）=="
  cargo build --profile "$PROFILE"
fi
[ -x "$BIN_SRC" ] || { echo "[缺失] 构建后仍找不到 $BIN_SRC" >&2; exit 1; }

STAMP="$(date +%Y%m%d)"
PLAT="$(uname -s | tr '[:upper:]' '[:lower:]')"
PKG_NAME="regval-offline-${STAMP}-${PLAT}"
STAGE="$(mktemp -d)"
DEST="$STAGE/$PKG_NAME"

cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT

echo "== 暂存交付内容 =="
mkdir -p "$DEST"
cp "$BIN_SRC" "$DEST/regval"
chmod +x "$DEST/regval"
cp -R macro "$DEST/macro"
cp -R rules "$DEST/rules"
cp -R samples/dataset "$DEST/samples-dataset"
mkdir -p "$DEST/docs"
cp docs/install.md "$DEST/docs/install.md"
cp .env.example "$DEST/.env.example"
cp scripts/offline-build.sh "$DEST/scripts-offline-build.sh" 2>/dev/null || true

if [ "$WITH_SOURCE" -eq 1 ]; then
  echo "== 追加源码与依赖目录（专机现场编译用）=="
  cp -R src "$DEST/src"
  cp Cargo.toml Cargo.lock "$DEST/"
  mkdir -p "$DEST/scripts"
  cp scripts/offline-build.sh "$DEST/scripts/offline-build.sh"
  if [ -d vendor ]; then
    cp -R vendor "$DEST/vendor"
    echo "依赖目录已包含：$(find vendor -maxdepth 1 -mindepth 1 -type d | wc -l | tr -d ' ') 个 crate"
  else
    echo "[提示] vendor/ 不存在，未包含依赖目录（专机上现场编译需要它）" >&2
  fi
fi

cat > "$DEST/INSTALL.txt" <<'EOF'
院内离线安装包 —— 装之前先读 docs/install.md

三步：
  1. 把 regval 放到 C:\Program Files\RegVal\regval.exe（或改宏顶部的引擎路径）
  2. 把 rules/ 放到 C:\Program Files\RegVal\rules\，并设 REGVAL_RULE_DIR 指向它
  3. 复制 .env.example 为 .env 填院内地址 → 设 REGVAL_STATE_DB → 跑 regval doctor

最小验证（2 分钟）：
  regval validate --input samples-dataset/合并表.csv --out result.json
EOF

echo "== 生成校验和 =="
# 校验和的路径要相对**包内目录**，这样解包后 `cd 包名 && sha256sum -c SHA256SUMS` 才对得上
( cd "$DEST" && find . -type f ! -name 'SHA256SUMS' -print0 \
    | sort -z | xargs -0 "$SHA_TOOL" > SHA256SUMS )

echo "== 打包 =="
mkdir -p "$OUT_DIR"
ARCHIVE="$OUT_DIR/$PKG_NAME.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGE" "$PKG_NAME"
( cd "$OUT_DIR" && "$SHA_TOOL" "$(basename "$ARCHIVE")" > "$(basename "$ARCHIVE").sha256" )

SIZE="$(du -h "$ARCHIVE" | cut -f1)"
echo "安装包：${ARCHIVE}（${SIZE}）"
echo "校验和：$ARCHIVE.sha256"
echo
echo "拷到专机后："
echo "  tar -xzf $PKG_NAME.tar.gz && cd $PKG_NAME"
echo "  sha256sum -c SHA256SUMS          # 逐文件核对"
echo "  chmod +x regval scripts/offline-build.sh"
