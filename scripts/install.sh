#!/usr/bin/env bash
#
# LYYS Panel 一键安装脚本（Debian / Arch 系，需 systemd 环境）。
#
# 功能：下载 release 二进制 -> 创建系统用户与目录 -> 写入 env 模板与
# systemd 单元 -> 启动服务。默认启用 HTTPS（自签证书），监听 3789 端口。
#
# 用法：
#   sudo bash install.sh [选项]
# 在线一键（以 root 执行）：
#   curl -fsSL https://raw.githubusercontent.com/Zecheng-6114/lyys_panel/main/scripts/install.sh \
#     | sudo bash -s -- [选项]
#
# 选项：
#   --version TAG   安装指定 release 标签（默认最新），如 --version v1.0.0
#   --addr ADDR     监听地址（默认 0.0.0.0:3789）
#   --no-tls        关闭 HTTPS，使用纯 HTTP（默认自签证书 HTTPS）
#   -h | --help     显示本帮助
#
# 重复执行即升级：会覆盖二进制与 systemd 单元，但绝不改动已有的 panel.env。
set -euo pipefail

REPO="Zecheng-6114/lyys_panel"
INSTALL_DIR="/opt/lyys-panel"
CONF_DIR="/etc/lyys-panel"
DATA_DIR="/var/lib/lyys-panel"
SERVICE_NAME="lyys-panel"
SERVICE_USER="lyys-panel"

VERSION=""
PANEL_ADDR="0.0.0.0:3789"
PANEL_TLS="auto"

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m错误:\033[0m %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,25p' "$0"; exit 0; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="${2:?缺少参数值}"; shift 2 ;;
    --addr)    PANEL_ADDR="${2:?缺少参数值}"; shift 2 ;;
    --no-tls)  PANEL_TLS="off"; shift ;;
    -h|--help) usage ;;
    *) die "未知参数：$1（--help 查看用法）" ;;
  esac
done

# ---------- 前置检查 ----------
[ "$(id -u)" -eq 0 ] || die "请以 root 运行（sudo bash install.sh）"
command -v systemctl >/dev/null 2>&1 || die "未检测到 systemd，本脚本仅支持 systemd 发行版"
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  die "需要 curl 或 wget 之一下载 release 产物"
fi

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)  ASSET="lyys-panel" ;;
  aarch64) ASSET="lyys-panel-aarch64" ;;
  *) die "不支持的架构：$ARCH（仅支持 x86_64 / aarch64）" ;;
esac

# ---------- 解析版本 ----------
if [ -z "$VERSION" ]; then
  log "查询最新 release 版本"
  # 404 = 仓库还没有任何 release，直接从 main 分支不可行（需 Node 构建），
  # 提示用户先在 GitHub 上发布或用 --version 指定。
  API_JSON="$(mktemp)"
  fetch "https://api.github.com/repos/${REPO}/releases/latest" "$API_JSON" \
    || die "查询最新 release 失败：仓库尚无 release 或网络不可达，可先手动构建后用 --version 指定"
  VERSION="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$API_JSON" | head -n1)"
  rm -f "$API_JSON"
  [ -n "$VERSION" ] || die "未能从 GitHub API 解析出版本号"
fi
log "安装版本：${VERSION}（架构 ${ARCH}，资产 ${ASSET}）"

# ---------- 创建用户与目录 ----------
if id "$SERVICE_USER" >/dev/null 2>&1; then
  log "系统用户 ${SERVICE_USER} 已存在，跳过创建"
else
  log "创建系统用户 ${SERVICE_USER}（不可登录，用于归属目录）"
  useradd --system --user-group --home-dir "$DATA_DIR" \
    --shell /usr/sbin/nologin "$SERVICE_USER" 2>/dev/null \
    || useradd --system --home-dir "$DATA_DIR" --shell /sbin/nologin "$SERVICE_USER"
fi

mkdir -p "$INSTALL_DIR"
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$CONF_DIR"
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"

