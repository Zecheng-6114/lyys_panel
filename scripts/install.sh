#!/usr/bin/env bash
#
# LYYS Panel 一键安装脚本（需 systemd 环境）。
#
# 功能：下载 release 二进制并做 sha256 校验 -> 创建系统用户与目录 -> 写入
# env 模板与 systemd 单元 -> 安装 lyys-panel-ctl 管理命令 -> 启动服务。
# 默认启用 HTTPS（自签证书），监听 3789 端口。
#
# 用法：
#   sudo bash install.sh [选项]
# 在线一键（以 root 执行）：
#   curl -fsSL https://raw.githubusercontent.com/Zecheng-6114/lyys_panel/main/scripts/install.sh \
#     | sudo bash -s -- [选项]
#
# 选项：
#   --version TAG        安装指定 release 标签（默认最新），如 --version v1.10.0
#   --addr ADDR          监听地址（默认 0.0.0.0:3789）
#   --port PORT          只改端口，等价于 --addr 0.0.0.0:PORT
#   --no-tls             关闭 HTTPS，使用纯 HTTP（默认自签证书 HTTPS）
#   --admin-password PWD 首次安装时设置管理员密码（仅首次启动创建账号时生效）
#   --package PATH       用本地二进制安装（离线安装，跳过下载与校验）
#   --no-checksum        跳过 sha256 校验（不推荐，仍保留体积下限检查）
#   -h | --help          显示本帮助
#
# 环境变量（与同名选项等价，选项优先，便于非交互批量部署）：
#   PANEL_VERSION / PANEL_ADDR / PANEL_TLS / PANEL_ADMIN_PASSWORD /
#   PANEL_INSTALL_DIR / PANEL_CONF_DIR / PANEL_DATA_DIR / PANEL_PACKAGE
#
# 重复执行即升级：会覆盖二进制与 systemd 单元，但绝不改动已有的 panel.env。
set -euo pipefail

REPO="${PANEL_REPO:-Zecheng-6114/lyys_panel}"
INSTALL_DIR="${PANEL_INSTALL_DIR:-/opt/lyys-panel}"
CONF_DIR="${PANEL_CONF_DIR:-/etc/lyys-panel}"
DATA_DIR="${PANEL_DATA_DIR:-/var/lib/lyys-panel}"
SERVICE_NAME="lyys-panel"
SERVICE_USER="lyys-panel"
CTL_PATH="/usr/local/bin/lyys-panel-ctl"

VERSION="${PANEL_VERSION:-}"
PANEL_ADDR="${PANEL_ADDR:-0.0.0.0:3789}"
PANEL_TLS="${PANEL_TLS:-auto}"
ADMIN_PASSWORD="${PANEL_ADMIN_PASSWORD:-}"
LOCAL_PACKAGE="${PANEL_PACKAGE:-}"
VERIFY_CHECKSUM=1

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m错误:\033[0m %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
LYYS Panel 一键安装脚本（需 systemd 环境）

用法：
  sudo bash install.sh [选项]
  curl -fsSL <仓库地址>/scripts/install.sh | sudo bash -s -- [选项]

选项：
  --version TAG        安装指定 release 标签（默认最新），如 --version v1.10.0
  --addr ADDR          监听地址（默认 0.0.0.0:3789）
  --port PORT          只改端口，等价于 --addr 0.0.0.0:PORT
  --no-tls             关闭 HTTPS，使用纯 HTTP（默认自签证书 HTTPS）
  --admin-password PWD 首次安装时设置管理员密码（仅首次启动创建账号时生效）
  --package PATH       用本地二进制安装（离线安装，跳过下载与校验）
  --no-checksum        跳过 sha256 校验（不推荐，仍保留体积下限检查）
  -h | --help          显示本帮助

环境变量（与同名选项等价，选项优先）：
  PANEL_VERSION PANEL_ADDR PANEL_TLS PANEL_ADMIN_PASSWORD
  PANEL_INSTALL_DIR PANEL_CONF_DIR PANEL_DATA_DIR PANEL_PACKAGE

说明：
  * 默认 HTTPS 自签证书，端口 3789；--no-tls 时退化为纯 HTTP。
  * 重复执行即升级：覆盖二进制与 systemd 单元，不改动已存在的 panel.env。
  * 未设置管理员密码时首启生成随机密码，见数据目录 initial_admin_password.txt。
  * 安装后可用的管理命令：lyys-panel-ctl help
USAGE
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)        VERSION="${2:?--version 缺少参数值}"; shift 2 ;;
    --addr)           PANEL_ADDR="${2:?--addr 缺少参数值}"; shift 2 ;;
    --port)           PANEL_ADDR="0.0.0.0:${2:?--port 缺少参数值}"; shift 2 ;;
    --no-tls)         PANEL_TLS="off"; shift ;;
    --admin-password) ADMIN_PASSWORD="${2:?--admin-password 缺少参数值}"; shift 2 ;;
    --package)        LOCAL_PACKAGE="${2:?--package 缺少参数值}"; shift 2 ;;
    --no-checksum)    VERIFY_CHECKSUM=0; shift ;;
    -h|--help)        usage ;;
    *) die "未知参数：$1（--help 查看用法）" ;;
  esac
