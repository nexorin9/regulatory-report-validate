#!/usr/bin/env bash
# 院内离线构建：把依赖从联网机搬到专机，在无外网环境里编出引擎二进制。
#
# 用法：
#   1) 联网机（一次性）：从源码生成依赖目录
#        ./scripts/offline-build.sh --vendor
#      → 生成 vendor/（约 210 个 crate、250 MB 上下），并写出 .cargo/config.toml
#
#   2) 把 vendor/ 与源码一起拷到专机（无外网）：
#        ./scripts/offline-build.sh
#      → 校验依赖目录完整性 → 写 .cargo/config.toml → cargo build --release --offline
#
#   3) 只查依赖目录完整不完整（不编译）：
#        ./scripts/offline-build.sh --check
#
# 其它开关：
#   --no-config   不写 .cargo/config.toml（只做依赖目录校验）
#   --debug       编 debug 版（默认 release）
#
# 说明：.cargo/config.toml 由本脚本生成，**不要提交进仓库** —— 依赖目录不进库，
# 仓库里留一份指向不存在的目录会让所有人 clone 下来就编不动。

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_DIR"

VENDOR_DIR="vendor"
CONFIG_FILE=".cargo/config.toml"
PROFILE="release"

DO_VENDOR=0
DO_CHECK=0
WRITE_CONFIG=1

while [ $# -gt 0 ]; do
  case "$1" in
    --vendor)     DO_VENDOR=1 ;;
    --check)      DO_CHECK=1 ;;
    --no-config)  WRITE_CONFIG=0 ;;
    --debug)      PROFILE="debug" ;;
    -h|--help)    sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "不认识的参数：$1" >&2; exit 2 ;;
  esac
  shift
done

# ---- cargo 工具链：默认 PATH 里常常没有 ~/.cargo/bin ----
if ! command -v cargo >/dev/null 2>&1; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then
    echo "[提示] PATH 里没有 cargo，改用 $HOME/.cargo/bin/cargo"
    export PATH="$HOME/.cargo/bin:$PATH"
  else
    echo "[缺失] 找不到 cargo。装 Rust 工具链（rustup）后重试。" >&2
    exit 1
  fi
fi

# ---- 依赖目录完整性：每个 crate 都要有 .cargo-checksum.json（cargo 自己就靠它校验）----
check_vendor() {
  if [ ! -d "$VENDOR_DIR" ]; then
    echo "[缺失] 依赖目录 $VENDOR_DIR/ 不存在。"
    echo "       联网机上先跑一次：./scripts/offline-build.sh --vendor"
    return 1
  fi
  local total=0 bad=0 name
  for d in "$VENDOR_DIR"/*/; do
    [ -d "$d" ] || continue
    name="$(basename "$d")"
    total=$((total + 1))
    if [ ! -f "${d}.cargo-checksum.json" ] && [ ! -f "$d/.cargo-checksum.json" ]; then
      echo "  [不完整] $name （缺 .cargo-checksum.json）"
      bad=$((bad + 1))
    fi
  done
  echo "依赖目录：$VENDOR_DIR/ 共 $total 个 crate，校验文件齐全 $((total - bad)) 个，不完整 $bad 个"
  if [ "$bad" -ne 0 ]; then
    echo "[缺失] 依赖目录不完整 —— 拷贝时断过，重新拷一次。" >&2
    return 1
  fi
  # Cargo.lock 里声明的包数应当与依赖目录里的 crate 数一致（源码包本身不在 vendor 里）
  local locked
  locked="$(grep -c '^\[\[package\]\]' Cargo.lock || true)"
  echo "Cargo.lock 声明 $locked 个包（含本项目自身 1 个），依赖目录 $total 个 crate"
  return 0
}

if [ "$DO_VENDOR" -eq 1 ]; then
  echo "== 联网机：从源码生成依赖目录 =="
  # cargo vendor 把「怎么用这些依赖」的配置写在 stdout；一并落到 .cargo/config.toml
  cargo vendor --versioned-dirs "$VENDOR_DIR" | tee /tmp/regval-vendor-config.txt
  if [ "$WRITE_CONFIG" -eq 1 ]; then
    mkdir -p .cargo
    cp /tmp/regval-vendor-config.txt "$CONFIG_FILE"
    echo "已写出 $CONFIG_FILE"
  fi
  echo "== 校验依赖目录 =="
  check_vendor
  echo "现在可以把 $VENDOR_DIR/ 与源码一起拷到无外网的专机。"
  exit 0
fi

if [ "$DO_CHECK" -eq 1 ]; then
  echo "== 只做依赖目录完整性校验 =="
  check_vendor
  exit $?
fi

echo "== 专机：离线构建 =="
check_vendor

if [ "$WRITE_CONFIG" -eq 1 ]; then
  mkdir -p .cargo
  cat > "$CONFIG_FILE" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor/"
EOF
  echo "已写出 ${CONFIG_FILE}（依赖来源切到 ${VENDOR_DIR}/）"
fi

echo "== cargo build --offline --profile $PROFILE =="
cargo build --offline --profile "$PROFILE"

BIN="target/$PROFILE/regval"
[ -x "$BIN" ] || BIN="target/$PROFILE/regval.exe"
echo "构建完成：$BIN"
echo "下一步："
echo "  1) cp .env.example .env  并按院内实际地址填写"
echo "  2) ./$BIN doctor            看缺哪几项配置"
echo "  3) ./$BIN validate --input <样例表> --out /tmp/regval-result.json   跑一次最小验证"
