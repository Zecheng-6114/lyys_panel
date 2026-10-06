//! 交互式终端：浏览器 ↔ 本机 PTY 的**双向** WebSocket 桥。
//!
//! 与 4.3 容器日志流同构（token 走查询串、handler 内做同口径手动鉴权、会话落
//! WS），区别是方向双向：前端 xterm.js 的按键经 **Binary** 帧写入 PTY 主端，
//! PTY 输出经 **Binary** 帧回推；控制帧走 JSON **Text**（当前只有 resize，
//! 用于把前端行列数同步到内核 TIOCSWINSZ）。
//!
//! 为什么不走 cmd.rs：cmd.rs 是「有界、带超时、结果一次性返回」的命令出口，
//! 而 shell 是长生命、双向、无界的交互进程，语义正交（同 container_log_stream
//! 之于 docker logs）。这里仅 openpty / ioctl / fcntl 三处系统调用用 libc，进程
//! 创建仍交给 tokio::process::Command（自带 kill_on_drop 与子进程回收），不手写
//! fork。

#[cfg(unix)]
mod imp {
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::Stdio;
    use std::time::Duration;

    use axum::extract::ws::{Message, WebSocket};
    use serde::Deserialize;
    use tokio::io::unix::AsyncFd;
    use tokio::process::Command;

    /// 单会话最长时长（秒）。终端比日志流用得久，但仍是有限值，防僵尸连接。
    const TERM_MAX_SECS: u64 = 4 * 60 * 60;
    /// 单次读取缓冲（字节）。PTY 输出无固定切分，逐块透传即可。
    const READ_BUF: usize = 8192;

