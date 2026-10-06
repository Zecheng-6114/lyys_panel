//! 4.3 深度运维：服务 unit 文件查看、SMART 磁盘健康。
//!
//! 设计要点：
//! - 两者都是只读探测，所有外部命令带超时，stderr 只进日志不进响应（P1-3）；
//! - unit 名沿用 opservice::action 的字符白名单，杜绝参数注入与路径穿越；
//! - smartctl 缺失 / lsblk 缺失时不报 5xx，返回 available=false 的降级提示，
//!   前端据此展示说明而非空白。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tokio::process::Command;
use tokio::time::Duration;

/// 单条命令超时（秒）
const CMD_TIMEOUT: u64 = 20;

// ---------- 服务 unit 文件查看 ----------

/// 校验 systemd 单元名（与 opservice::action 同一口径）：
/// 仅允许字母数字与 `. @ - _`，不含路径分隔符，天然防穿越。
pub(crate) fn check_unit_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('.')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '@' | '-' | '_'))
    {
        bail!("非法服务名");
    }
    Ok(())
}

/// 读取服务 unit 文件内容（含 drop-in 覆盖片段，systemctl cat 只读不写）。
pub async fn unit_file(name: &str) -> Result<String> {
    check_unit_name(name)?;
    // P1-1：只读查询，预算 20s
    let mut cmd = Command::new("systemctl");
    cmd.args(["cat", "--no-pager", name]);
    let out = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(CMD_TIMEOUT))
        .await
        .context("调用 systemctl 失败，请确认系统使用 systemd")?;
    if !out.status.success() {
        // P1-3：stderr（可能含绝对路径等细节）只进日志
        tracing::warn!(
            "读取 unit 文件失败，systemctl stderr：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        bail!("读取 unit 文件失败，请确认服务名是否正确");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------- SMART 磁盘健康 ----------

/// 单块磁盘的 SMART 概要
#[derive(Serialize)]
pub struct DiskSmart {
    /// 设备名（如 sda）
    pub device: String,
    /// 型号（拿不到为空）
    pub model: String,
    /// 序列号（拿不到为空）
    pub serial: String,
    /// PASSED / FAILED / UNKNOWN
    pub health: String,
    /// 温度（摄氏度，拿不到为空）
    pub temperature: Option<i64>,
    /// 通电时长（小时，拿不到为空）
    pub powered_on_hours: Option<i64>,
}

/// SMART 报告。available=false 时 disks 为空、message 给出原因。
#[derive(Serialize)]
pub struct SmartReport {
    pub available: bool,
    /// smartctl 缺失等降级原因（available=true 时为空）
    pub message: String,
    pub disks: Vec<DiskSmart>,
}

/// 汇总所有物理磁盘的 SMART 健康状态。
/// 任何一块盘探测失败都不影响其它盘，仅该盘字段回 UNKNOWN。
pub async fn smart_report() -> SmartReport {
    // 1) smartctl 是否可用
    let mut probe = Command::new("smartctl");
    probe.arg("--version");
    let probe = crate::cmd::run(&mut probe, crate::cmd::Budget::query(10)).await;
    let smartctl_ok = matches!(&probe, Ok(o) if o.success());
    if !smartctl_ok {
        tracing::info!("smartctl 不可用，SMART 健康功能按未安装降级");
        return SmartReport {
            available: false,
            message: "未安装 smartctl（可安装 smartmontools 包后刷新）".into(),
            disks: Vec::new(),
        };
    }

    // 2) 枚举物理磁盘（lsblk 只取 type=disk，忽略分区/loop）
    let mut lsblk_cmd = Command::new("lsblk");
    lsblk_cmd.args(["-d", "-n", "-o", "NAME,TYPE"]);
    let lsblk = crate::cmd::run(&mut lsblk_cmd, crate::cmd::Budget::query(CMD_TIMEOUT)).await;
    let Ok(out) = lsblk else {
        tracing::warn!("lsblk 不可用，无法枚举磁盘");
        return SmartReport {
            available: false,
            message: "lsblk 不可用，无法枚举磁盘".into(),
            disks: Vec::new(),
        };
    };
    let disks: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let ty = it.next().unwrap_or("");
            (ty == "disk").then(|| name.trim_start_matches('/').to_string())
        })
        .collect();

    if disks.is_empty() {
        return SmartReport {
            available: true,
            message: "未发现物理磁盘".into(),
            disks: Vec::new(),
        };
    }

    // 3) 逐盘 smartctl --json -H（只取健康概要，不拉全量属性表）
    let mut list = Vec::new();
    for dev in disks {
        list.push(probe_disk(&dev).await);
    }
    SmartReport {
        available: true,
        message: String::new(),
        disks: list,
    }
}

