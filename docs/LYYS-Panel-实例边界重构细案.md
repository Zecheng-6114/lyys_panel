# 实例边界重构 —— 落地细案

> 实例页现有的「主机应用」是扫描进程、按可执行文件路径聚合出来的自动发现结果。
> 它既不是主流面板的管理边界，也会把「跑在系统目录下的解释器型应用」（典型是
> `java -jar server.jar`）挡在外面——MC 服务端因此完全不出现在实例页。本细案把实例页
> 的边界重塑为「容器 + 管理员定义的 systemd 服务」，并删除进程扫描自动发现。

## 一、要解决的具体问题

| 场景 | 现状 | 症状 |
|------|------|------|
| 以 systemd 服务托管的应用（`minecraft.service` → `/usr/bin/java`） | 可执行文件落在 `/usr/` 下，被判为系统进程 | 实例页没有它；进程页默认也不显示它 |
| 手工部署在 `/opt` 的应用（`/opt/foo/bin/foo`） | 被判为「应用」 | 实例页凭空出现一张以可执行文件路径为名的卡片，没有任何显式注册 |
| 一个应用由多个同名可执行文件组成 | 按可执行路径聚合 | 猜错时静默合并 / 拆分，用户无从修正 |
| `/init`（WSL 引导） | 硬编码排除 | 死代码；项目不支持 WSL |

根因：**「什么算被托管的负载」不能由扫描进程来判定。** 主流面板的管理边界只有三种
——容器、systemd unit、显式注册项目；进程扫描只是展示手段。本项目对照后，缺的正是
「systemd 服务」这一档。

## 二、对标结论（已调研，不再回抛）

| 面板 | 实例 / 应用边界 |
|------|----------------|
| 1Panel | 应用商店安装的应用（安装时由面板写入 unit / compose），显式创建 |
| 宝塔 | Java 项目管理器 / Supervisor 里**手动添加**的项目，显式注册 |
| Cockpit | 以 systemd unit 为管理单元，服务与容器分列，不做进程扫描 |
| MCSManager / Crafty | 在面板里显式创建实例，不扫描进程 |
| Pterodactyl | 全部容器化 |

**没有任何主流面板靠进程扫描识别主机应用。** 因此删除本项目的自动发现不是「砍功能」，
而是移除一个错误分支。

## 三、边界模型（本细案已定）

实例 = 以下两类之一：

1. **容器**：Docker 管理的负载（现状，不动）。
2. **服务**：**管理员定义**的 systemd service 单元——unit 文件位于
   `/etc/systemd/system/` 或 `/run/systemd/system/`。

明确排除：

- 软件包自带的 unit（位于 `/usr/lib/systemd/system/`；Debian 的 `/lib/systemd/system`
  在 merged-usr 下是它的符号链接）——它们属于「服务」页，不是被托管的负载。
  apt / pacman 装的 nginx、docker 不会进实例页。
- 任何由进程扫描得出的「主机应用」——本细案删除这条分支。

为什么用「unit 文件位置」作判据：

- 它等价于「显式注册」：手写 unit、`systemctl edit --full`、面板安装应用（1Panel 式做法）
  都会在 `/etc/systemd/system/` 落文件；而 `systemctl enable` 只在 `*.wants/` 建符号链接，
  不会误命中。
- 一次 `stat` 即可判定，不必解析 unit 文件内容，也不必为每个 unit 起 `systemctl show`。
- 发行版组件天然被 `/usr/lib` 挡住，无需维护容易失效的白名单。

边界说明（同步写入 README 已知限制）：手动 `nohup ./server &` 起、没有 unit 的进程仍不会
出现在实例页——这与主流一致（要托管就先写 unit）。「不依赖 systemd 的受管应用（显式注册
+ Supervisor 式守护）」是后续路线图项，不在本次范围。

## 四、进程 → 服务单元的归属规则

实例要显示 pids、CPU、内存，必须把进程归到 unit 名下，依据是 `/proc/<pid>/cgroup`
里的 unit 路径：

```
cgroup v2（Debian 12+ / Arch 默认）：0::/system.slice/minecraft.service
cgroup v1（hybrid）：              1:name=systemd:/system.slice/minecraft.service
```

解析规则（新增 `rprocess::cgroup_unit(pid) -> Option<String>`）：

1. 逐行读 `/proc/<pid>/cgroup`；每行按 `:` 最多切三段，取第三段（路径）。
2. 路径按 `/` 切分，取**最后一个**以 `.service` 结尾的组件作为该进程的 unit。
   - 取「最后一个」是为了支持嵌套：`user@1000.service/app.slice/foo.service`
     应归到 `foo.service`，而不是 `user@1000.service`。
