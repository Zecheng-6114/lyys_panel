# P2-1 本地作业表与任务流 —— 落地细案

> 依据《LYYS Panel 后端改进落地方案》第四章。这是 P2 三项中唯一具有架构性的改造，
> 方案中已注明「单独出细案评估后再动手」，本文即该细案。P2-2、P2-3 已实施完毕。

## 一、要解决的具体问题

当前所有长操作都绑在 HTTP 请求生命周期内：

| 操作 | 现状 | 具体症状 |
|------|------|---------|
| `packages/action` 安装 / 卸载 / 升级 | `cmd::run` 同步等待，预算 1800s | 前端转圈最长达半小时，中途切页即失联 |
| `docker/install`、`docker pull` | 同上，`run` 预算 900s | 拉取进度完全不可见，只能看最终结果 |
| `backups/restore` | `spawn_blocking` 同步执行 | 恢复期间请求挂起，超时后无法判断是否真的失败 |

三个共性问题：**进度不可见、连接被占、失败原因只在响应体里出现一次**。
作业表要换掉的正是「请求 = 操作生命周期」这个等号。

## 二、设计决策（本细案已定，不再回抛）

1. **存储复用 SQLite**，落 `jobs` 表。不引入 Redis / 消息队列——面板是单机自用，
   进程内 tokio 任务 + 一张表已经覆盖全部需求，多一个外部依赖就多一个部署故障点。
2. **执行体走 `cmd.rs`**。作业只是 `cmd::run` 的调用方，超时预算与 `LockGroup`
   互斥完全沿用 P1-1 的既有语义，不另起一套并发控制。
3. **输出走 SSE**，复用 `ai.rs` 已有的流式响应能力，前端不引入 WebSocket / 轮询。
4. **输出保留最后 200 行**（环形截断），作业结束后仍可回看，但表不会无限增长。
5. **首批只接四类长操作**：`apt-get install/remove/upgrade`、`pacman -S/-R/-Syu`、
   备份创建与恢复、`docker pull`。查询类接口（`list_installed` / `upgradable` /
   `search`）保持同步——它们有 15s 级超时预算，改成作业只会让前端更麻烦。
6. **`cmd.rs` 新增流式入口**，不改造现有 `run`。`run` 仍在同步路径上使用，
   两条路互不影响。

## 三、表结构（迁移 `0011_jobs.sql`）

> 编号必须取 `0011`。迁移表按单调整数 `schema_version` 判定是否执行，任何后补的
> 低版本号迁移都会被静默跳过——所以编号只能顺着当前链尾（`0010`）往后排，
> 不能沿用早期规划里的跳号。

```sql
CREATE TABLE IF NOT EXISTS jobs (
    id          TEXT PRIMARY KEY,      -- uuid v4 十六进制，32 字符
    kind        TEXT NOT NULL,         -- pkg_install / pkg_remove / pkg_upgrade /
                                       -- backup_create / backup_restore / docker_pull
    payload     TEXT NOT NULL DEFAULT '',  -- 提交参数 JSON（包名列表、容器镜像等）
    status      TEXT NOT NULL,         -- pending / running / success / failed / cancelled / interrupted
    exit_code   INTEGER,               -- 进程退出码，未结束为 NULL
    stdout_tail TEXT NOT NULL DEFAULT '',  -- 最后 200 行输出，行间以 \n 连接
    error       TEXT,                  -- 面板自造的错误文案（不回显 stderr 原文）
    created_at  INTEGER NOT NULL,
    started_at  INTEGER,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_jobs_created ON jobs (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_status  ON jobs (status);
```

`interrupted` 是独立状态而非 `failed` 的别名：它表示「面板在作业运行中被重启」，
与「命令真的失败了」是两回事，前端需要区分展示。

## 四、状态机与重启收尾

```
pending ──▶ running ──┬──▶ success
                      ├──▶ failed      （非零退出码 / 执行器报错）
                      ├──▶ cancelled   （用户取消，或超时被杀）
                      └──▶ interrupted （面板重启，见下）
```

**重启收尾是这一项最容易漏的地方**：进程重启后 tokio 任务全部消失，但表里
仍留着 `running` 的行，若不做处理，这些作业会永久停在「运行中」。

处理方式：`Db::open` 之后、`spawn_sampler` 之前执行一次
`UPDATE jobs SET status='interrupted', finished_at=? WHERE status IN ('pending','running')`。
不做「自动续跑」——安装类操作被中断后系统可能处于半完成状态，自动重试比重做更危险，
交给用户在界面上重新提交。

## 五、后端落点

**新增 `backend/src/jobs.rs`**，对外接口：