    /// 前端发来的控制帧（Text 帧按 JSON 解析）。用户按键走 Binary 帧，不进这里。
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "lowercase")]
    enum ClientControl {
        Resize { cols: u16, rows: u16 },
    }

    /// 打开 PTY 并把 shell 挂到从端上，返回主端 fd 与子进程句柄。
    fn spawn_shell(cols: u16, rows: u16) -> io::Result<(OwnedFd, tokio::process::Child)> {
        // 优先用登录用户的 $SHELL，缺失时按常见顺序回退（目标平台为 Debian/Arch）
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                ["/bin/bash", "/bin/sh"]
                    .iter()
                    .find(|p| std::path::Path::new(p).exists())
                    .map(|p| (*p).to_string())
            })
            .unwrap_or_else(|| "/bin/sh".to_string());

        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty 成功时向两个 out 指针写入有效 fd；name/termios 传空表示
        // 用系统默认，winp 指向栈上合法的 winsize。
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &ws,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 上面返回的两个 fd 归本函数所有，转成 OwnedFd 以便自动关闭。
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let slave_fd = slave.as_raw_fd();

        let mut cmd = Command::new(&shell);
        cmd.kill_on_drop(true)
            .env("TERM", "xterm-256color")
            // 先把 0/1/2 兜底成 /dev/null：即便 pre_exec 中途失败，也不会把面板
            // 自身的标准流泄露给 shell 子进程。
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: pre_exec 在 fork 之后、exec 之前运行，只能调用 async-signal-safe
        // 的系统调用，这里全为 setsid / ioctl / dup2。
        unsafe {
            cmd.pre_exec(move || {
                // 新建会话并接管控制终端：shell 才有作业控制、Ctrl-C 与 SIGWINCH
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                for fd in 0..3 {
                    if libc::dup2(slave_fd, fd) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        // 父进程关掉从端：不关的话 shell 退出后主端读不到 EOF，连接会一直吊着
        drop(slave);
        Ok((master, child))
    }

    /// 把前端的行列数下发到内核（改主端即可，从端尺寸随之生效）。
    fn resize(master_fd: libc::c_int, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows.clamp(1, 1000),
            ws_col: cols.clamp(1, 1000),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master_fd 是本会话持有的 PTY 主端，ws 是栈上合法的 winsize，
        // 两者在 ioctl 期间均有效。
        let rc = unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ as _, &ws) };
        if rc != 0 {
            tracing::debug!("终端：设置窗口尺寸失败");
        }
    }

    /// 把 PTY 主端置为非阻塞。AsyncFd 依赖 O_NONBLOCK：就绪通知后若仍阻塞读写，
    /// 会把运行时的工作线程整个卡住。
    fn set_nonblocking(fd: libc::c_int) -> io::Result<()> {
        // SAFETY: F_GETFL/F_SETFL 只读写该 fd 的状态标志，参数均为合法常量。
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// 把一段字节全部写进 PTY 主端：非阻塞写，遇 WouldBlock 就等可写再续。
    /// 返回 false 表示设备/连接已不可用，调用方应结束会话。
    async fn write_all(afd: &AsyncFd<std::fs::File>, mut data: &[u8]) -> bool {
        while !data.is_empty() {
            let mut guard = match afd.writable().await {
                Ok(g) => g,
                Err(_) => return false,
            };
            match guard.try_io(|inner| {
                let mut f = inner.get_ref();
                f.write(data)
            }) {
                Ok(Ok(0)) => return false,
                Ok(Ok(n)) => data = &data[n..],
                Ok(Err(_)) => return false,
                Err(_) => {} // WouldBlock：下轮重新等可写
            }
        }
        true
    }

    /// WebSocket ↔ PTY 双向泵。`cols`/`rows` 为前端上报并已夹紧的初始尺寸。
    ///
    /// 主端交给 `AsyncFd`：Linux 的 pty 主端**支持 epoll**（就绪语义正常），
    /// 挂到 tokio 反应堆即可，不占阻塞线程。这里刻意不用 `tokio::fs::File`：
    /// 它内部是 read/write 共享的状态机，`select!` 在对端来帧时会取消挂起的读
    /// future，状态留在 Busy，之后的写便永久 pending —— 实测即「首帧输入之后
    /// 连接僵死」。`readable()/writable()` 是就绪直连 fd 的原语，可安全取消。
    pub async fn bridge(mut socket: WebSocket, cols: u16, rows: u16) {
        let (master, mut child) = match spawn_shell(cols, rows) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("终端：拉起 shell 失败：{e}");
                let _ = socket.send(Message::Text("启动终端失败".into())).await;
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        };
        let master_fd = master.as_raw_fd();
        if let Err(e) = set_nonblocking(master_fd) {
            tracing::warn!("终端：设置非阻塞失败：{e}");
            let _ = child.kill().await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        let afd = match AsyncFd::new(std::fs::File::from(master)) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("终端：注册 PTY 到反应堆失败：{e}");
                let _ = child.kill().await;
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        };

        let deadline = tokio::time::Instant::now() + Duration::from_secs(TERM_MAX_SECS);
        let mut buf = vec![0u8; READ_BUF];
        let mut timed_out = false;
        loop {
            tokio::select! {
                ready = afd.readable() => {
                    let mut guard = match ready {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::debug!("终端：等待可读失败：{e}");
                            break;
                        }
                    };
                    match guard.try_io(|inner| {
                        let mut f = inner.get_ref();
                        f.read(&mut buf)
                    }) {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            // PTY 输出原样透传（Binary 帧），UTF-8 跨块切分交给前端 xterm
                            if socket
                                .send(Message::Binary(buf[..n].to_vec().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        // shell 退出后主端读会返回 EIO，属正常收尾
                        Ok(Err(e)) => {
                            tracing::debug!("终端：PTY 读结束：{e}");
                            break;
                        }
                        // 就绪被其它分支取走（极罕见），下轮重来
                        Err(_) => {}
                    }
                }
                msg = socket.recv() => match msg {
                    Some(Ok(Message::Binary(b))) => {
                        if !write_all(&afd, &b).await {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(ClientControl::Resize { cols, rows }) = serde_json::from_str(&t) {
                            resize(master_fd, cols, rows);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // Ping/Pong 由 axum 自动处理，其余忽略
                    Some(Err(_)) => break,
                },
                _ = tokio::time::sleep_until(deadline) => {
                    timed_out = true;
                    break;
                }
            }
        }

        // 收尾：先杀 shell（kill_on_drop 也会兜底），再关连接
        let _ = child.kill().await;
        let notice = if timed_out {
            "[终端会话已达时长上限，已断开]"
        } else {
            "[终端会话已结束]"
        };
        let _ = socket.send(Message::Text(notice.into())).await;
        let _ = socket.send(Message::Close(None)).await;
    }
}

#[cfg(unix)]
pub use imp::bridge;

/// 非 Unix 平台的占位实现：面板部署目标是 Linux，其它平台直接告知不可用，
/// 只为让仓库在开发机（如 Windows）上仍能通过编译与 clippy。
#[cfg(not(unix))]
pub async fn bridge(mut socket: axum::extract::ws::WebSocket, _cols: u16, _rows: u16) {
    use axum::extract::ws::Message;
    let _ = socket
        .send(Message::Text(
            "当前平台不支持终端功能（面板部署目标为 Linux）".into(),
        ))
        .await;
    let _ = socket.send(Message::Close(None)).await;
}