/// 探测单块盘。失败时返回 health=UNKNOWN 的占位行，保证表格行数与盘数一致。
async fn probe_disk(dev: &str) -> DiskSmart {
    let unknown = |device: &str| DiskSmart {
        device: device.to_string(),
        model: String::new(),
        serial: String::new(),
        health: "UNKNOWN".into(),
        temperature: None,
        powered_on_hours: None,
    };
    let mut cmd = Command::new("smartctl");
    cmd.args(["--json", "-H", "-A", &format!("/dev/{dev}")]);
    let Ok(out) = crate::cmd::run(&mut cmd, crate::cmd::Budget::query(CMD_TIMEOUT)).await else {
        tracing::warn!("smartctl 探测 /dev/{dev} 超时或启动失败");
        return unknown(dev);
    };
    if !out.status.success() && out.stdout.is_empty() {
        // 某些盘（如 USB 桥接）smartctl 不支持，退出码非 0 但可能有 JSON 输出
        tracing::warn!("smartctl 探测 /dev/{dev} 失败（退出码非 0）");
        return unknown(dev);
    }
    let v: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("smartctl /dev/{dev} 输出无法按 JSON 解析");
            return unknown(dev);
        }
    };
    // smart_status.passed → PASSED/FAILED；缺字段视为 UNKNOWN
    let health = match v.pointer("/smart_status/passed") {
        Some(serde_json::Value::Bool(true)) => "PASSED".to_string(),
        Some(serde_json::Value::Bool(false)) => "FAILED".to_string(),
        _ => "UNKNOWN".to_string(),
    };
    DiskSmart {
        device: dev.to_string(),
        model: v
            .get("model_name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        serial: v
            .get("serial_number")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        health,
        temperature: v.pointer("/temperature/current").and_then(|x| x.as_i64()),
        powered_on_hours: v.pointer("/power_on_time/hours").and_then(|x| x.as_i64()),
    }
}

// ---------- 4.3 容器日志流（WebSocket） ----------

use std::process::Stdio;

use axum::extract::ws::{Message, WebSocket};
use tokio::io::{AsyncRead, AsyncReadExt};

/// 单行日志 / 单行缓冲上限（字节），防恶意容器输出超长行撑爆内存
const LINE_LIMIT: usize = 64 * 1024;
/// 单次日志会话最长时长（秒），到期主动断开，防僵尸连接
const SESSION_SECS: u64 = 30 * 60;

/// 将一条子进程输出流按行抽干，经 mpsc 转发。
/// 用 8KB 分块读 + 按换行切片，配合 LINE_LIMIT 兜底，
/// 避免无换行超长行导致缓冲无界增长。
async fn pump_lines<R: AsyncRead + Unpin>(mut reader: R, tx: tokio::sync::mpsc::Sender<String>) {
    let mut chunk = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                pending.extend_from_slice(&chunk[..n]);
                while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                    let mut line: Vec<u8> = pending.drain(..=pos).collect();
                    line.pop(); // 去掉 \n
                    if line.last() == Some(&b'\r') {
                        line.pop(); // 去掉 \r
                    }
                    if line.len() > LINE_LIMIT {
                        line.truncate(LINE_LIMIT);
                    }
                    let s = String::from_utf8_lossy(&line).into_owned();
                    if tx.send(s).await.is_err() {
                        return; // 下游已断开，读任务退出
                    }
                }
                if pending.len() > LINE_LIMIT {
                    // 无换行的超长行：截断为一条独立日志后清空缓冲
                    let s = String::from_utf8_lossy(&pending[..LINE_LIMIT]).into_owned();
                    pending.clear();
                    if tx.send(s).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
    // 流结束时把残留半行也吐出去（无尾换行的最后一条日志）；
    // 发送失败即连接已断，直接结束（返回值为 ()，此处用退出循环代替 return）
    if !pending.is_empty()
        && tx
            .send(String::from_utf8_lossy(&pending).into_owned())
            .await
            .is_err()
    {
        tracing::debug!("pump_lines：接收端已断开，残留半行丢弃");
    }
}

/// 容器日志流 WebSocket 泵：
/// `docker logs -f --tail N <id>` 的 stdout/stderr 两路并成一个通道，
/// 每行作为一条 Text 帧推给前端；同时监听客户端 Close 帧。
/// - `kill_on_drop(true)`：连接断开时子进程随之终止，不留孤儿 docker logs；
/// - 单会话最长 SESSION_SECS，到期服务端主动关闭；
/// - 客户端断开 / 双方任意一端出错都立即结束循环。
pub async fn container_log_stream(socket: WebSocket, id: String, tail: usize) {
    let mut socket = socket;
    let child = Command::new("docker")
        .args(["logs", "-f", "--tail", &tail.to_string(), &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("启动 docker logs 失败：{e}");
            let _ = socket
                .send(Message::Text("启动 docker logs 失败".into()))
                .await;
            return;
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
    if let Some(out) = child.stdout.take() {
        let tx1 = tx.clone();
        tokio::spawn(pump_lines(out, tx1));
    }
    if let Some(err) = child.stderr.take() {
        let tx2 = tx.clone();
        tokio::spawn(pump_lines(err, tx2));
    }
    // 主循环只留 rx；Sender 计数归零（两个读任务都退出）时 rx 才返回 None
    drop(tx);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(SESSION_SECS);
    loop {
        tokio::select! {
            line = rx.recv() => match line {
                Some(l) => {
                    if socket.send(Message::Text(l.into())).await.is_err() {
                        break; // 客户端已断开
                    }
                }
                None => {
                    // 两个读任务都退出 → docker logs 进程已结束（如容器停止）
                    let _ = socket
                        .send(Message::Text("[容器日志流结束]".into()))
                        .await;
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // 忽略客户端发来的其它帧
                Some(Err(_)) => break,
            },
            _ = tokio::time::sleep_until(deadline) => {
                let _ = socket
                    .send(Message::Text("[日志流已达单次会话上限，已自动断开]".into()))
                    .await;
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
        }
    }
    // child（kill_on_drop）在此 drop，docker logs 进程被终止
}
