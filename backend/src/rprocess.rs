use anyhow::Result;

use crate::monitor::ProcessInfo;
use crate::AppState;

/// 获取进程列表（按 CPU 降序）
pub async fn list(state: &AppState) -> Result<Vec<ProcessInfo>> {
    let mut m = state.monitor.lock().await;
    Ok(m.processes())
}

/// 结束指定进程
pub async fn kill(state: &AppState, pid: u32) -> Result<bool> {
    let mut m = state.monitor.lock().await;
    Ok(m.kill_process(pid))
}