# ---------- 下载并安装二进制 ----------
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
TMP_BIN="$(mktemp)"
log "下载 ${URL}"
fetch "$URL" "$TMP_BIN" || die "下载失败：请检查网络或版本号（${VERSION}）"
# 体积下限校验：release 产物约 10MB+，小于 1MB 必是错误页/占位文件
SIZE="$(stat -c %s "$TMP_BIN" 2>/dev/null || stat -f %z "$TMP_BIN")"
[ "$SIZE" -ge 1048576 ] || { rm -f "$TMP_BIN"; die "下载产物仅 ${SIZE} 字节，疑似错误页，中止"; }
install -m 0755 "$TMP_BIN" "${INSTALL_DIR}/lyys-panel"
rm -f "$TMP_BIN"
log "二进制已安装到 ${INSTALL_DIR}/lyys-panel"

# ---------- env 模板（已存在则保留，绝不覆盖） ----------
ENV_FILE="${CONF_DIR}/panel.env"
if [ -f "$ENV_FILE" ]; then
  log "检测到已有配置 ${ENV_FILE}，保留不覆盖"
else
  log "生成环境配置模板 ${ENV_FILE}"
  cat > "$ENV_FILE" <<EOF
# LYYS Panel 环境配置（systemd EnvironmentFile，权限 0600）
# 监听地址：面板默认提供 HTTPS（自签证书）
PANEL_ADDR=${PANEL_ADDR}
# SQLite 数据库与数据目录
PANEL_DB=${DATA_DIR}/panel.db
PANEL_DATA_DIR=${DATA_DIR}
# TLS 模式：auto=首启自动生成自签证书（默认）；custom=搭配
# PANEL_TLS_CERT / PANEL_TLS_KEY 使用已有证书；off=纯 HTTP
PANEL_TLS=${PANEL_TLS}
# 可选：额外监听一个纯 HTTP 端口并 301 跳转到 HTTPS
#PANEL_HTTP_PORT=80

# 首次启动创建 admin 账号时使用的密码（仅第一次生效）。
# 强烈建议：启动前取消注释并改成强密码；留空则生成随机密码写入
# 数据目录 initial_admin_password.txt（0600，首次登录后自动删除）。
#PANEL_ADMIN_PASSWORD=change-me-now
EOF
fi
chmod 0600 "$ENV_FILE"

# ---------- systemd 单元 ----------
log "写入 systemd 单元 /etc/systemd/system/${SERVICE_NAME}.service"
cat > "/etc/systemd/system/${SERVICE_NAME}.service" <<EOF
[Unit]
Description=LYYS Server Panel
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart=${INSTALL_DIR}/lyys-panel
EnvironmentFile=${ENV_FILE}
WorkingDirectory=${INSTALL_DIR}
# 面板需管理系统服务/进程/日志，以 root 运行；数据目录已归属
# ${SERVICE_USER} 用户，如后续支持降权运行，取消下面两行注释即可
#User=${SERVICE_USER}
#Group=${SERVICE_USER}
Restart=on-failure
RestartSec=3
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now "$SERVICE_NAME"

# ---------- 完成提示 ----------
HOST_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
SCHEME="https"; [ "$PANEL_TLS" = "off" ] && SCHEME="http"
log "安装完成！"
cat <<EOF

  服务状态 : systemctl status ${SERVICE_NAME}
  访问地址 : ${SCHEME}://${HOST_IP:-<服务器IP>}:3789
  默认账号 : admin

  提示：
  * HTTPS 使用首启自动生成的自签证书，浏览器会提示不受信任，属正常现象；
    正式证书请设置 PANEL_TLS=custom 并指定 PANEL_TLS_CERT / PANEL_TLS_KEY。
  * 未设置 PANEL_ADMIN_PASSWORD 时，首启会把随机初始密码写入
      ${DATA_DIR}/initial_admin_password.txt（权限 0600，首次登录后自动删除），
    查看方式：cat ${DATA_DIR}/initial_admin_password.txt
    首次登录后请立即修改密码。
EOF