3. 读不到 `/proc/<pid>/cgroup`（权限不足、进程已退出）→ 返回 `None`，不归属任何实例。

**关键约束：归属只在「已列出的实例 unit 集合」内匹配。** 即 `cgroup_unit` 的结果必须与
本次实例列表里的某个 unit 名精确相等才算归属。这样 `user@1000.service`、
`systemd-logind.service` 之类的噪声单元不会意外变成实例。

解析核心抽成不读文件系统的纯函数，便于单测：

```rust
/// 从 /proc/<pid>/cgroup 的文本里取出所属 service 单元
pub(crate) fn unit_from_cgroup_text(text: &str) -> Option<String>;
```

## 五、后端改动清单

### 5.1 `instances.rs`

删除：

- `InstanceKind::Host` → 换为 `InstanceKind::Service`（序列化值 `service`）
- `APP_PREFIX`、`app_id()`、`aggregate_host_apps()` 及相关测试断言
- 模块头注释里关于「主机应用」的描述

新增：

```rust
/// 服务实例的 id 前缀
pub const SERVICE_PREFIX: &str = "service:";

/// 管理员定义 unit 的落盘目录
const ADMIN_UNIT_DIRS: [&str; 2] = ["/etc/systemd/system/", "/run/systemd/system/"];

/// unit 文件的候选路径（含模板形式），供 is_admin_unit 与单测使用
fn admin_unit_paths(unit: &str) -> Vec<String>;

/// unit 是否由管理员定义
fn is_admin_unit(unit: &str) -> bool;

/// 汇总成服务实例（纯函数，便于单测）
fn from_service(svc: &opservice::ServiceInfo, procs: &[&ProcessInfo]) -> Instance;
```

`list()` 流程：

1. 容器（现状）。
2. `opservice::list().await` 取已加载 unit → 过滤 `is_admin_unit` → 得到实例 unit 集合。
3. 取一次进程快照（`state.monitor`），对集合内每个 unit 筛出 `cgroup_unit(pid) == unit`
   的进程，求和 CPU / 内存、收集 pids。
4. unit 没有归属进程也保留（对应「已停止」），`state` 由 `svc.active == "active"`
   映射为 `running` / `exited`，与容器 state 语义对齐。
5. 排序：容器在前、服务在后，同类按名称排序（现状规则不变）。

`Instance` 各字段对服务的取值：

| 字段 | 取值 |
|------|------|
| `id` | `service:<unit>`，如 `service:minecraft.service` |
| `name` | unit 名去掉 `.service` 后缀（`minecraft`） |
| `detail` | `ServiceInfo.description` |
| `state` | `active == "active"` → `running`，否则 `exited` |
| `ports` / `project` | 空 |
| `cpu` / `mem` | 归属进程求和，内存复用现有 `fmt_bytes` |
| `pids` | 归属进程 pid，升序 |

关于 unit 的来源：复用 `opservice::list()` 的 `systemctl list-units --type=service --all`，
即只包含 systemd **已加载**进内存的单元。刚写好但从未 start / enable 的 unit 在
`daemon-reload` 之后才会出现——这是刻意的，避免为了列出「存在但未加载」的单元再引入
`list-unit-files` 与状态 join 的第二条命令。

`is_admin_unit` 的模板单元处理：unit 名形如 `foo@bar.service` 时，除精确匹配
`/etc/systemd/system/foo@bar.service` 外，再匹配模板形式 `/etc/systemd/system/foo@.service`。

### 5.2 `rprocess.rs`

删除：

- `Scope` 枚举与 `Scope::parse`
- `classify()`、`is_system_exe()`、`SYSTEM_PREFIXES`、`SYSTEM_EXES`（`/init` 死代码）
- `in_container()`、`RUNTIME_HINTS`（删除后无人引用；`cgroup_owns` 用的是自己的前缀数组）
- 测试 `scope_parse_defaults_to_system`、`system_exe_covers_kernel_threads_and_system_dirs`、
  `system_exe_covers_wsl_bootstrap_at_the_root`、`classify_sends_foreign_paths_to_app`

保留并调整：

- `cgroup_owns()` 原样保留（容器归属）
- 新增：

```rust
/// 进程所属的 systemd service 单元（cgroup 路径里最深的一个 *.service 组件）
pub fn cgroup_unit(pid: u32) -> Option<String>;
```

