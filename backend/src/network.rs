use anyhow::Context;
use serde_json::Value;
use tokio::process::Command;

/// 执行命令并解析 JSON 输出
async fn json_cmd(program: &str, args: &[&str]) -> anyhow::Result<Value> {
    let out = Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("调用 {program} 失败"))?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显（防内部细节泄露）
        tracing::warn!("{program} stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
        anyhow::bail!("{program} 执行失败，详见服务端日志");
    }
    let v: Value =
        serde_json::from_slice(&out.stdout).with_context(|| format!("解析 {program} 输出失败"))?;
    Ok(v)
}

/// 网络接口（ip -j addr）
pub async fn interfaces() -> anyhow::Result<Value> {
    json_cmd("ip", &["-j", "addr"]).await
}

/// 路由表（ip -j route）
pub async fn routes() -> anyhow::Result<Value> {
    json_cmd("ip", &["-j", "route"]).await
}

/// TCP 连接（解析 ss 文本输出；此版本 ss 不支持 -j）
pub async fn connections() -> anyhow::Result<Vec<Value>> {
    let out = Command::new("ss")
        .args(["-t", "-a", "-n", "-p"])
        .output()
        .await
        .context("调用 ss 失败")?;
    if !out.status.success() {
        // P1-3：命令 stderr 只进日志，响应体不回显
        tracing::warn!("ss stderr：{}", String::from_utf8_lossy(&out.stderr).trim());
        anyhow::bail!("ss 执行失败，详见服务端日志");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();
    for line in text.lines().skip(1) {
        // 列：State Recv-Q Send-Q Local:Port Peer:Port [Process]
        let mut it = line.split_whitespace();
        let (Some(state), Some(recv_q), Some(send_q), Some(local), Some(peer)) = (
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
        ) else {
            continue;
        };
        // 进程信息为剩余部分
        let process = it.collect::<Vec<_>>().join(" ");
        list.push(serde_json::json!({
            "state": state,
            "recv_q": recv_q,
            "send_q": send_q,
            "local": local,
            "peer": peer,
            "process": process,
        }));
    }
    Ok(list)
}

/// DNS 配置（读取 /etc/resolv.conf 中 nameserver 行）
pub async fn dns() -> anyhow::Result<Vec<String>> {
    let text = tokio::fs::read_to_string("/etc/resolv.conf")
        .await
        .context("读取 /etc/resolv.conf 失败")?;
    let servers = text
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("nameserver")
                .map(|s| s.trim().to_string())
        })
        .collect();
    Ok(servers)
}
