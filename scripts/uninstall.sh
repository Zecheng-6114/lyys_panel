#!/usr/bin/env bash
#
# LYYS Panel 卸载脚本。反向执行 install.sh：停止并禁用服务、删除
# systemd 单元、二进制与配置；默认保留数据目录与系统用户。
#
# 用法：
#   sudo bash uninstall.sh [选项]
#
# 选项：
#   --purge      连同数据目录（/var/lib/lyys-panel，含数据库/证书/备份）
#                与系统用户一起删除，请谨慎使用
#   --keep-conf  保留 /etc/lyys-panel 配置目录（默认会删除）
#   -h | --help  显示本帮助
set -euo pipefail

INSTALL_DIR="/opt/lyys-panel"
CONF_DIR="/etc/lyys-panel"
DATA_DIR="/var/lib/lyys-panel"
SERVICE_NAME="lyys-panel"
SERVICE_USER="lyys-panel"

PURGE=0
KEEP_CONF=0

log() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }

usage() { sed -n '2,14p' "$0"; exit 0; }

while [ $# -gt 0 ]; do
  case "$1" in
    --purge)     PURGE=1; shift ;;
    --keep-conf) KEEP_CONF=1; shift ;;
    -h|--help)   usage ;;
    *) printf '未知参数：%s（--help 查看用法）\n' "$1" >&2; exit 1 ;;
  esac
done

[ "$(id -u)" -eq 0 ] || { echo "请以 root 运行（sudo bash uninstall.sh）" >&2; exit 1; }

log "停止并禁用服务 ${SERVICE_NAME}"
systemctl disable --now "$SERVICE_NAME" 2>/dev/null || true

log "删除 systemd 单元"
rm -f "/etc/systemd/system/${SERVICE_NAME}.service"
systemctl daemon-reload 2>/dev/null || true
systemctl reset-failed "$SERVICE_NAME" 2>/dev/null || true

log "删除程序目录 ${INSTALL_DIR}"
rm -rf "$INSTALL_DIR"

if [ "$KEEP_CONF" -eq 1 ]; then
  log "按要求保留配置目录 ${CONF_DIR}"
else
  log "删除配置目录 ${CONF_DIR}"
  rm -rf "$CONF_DIR"
fi

if [ "$PURGE" -eq 1 ]; then
  log "删除数据目录 ${DATA_DIR}（数据库 / 自签证书 / 备份）"
  rm -rf "$DATA_DIR"
  if id "$SERVICE_USER" >/dev/null 2>&1; then
    log "删除系统用户 ${SERVICE_USER}"
    userdel "$SERVICE_USER" 2>/dev/null || true
  fi
else
  log "保留数据目录 ${DATA_DIR} 与系统用户 ${SERVICE_USER}"
  echo "  确认不再需要后可手动删除，或重新运行本脚本加 --purge。"
fi

log "卸载完成"