- `owns()` 新增分支：`service:<unit>` → `cgroup_unit(p.pid).as_deref() == Some(unit)`
- `list()` 去掉 `scope` 参数：

```rust
pub async fn list(state: &AppState, instance: Option<&str>) -> Result<Vec<ProcessInfo>>;
```

  无 `instance` → 返回全量进程（按 CPU 降序，现状）；有 `instance` → 按 `owns` 过滤。
  模块头注释同步改写（不再是「默认只返回系统进程」）。

新增测试：`unit_from_cgroup_text` 的 v1 / v2 / 嵌套 / 无 unit 四例，`owns_matches_service_unit`。

### 5.3 `api.rs`

- `ProcessesQuery` 删除 `scope` 字段；`processes_list` 改为
  `rprocess::list(&state, q.instance.as_deref())`
- `instances_list` 不变
- `container_of()` 保留：服务实例调 `/instances/:id/files` 时仍返回
  「该实例不是容器，没有独立的文件系统」——这条文案对服务同样成立（前端会隐藏入口，
  接口层断言保留）
- 服务的启停重启**不新增接口**：前端复用 `POST /api/services`
  （`RequireRole<1>`，与「服务」页同一权限），`opservice::action` 已做服务名合法性校验

### 5.4 `ai_tools.rs`

- `list_processes`（现 L210）改为 `crate::rprocess::list(state, None)`

## 六、前端改动清单

### `Instances.vue`

- `Instance.kind` 类型加 `"service"`；`appCount` → `serviceCount`
- 卡片 tag 两分支：容器 / 服务；`detail` 与 `pids.length` 展示沿用（服务 pids 可能为 0）
- 卡片动作按 kind 分支：
  - 容器：文件管理 / 日志 / 进程 / 更多（现状不变）
  - 服务：日志 / 进程 / 更多（启动 · 停止 · 重启）——**不提供「文件管理」**，
    unit 没有独立文件树，跳到某个目录是误导
  - 服务「更多」复用 `POST /services`，参数 `{ name: unit, action }`
- 移除主机应用相关文案与分支：`openFiles` 的主机分支、`openLogs` 的空跳转分支
- 搜索框占位改为「搜索实例名 / 镜像 / 描述」；计数行改为「容器 N · 服务 M」

### `Processes.vue`

- 删除「系统进程 / 应用进程 / 全部」单选组与 `scope` 状态；`load()` 只发 `instance`
- `instanceLabel` 增加 `service:` 分支（显示 unit 名），保留 `container:` 分支
- 表格即全量进程，`filtered` 只做关键词过滤（现状）

### `Logs.vue`

- 新增 `service:` 分支：`mode` 固定 `journal`，`unit` 预填为 unit 名且输入框只读，
  进场即 `load()`（点「日志」不该看到空框）
- 提示标签：`服务日志 · <unit>`，可关闭并回到常规视图（复用 `clearInstance`）
- 现有 `container:` 分支不变

## 七、删除清单（逐项确认）

| 位置 | 删除内容 |
|------|---------|
| `instances.rs` | `InstanceKind::Host`、`APP_PREFIX`、`app_id`、`aggregate_host_apps` 及主机应用测试 |
| `rprocess.rs` | `Scope`（含 `parse`）、`classify`、`is_system_exe`、`SYSTEM_PREFIXES`、`SYSTEM_EXES`（`/init`）、`in_container`、`RUNTIME_HINTS` 及四个相关测试 |
| `api.rs` | `ProcessesQuery.scope` 字段 |
| `Instances.vue` | 主机应用文案与分支、`appCount` |
| `Processes.vue` | scope 单选组 |

## 八、测试用例

后端（`cargo test`）：

1. `unit_from_cgroup_text`
   - `0::/system.slice/minecraft.service` → `minecraft.service`
   - `1:name=systemd:/system.slice/minecraft.service` → `minecraft.service`
   - `.../user@1000.service/app.slice/foo.service` → `foo.service`（取最深）
   - `0::/` → `None`
2. `admin_unit_paths` / `is_admin_unit`
   - `/etc/systemd/system/foo.service` 命中
   - `/usr/lib/systemd/system/foo.service` 不命中
   - 模板 `foo@.service` 命中实例 `foo@bar.service`
   （把路径拼装抽成纯函数，单测不真的读磁盘；`is_admin_unit` 的文件存在性判断以
   `/etc/systemd/system/`、`/run/systemd/system/` 两个真实目录做一次集成性抽查）