done

# ---------- 前置检查 ----------
[ "$(id -u)" -eq 0 ] || die "请以 root 运行（sudo bash install.sh）"
command -v systemctl >/dev/null 2>&1 || die "未检测到 systemd，本脚本仅支持 systemd 发行版"
if command -v curl >/dev/null 2>&1; then
  # -f 让 HTTP 错误码直接失败，避免把错误页当成二进制装下去
  fetch() { curl -fsSL --connect-timeout 15 "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  die "需要 curl 或 wget 之一下载 release 产物"
fi

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

# 大文件单连接容易中途断，失败重试 3 次再判定为失败
fetch_retry() {
  local url="$1" out="$2" attempt=1
  while :; do
    if fetch "$url" "$out"; then return 0; fi
    if [ "$attempt" -ge 3 ]; then return 1; fi
    warn "下载失败，重试 ${attempt}/3：${url}"
    sleep 2
    attempt=$((attempt + 1))
  done
}

# 校验下载产物的 sha256；校验文件不可得或系统缺工具时降级为体积检查，不阻断安装
verify_checksum() {
  local sum_url="$1" file="$2" sum_file="$TMP_DIR/checksum.txt"
  local expected actual
  if ! fetch_retry "$sum_url" "$sum_file"; then
    warn "无法获取校验文件 ${sum_url}，跳过 sha256 校验（仅做体积检查）"
    return 0
  fi
  expected="$(awk 'NR==1{print $1}' "$sum_file" | tr 'A-F' 'a-f')"
  case "$expected" in
    ""|*[!0-9a-f]*) warn "校验文件内容无法解析，跳过 sha256 校验"; return 0 ;;
  esac
  [ "${#expected}" -eq 64 ] || { warn "校验值长度异常，跳过 sha256 校验"; return 0; }
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
  else
    warn "系统缺少 sha256sum / shasum，跳过 sha256 校验"
    return 0
  fi
  [ "$actual" = "$expected" ] || die "sha256 校验失败：期望 ${expected}，实际 ${actual}，产物可能被篡改或损坏，已中止"
  log "sha256 校验通过"
}

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)  ASSET="lyys-panel" ;;
  aarch64|arm64) ASSET="lyys-panel-aarch64" ;;
  *) die "不支持的架构：$ARCH（release 仅提供 x86_64 / aarch64 产物）" ;;
esac

TMP_BIN="$TMP_DIR/$ASSET"

# ---------- 解析版本 / 取得二进制 ----------
if [ -n "$LOCAL_PACKAGE" ]; then
  [ -f "$LOCAL_PACKAGE" ] || die "本地二进制不存在：${LOCAL_PACKAGE}"
  log "离线安装：使用本地二进制 ${LOCAL_PACKAGE}"
  cp -- "$LOCAL_PACKAGE" "$TMP_BIN" || die "复制本地二进制失败：${LOCAL_PACKAGE}"
