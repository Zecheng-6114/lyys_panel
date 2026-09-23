#!/usr/bin/env bash
# LYYS Panel 一键构建：先构建前端，再编译后端。
#
# 顺序不可颠倒 —— 后端用 rust-embed 在编译期读取 frontend/dist，
# 该目录不存在会直接编译失败。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

command -v npm >/dev/null 2>&1 || { echo "缺少 npm，请先安装 Node.js 20+" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "缺少 cargo，请先安装 Rust（https://rustup.rs）" >&2; exit 1; }

echo "==> 构建前端"
cd "$ROOT/frontend"
# 用 npm install 而非 npm ci：ci 会先清空 node_modules 再全量重装，
# 在已有依赖的开发机上纯属浪费；更麻烦的是中途中断会留下「目录在、
# 文件不全」的半损坏状态，此时 npm install 会误判为最新而跳过。
# 需要严格按 lockfile 复现时，手动跑 npm ci（并确保不会中断）。
if [ ! -d node_modules ]; then
  echo "    安装依赖..."
  npm install
fi
npm run build

echo "==> 编译后端"
cd "$ROOT/backend"
cargo build --release

echo
echo "构建完成：$ROOT/backend/target/release/lyys-panel"