3. `from_service`
   - 无归属进程 → 仍在列表里、`state == "exited"`、`pids` 为空
   - 多进程 → `pids` 升序、CPU / 内存为求和
   - `id == "service:minecraft.service"`、`name == "minecraft"`
4. `owns`
   - `service:minecraft.service` 只匹配 `cgroup_unit == minecraft.service` 的进程
   - `container:<短ID>` 分支回归不受影响

前端无自动化测试（项目现状），靠手测。

## 九、手工验收

在 Debian 或 Arch 上：

1. 建一个测试 unit（刻意把可执行文件放在 `/usr/bin` 下，复现「解释器型应用」场景）：

```ini
# /etc/systemd/system/lyys-demo.service
[Service]
ExecStart=/usr/bin/sleep 100000
[Install]
WantedBy=multi-user.target
```

   `systemctl daemon-reload && systemctl start lyys-demo`
2. 实例页出现 `lyys-demo` 卡片，标签「服务」，`state=running`，`pids` 含 `sleep` 的 pid，
   CPU / 内存有值。
3. 卡片「进程」→ 进程页只显示该 pid；「日志」→ 直接是 `journalctl -u lyys-demo` 的内容；
   「更多 → 重启」→ unit 重启、卡片状态刷新。
4. `systemctl stop lyys-demo` → 卡片保留，`state=exited`，`pids` 为空。
5. 确认 `sshd`、`docker`、`lyys-panel` 等 `/usr/lib` 单元**不**出现在实例页。
6. 确认原先 `/opt/...` 这类主机应用卡片不再出现；进程页不再有系统 / 应用切换。
7. 未装 Docker 的机器：实例页仍能列出服务，不报错。

## 十、回退方案

改动是「删一条分支 + 加一条分支」，无数据迁移、无表结构变化。回退即 `git revert`
对应提交，不涉及数据库。

兼容性说明：旧的 `app:<路径>` 实例 id 会失效。该 id 不存在服务端持久化，只出现在 URL
query，刷新即回到新列表；若用户书签了 `?instance=app:...`，进程页按 `owns` 匹配不到会
返回空列表——可接受。

## 十一、风险与取舍

1. **apt / pacman 装的 nginx 不进实例页**：这是刻意的边界（对照 1Panel / 宝塔：商店安装、
   显式添加才进）。用户若希望它进来，正确做法是显式注册（第三阶段路线图项），
   而不是放宽判据——放宽会把上百个系统 unit 灌进列表。
2. **归属依赖 cgroup 格式**：cgroup v1 / v2 都已覆盖；systemd 之外的守护方式
   （Supervisor、裸 `nohup`、Docker 之外的容器运行时）不在范围。
3. **每轮实例列表要读一遍全部 `/proc/<pid>/cgroup`**：进程数上千时是上千次小文件读，
   与现有 `aggregate_host_apps` 同量级，无明显回退。若后续成为瓶颈，再考虑按 cgroup
   缓存，本次不做。
4. **服务的 CPU / 内存是归属进程之和**：脱离 cgroup 的派生进程不计入，与 Cockpit 取舍一致。
5. **只列已加载单元**：见 5.1 末段。
6. **`is_admin_unit` 是文件名匹配**：`systemctl edit` 的 drop-in 不改变 unit 文件位置，
   因此不会把 `/usr/lib` 的单元拉进来——符合预期。

## 十二、改动面

| 部分 | 新增 | 修改 | 删除 |
|------|------|------|------|
| 后端 | `cgroup_unit` / `unit_from_cgroup_text`、实例的服务分支 | `api.rs`、`ai_tools.rs` | `instances` 主机应用链、`rprocess` 分类链 |
| 前端 | 服务卡片分支、日志服务分支 | `Instances.vue`、`Processes.vue`、`Logs.vue` | scope 单选组 |
| 文档 | — | `README.md`（功能表「进程」「实例」行、已知限制补一条）；`CHANGELOG.md` 随发布补写，本次不动 | — |

## 十三、实施顺序

1. 后端：`rprocess.rs` 解析函数 + 测试（可独立提交）
2. 后端：`instances.rs` 服务分支 + 删除主机应用链（可独立提交）
3. 后端：`api.rs` / `ai_tools.rs` 调用点收口，`cargo clippy --all-targets -- -D warnings` 通过
4. 前端：`Instances.vue` / `Processes.vue` / `Logs.vue`，`npm run build` 通过
5. 文档：README（CHANGELOG 随发布补写，本次不动）

每步之间保持可编译；构建顺序先前端 `npm run build` 再后端 `cargo build`
（`frontend/dist` 在编译期被嵌入）。