else
  if [ -z "$VERSION" ]; then
    log "查询最新 release 版本"
    # 404 = 仓库还没有任何 release，直接从 main 分支不可行（需 Node 构建），
    # 提示用户先在 GitHub 上发布或用 --version 指定。
    API_JSON="$TMP_DIR/latest.json"
    fetch_retry "https://api.github.com/repos/${REPO}/releases/latest" "$API_JSON" \
      || die "查询最新 release 失败：仓库尚无 release 或网络不可达，可先手动构建后用 --version 指定"
    VERSION="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$API_JSON" | head -n1 || true)"
    [ -n "$VERSION" ] || die "未能从 GitHub API 解析出版本号"
  fi
  log "安装版本：${VERSION}（架构 ${ARCH}，资产 ${ASSET}）"

  BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
  log "下载 ${BASE_URL}/${ASSET}"
  fetch_retry "${BASE_URL}/${ASSET}" "$TMP_BIN" || die "下载失败：请检查网络或版本号（${VERSION}）"

  if [ "$VERIFY_CHECKSUM" -eq 1 ]; then
    verify_checksum "${BASE_URL}/${ASSET}.sha256" "$TMP_BIN"
  fi
fi

# 体积下限校验：release 产物约 60MB，小于 1MB 必是错误页/占位文件
SIZE="$(stat -c %s "$TMP_BIN" 2>/dev/null || stat -f %z "$TMP_BIN")"
[ "$SIZE" -ge 1048576 ] || die "产物仅 ${SIZE} 字节，疑似错误页，中止"

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

install -m 0755 "$TMP_BIN" "${INSTALL_DIR}/lyys-panel"
log "二进制已安装到 ${INSTALL_DIR}/lyys-panel"

# ---------- env 模板（已存在则保留，绝不覆盖） ----------
ENV_FILE="${CONF_DIR}/panel.env"
if [ -f "$ENV_FILE" ]; then
  log "检测到已有配置 ${ENV_FILE}，保留不覆盖"
  if [ -n "$ADMIN_PASSWORD" ]; then
    warn "--admin-password 未生效：配置已存在。如需改密码，编辑 ${ENV_FILE} 的 PANEL_ADMIN_PASSWORD 后重启服务"
  fi
else
  log "生成环境配置模板 ${ENV_FILE}"
  if [ -n "$ADMIN_PASSWORD" ]; then
    PW_LINE="PANEL_ADMIN_PASSWORD=${ADMIN_PASSWORD}"
  else
    PW_LINE="#PANEL_ADMIN_PASSWORD=change-me-now"
  fi
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
${PW_LINE}
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