```rust
pub struct JobHandle { pub id: String }

/// 提交作业：写 pending 行 → 返回 id → spawn 执行体（调用方拿到 id 立即返回）
pub async fn submit(state: &AppState, kind: JobKind, payload: serde_json::Value) -> Result<JobHandle>;

/// 取消：对 running 作业发 kill 信号，状态置 cancelled
pub async fn cancel(state: &AppState, id: &str) -> Result<()>;

/// 增量输出订阅（SSE 用）：先回放当前 stdout_tail，再流式推送新增行
pub fn subscribe(state: &AppState, id: String) -> impl Stream<Item = String>;
```

进程内维护 `HashMap<String, JoinHandle<()>>` 用于取消；表是唯一事实来源，
内存表只做句柄索引，两者以 `id` 对齐。

**`cmd.rs` 新增**：

```rust
/// 流式执行：逐行回调 on_line，供作业写 stdout_tail
pub async fn run_streaming(
    cmd: &mut Command,
    budget: Budget,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<Output>
```

实现要点与现有 `run` 一致（超时 kill + wait、输出上限），差别只是 stdout 用
`BufReader::lines()` 边读边回调，而不是 `output()` 一次性收齐。

**`api.rs` 新增路由**：

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/jobs` | 提交作业，返回 `{id}` |
| GET | `/api/jobs` | 列表（分页，默认按创建时间倒序） |
| GET | `/api/jobs/:id` | 单条详情（含 stdout_tail 全量） |
| POST | `/api/jobs/:id/cancel` | 取消 |
| GET | `/api/jobs/:id/stream` | SSE 增量输出 |

SSE 是单向的，因此取消必须走独立的 POST，不能靠关流实现。

**互斥衔接**：作业执行体内部调用 `cmd::run_streaming` 时传 `Budget::package(1800)`，
`LockGroup::Package` 的静态锁自然把同组作业排成队列，dpkg / pacman 锁冲突由
P1-1 的既有机制消解，作业层不需要额外排队逻辑。

## 六、前端落点

- 新增 `frontend/src/views/Tasks.vue`：列表（状态标签 + 耗时 + 尾部输出折叠）+ 详情抽屉 +
  `EventSource` 接 `/api/jobs/:id/stream` + 取消按钮。
- 路由与侧边菜单各加一项。
- `Packages.vue` / `Docker.vue` / `Backups.vue` 的写操作改为提交作业，成功后跳转任务页
  并提示「已加入任务队列」。Arch 下的「滚动更新」按钮同样走作业。
- 旧的同步响应分支保留：`job_id` 为空时仍按原逻辑展示结果，便于灰度。

## 七、实施顺序与验收

1. 迁移 + `jobs.rs` 骨架 + `Db` 的 jobs CRUD + 重启收尾（可独立提交）
2. `cmd.rs::run_streaming` + 单测（可独立提交）
3. 接入 `packages/action`（样本最复杂，先做）
4. 接入 `backup_restore` / `backup_create`
5. 接入 `docker pull` / `docker_install`
6. 前端任务页与三个页面的提交改造

验收条件：

- 提交一次安装 → 立即拿到 `job_id` → 任务页能实时看到输出增长 → 结束后状态为 `success`；
- 安装进行中提交第二个安装 → 第二个作业停在 `pending`，第一个结束后才开始执行；
- 作业运行中 `kill` 掉面板进程 → 重启后该作业显示为 `interrupted`，不是永久的 `running`；
- 取消运行中的作业 → 状态 `cancelled`，且 `ps` 中不再有对应子进程。

## 八、已知风险与边界

1. **超时杀进程只杀直接子进程**：`apt-get` 会派生 `dpkg`，当前 `cmd.rs` 的
   `start_kill()` 不覆盖孙进程。彻底解决需要 `setsid` + 杀进程组，属独立改动，
   本细案不夹带；影响是极端超时场景下可能残留一个 `dpkg`，重启面板即可清除。
2. **SSE 连接与作业生命周期解耦**：前端刷新页面会重连并回放 `stdout_tail`，
   不会因为断流丢进度；但如果输出超过 200 行，早期内容不可追溯（这是刻意的取舍）。
3. **不自动续跑**：理由见第四节。
4. **表清理**：`jobs` 行随采样循环按 90 天清理（复用 `AUDIT_RETENTION_SECS` 口径），
   提交时顺带清理 `interrupted` 且超过 7 天的空作业。

## 九、工作量与改动面

| 部分 | 新增 | 修改 |
|------|------|------|
| 后端 | `jobs.rs`（约 300 行）、迁移 1 个 | `db.rs`、`cmd.rs`、`api.rs`、`main.rs` |
| 前端 | `Tasks.vue`、`api/jobs.ts` | 路由、菜单、三个业务页面 |

后端部分可在不碰前端的情况下先跑通（用 curl 验证），前端改造独立于后端提交。