# ---------- 管理命令（内嵌生成，避免安装时再联网拉取） ----------
install_ctl() {
  {
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# LYYS Panel 服务管理命令（由 install.sh 生成，重新安装会覆盖，请勿手改）。'
    printf '%s\n' '# 卸载请使用项目里的 scripts/uninstall.sh。'
    printf 'SERVICE_NAME=%q\n' "$SERVICE_NAME"
    printf 'ENV_FILE=%q\n' "$ENV_FILE"
    printf 'DATA_DIR=%q\n' "$DATA_DIR"
    cat <<'CTL_BODY'
set -euo pipefail

usage() {
  cat <<'USAGE'
lyys-panel-ctl — LYYS Server Panel 服务管理

用法：lyys-panel-ctl <命令>

命令：
  status        查看服务状态与最近日志
  start         启动服务
  stop          停止服务
  restart       重启服务
  logs [-f]     查看服务日志（-f 跟随输出，默认末 200 行）
  user-info     显示访问地址、账号与初始密码文件位置
  config        显示当前环境配置（密码字段以 ****** 打码）
  help          显示本帮助

卸载：使用项目里的 scripts/uninstall.sh（见 README）
USAGE
}

need_root() {
  [ "$(id -u)" -eq 0 ] || { echo "请以 root 运行：sudo lyys-panel-ctl $*" >&2; exit 1; }
}

run_systemctl() {
  need_root "$1"
  if systemctl "$1" "$SERVICE_NAME"; then
    echo "已${2} ${SERVICE_NAME}"
  else
    echo "${2}失败，可查看日志：lyys-panel-ctl logs" >&2
    exit 1
  fi
}

user_info() {
  local addr host port scheme pw_file
  addr="$(sed -n 's/^PANEL_ADDR=//p' "$ENV_FILE" 2>/dev/null | tail -n1 || true)"
  [ -n "$addr" ] || addr="0.0.0.0:3789"
  port="${addr##*:}"
  host="${addr%:*}"
  case "$host" in
    0.0.0.0|"") host="$(hostname -I 2>/dev/null | awk '{print $1}' || true)" ;;
  esac
  scheme=https
  if grep -q '^PANEL_TLS=off' "$ENV_FILE" 2>/dev/null; then scheme=http; fi
  printf '面板地址 : %s://%s:%s\n' "$scheme" "${host:-<服务器IP>}" "$port"
  printf '面板账号 : admin\n'
  pw_file="$DATA_DIR/initial_admin_password.txt"
  if [ -f "$pw_file" ]; then
    printf '初始密码 : 尚未首次登录，执行 cat %s 查看\n' "$pw_file"
  else
    printf '初始密码 : 该文件不存在（未生成，或首次登录后已被自动删除）\n'
  fi
}

case "${1:-help}" in
  status)   systemctl status "$SERVICE_NAME" --no-pager ;;
  start)    run_systemctl start 启动 ;;
  stop)     run_systemctl stop 停止 ;;
  restart)  run_systemctl restart 重启 ;;
  logs)     need_root logs; shift; journalctl -u "$SERVICE_NAME" --no-pager -n 200 "$@" ;;
  user-info) need_root user-info; user_info ;;
  config)   need_root config; sed 's/^\(PANEL_ADMIN_PASSWORD=\).*/\1******/' "$ENV_FILE" ;;
  help|-h|--help) usage ;;
  *) echo "未知命令：$1（lyys-panel-ctl help 查看用法）" >&2; exit 1 ;;
esac
CTL_BODY
  } > "$CTL_PATH"
  chmod 0755 "$CTL_PATH"
  log "管理命令已安装到 ${CTL_PATH}"
}

install_ctl

systemctl daemon-reload
systemctl enable --now "$SERVICE_NAME"

# ---------- 完成提示 ----------
HOST_IP="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
SCHEME="https"; [ "$PANEL_TLS" = "off" ] && SCHEME="http"
PORT="${PANEL_ADDR##*:}"

FW_HINT=""
if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
  FW_HINT="  * 检测到 firewalld 正在运行，若面板无法访问请放行端口：
      firewall-cmd --permanent --add-port=${PORT}/tcp && firewall-cmd --reload"
elif command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q '^Status: active'; then
  FW_HINT="  * 检测到 ufw 已启用，若面板无法访问请放行端口：
      ufw allow ${PORT}/tcp"
fi

log "安装完成！"
cat <<EOF

  服务状态 : systemctl status ${SERVICE_NAME}
  管理命令 : lyys-panel-ctl help        # status / restart / logs / user-info
  访问地址 : ${SCHEME}://${HOST_IP:-<服务器IP>}:${PORT}
  默认账号 : admin

  提示：
  * HTTPS 使用首启自动生成的自签证书，浏览器会提示不受信任，属正常现象；
    正式证书请设置 PANEL_TLS=custom 并指定 PANEL_TLS_CERT / PANEL_TLS_KEY。
  * 未设置 PANEL_ADMIN_PASSWORD 时，首启会把随机初始密码写入
      ${DATA_DIR}/initial_admin_password.txt（权限 0600，首次登录后自动删除），
    查看方式：cat ${DATA_DIR}/initial_admin_password.txt
    首次登录后请立即修改密码。
  * 重新执行本脚本即为升级，不会改动已有配置与数据。
${FW_HINT}
EOF
