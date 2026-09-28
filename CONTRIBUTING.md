# lyys-panel 贡献与工程规范（强制）

本文件定义本仓库的提交规范、版本策略与代码风格。提交信息格式采用业界标准
**Conventional Commits 1.0.0**（<https://www.conventionalcommits.org/>），版本管理
采用 **Semantic Versioning 2.0.0**（<https://semver.org/>），变更日志采用
**Keep a Changelog 1.1.0**（<https://keepachangelog.com/>）。

提交格式由 `.git/hooks/commit-msg` 钩子**机械强制执行**：不合格的信息会被 git 直接
拒绝。任何人（包括 AI）都不得绕过钩子（见「六、禁止事项」）。AI 的额外行为约束见
`AGENTS.md`。

钩子不随 git 本身版本化。重新 `git init` 或恢复 `.git` 后，必须先运行：

```sh
sh scripts/install-hooks.sh
```

---

## 一、提交顺序（工作流）

一次提交 = 一个逻辑单元（一件事）。禁止把不相关的改动混进同一个提交，
禁止碎片化的小提交（小修复攒到所属功能完成一起提交）。

严格按以下顺序执行：

1. **完成一个逻辑单元**：一个功能、一个修复、一次重构或一处文档变更。
2. **验证通过才允许提交**：
   - 后端：`cd backend && source $HOME/.cargo/env && cargo clippy --all-targets -- -D warnings`
   - 后端测试：`cd backend && source $HOME/.cargo/env && cargo test`
   - 涉及前端：`cd frontend && npm run build`
   - 任何一项不通过，先修复，不得提交。
3. **自查**：`git status` + `git diff` 确认改动范围与本逻辑单元一致，
   无意外文件（调试代码、临时文件、密钥）。
4. **精确暂存**：`git add <具体文件>...`。
   禁止 `git add -A` / `git add .`。
5. **起草提交信息**：按「二、提交信息格式」编写。
6. **审批**（对 AI 强制）：AI 必须把提交信息全文展示给用户，获得明确批准
   后才能执行提交。
7. **提交**：`git commit -s`（`-s` 自动生成 `Signed-off-by:` 尾行，必填）。
8. **推送**：仅在用户明确要求时执行 `git push`。

## 二、提交信息格式（Conventional Commits）

```
<type>[(<scope>)][!]: <description>

<body：说明 what 与 why（不是 how）。每行不超过 72 字符，可空行分段。
小改动也必须有至少一行正文。>

[ BREAKING CHANGE: <破坏性变更说明> ]

Signed-off-by: <Name> <email>
```

### 主题行（subject）

- **type（必填）**：`feat` `fix` `perf` `refactor` `docs` `test` `build` `ci`
  `chore` `revert`。语义与 SemVer 的映射见「三、版本策略」。
- **scope（可选）**：小写子系统名，圆括号包裹，取值为后端模块
  （`api` `auth` `db` `monitor` `backup` `alerts` `update` `docker` `files`
  `logs` `network` `packages` `cron` `tls` `embed` `distro` `ops`）或前端
  （`ui` `theme` `layout` `router` `views`）；跨层或仓库级改动可省略 scope。
  钩子不校验 scope 白名单，只要求小写字母开头。
- **`!`（可选）**：破坏性变更标记，必须同时提供 `BREAKING CHANGE:` 行，否则钩子拒绝。
- **description**：紧跟 `: ` 之后，**小写开头**、祈使句现在时
  （`add` / `fix` / `remove`，不用 `added` / `fixes` / `updating`）、末尾无句号、
  整行 ≤ 72 字符。全英文。

### 正文（body）

- 必填，至少一行，说明**改了什么、为什么改**；动机、取舍、与旧行为的差异
  写在这里，而不是主题行。
- 每行 ≤ 72 字符（手动折行）。全英文。
- 破坏性变更在正文之后以 `BREAKING CHANGE: <说明>` 单独一行声明。

### 尾行（trailer）

- 必须有 `Signed-off-by: Name <email>`，用 `git commit -s` 自动生成。

## 三、版本策略（Semantic Versioning 2.0.0）

- 版本号同时写在 `backend/Cargo.toml` 与 `frontend/package.json`，**两处必须一致**；
  发布前核对：`grep '^version' backend/Cargo.toml` 与 `grep '"version"' frontend/package.json`。
- 版本递增由提交 type 驱动：
  - `fix` / `perf` → PATCH（x.y.Z）
  - `feat` → MINOR（x.Y.z）
  - `BREAKING CHANGE`（任意 type）→ MAJOR（X.y.z）
- `0.y.z` 阶段（当前 `0.1.0`）为初始开发期，一切接口不保证稳定；`1.0.0` 定义
  面板对外 API（HTTP 路由 + 二进制行为）的稳定基线。
- 版本号的实际递增在**发布**时执行（打 `vX.Y.Z` tag + GitHub Release），日常功能
  提交不改动版本号文件；发布提交用 `chore(release): X.Y.Z` 主题。
- 面板自更新模块（`update.rs`）从 GitHub Release 读取最新版本，发布 tag 即更新源。

## 四、变更日志（Keep a Changelog）

- 根目录维护 `CHANGELOG.md`，按 Keep a Changelog 1.1.0 格式：最新版本在最上，
  每个版本下按 `Added` / `Changed` / `Deprecated` / `Removed` / `Fixed` /
  `Security` 分组。
- 每次发布时根据自上一版本以来的提交历史补写对应版本段落；日常提交不强制
  同步更新（避免冲突）。`Unreleased` 段落可选，用于攒发布说明。
- 分组映射参考：`feat`→Added、`fix`→Fixed、`perf`→Fixed、`refactor`→Changed、
  `BREAKING CHANGE`→Changed + 显著标注、安全修复→Security。

## 五、代码规范细则

### Rust（backend/）

- 格式化：rustfmt 默认配置（等效 `cargo fmt --check` 通过）；4 空格缩进、
  行宽 100、Unix 换行，遵循 The Rust Style Guide（<https://doc.rust-lang.org/style-guide/>）。
- Lint：clippy `all` 组（correctness/suspicious/style/complexity/perf）全部
  `-D warnings` 提级为错误；`pedantic`/`restriction`/`nursery` 不整体开启，
  需要时逐条 cherry-pick。确需保留例外用 `#[allow(...)]` 局部豁免并注释原因。
- 错误处理：内部 anyhow，对外 API 层 thiserror；库代码禁 panic、禁
  `unwrap()`/`expect()` 出现在非测试路径（测试内可用）。
- 模块按领域划分（auth/monitor/process/service/log/backup/alerts/update/...），
  跨模块共享状态只经 `AppState`。
- 注释用中文；公开项（pub fn/struct）写一行 doc 注释说明职责。

### Vue 3 + TypeScript（frontend/）

- 一律 `<script setup lang="ts">` 组合式 API；禁用 Options API。
- props 用**基于类型的声明**：`defineProps<Props>()`（interface 就近定义或
  `import type` 导入）；默认值用响应式解构或 `withDefaults`。
- emits 同样用类型声明 `defineEmits<{ ... }>()`。
- 组件文件名与模板引用 PascalCase（`Backups.vue`）；路由视图放 `views/`，
  可复用块放 `components/`。
- 类型检查以 `vue-tsc`（已并入 `npm run build`）为准，禁止 `any` 泛滥；
  确需 `any` 局部使用并注释。
- ESLint + Prettier 默认配置，提交前无 error。

### 通用

- 所有中间件必须 async/await，禁止 callback 风格。
- 仅支持 Debian/Ubuntu 系与 Arch 系；不支持的系统启动时打印提示并退出。
- 系统命令按发行版家族分派（apt/dpkg vs pacman；Docker 包名同理）。

## 六、安全基线（面板自身）

以下为本仓库已确立并持续执行的安全约束，新增代码必须遵守；审查按此清单逐项核对：

1. **凭据**：JWT 密钥只从环境变量或 `0600` 密钥文件加载，绝不落库、不落日志；
   初始管理员密码随机生成写 `0600` 文件。任何提交/日志/接口响应不得回显密钥、
   密码、token。
2. **认证与会话**：全部业务路由经 JWT 中间件；服务端维护吊销名单，登出即失效；
   登录失败指数退避（throttle）。
3. **授权（RBAC）**：接口按 `RequireRole<N>` 分级（0 viewer / 1 operator / 2 admin）；
   危险操作（备份恢复、自更新、告警配置、账号管理）一律 admin；前端菜单与路由
   `adminOnly` 只是体验层，服务端校验是唯一权威。
4. **输入校验**：文件名/路径类参数一律白名单校验（如备份名
   `panel-YYYYMMDD-HHMMSS.db`），杜绝路径穿越；文件管理接口整体禁访问数据目录。
   JSON 请求体经 `SafeJson` 包装并设大小上限；上传接口设体积上限。
5. **SQL**：全部参数化查询（rusqlite `?` 占位），禁止拼接用户输入。
6. **出站请求**：webhook URL 仅 http/https + ASCII 白名单 + 拒 userinfo；
   发送失败只记日志不重试，不得阻塞主循环。
7. **响应头**：五项安全头（nosniff / X-Frame-Options DENY / CSP default-src 'self' /
   no-referrer / Permissions-Policy）由 `security_headers` 中间件统一注入，
   错误路径同样生效。
8. **传输**：默认 HTTPS（自签或指定证书）；HTTP 端口仅可选做 301 跳转。
9. **审计**：非 GET 请求写 `audit_log`（90 天保留）；告警事件独立表存储。
10. **依赖**：新增依赖优先纯 Rust 实现（rustls 栈），避免同一进程引入两个
    crypto provider（rustls 会在握手 panic）；引入后 `cargo tree` 查重。

## 七、禁止事项

以下操作一律禁止，无例外：

1. `git commit --no-verify` 或任何绕过钩子的行为。
2. `git push --force` / `--force-with-lease` 到共享分支；改写已推送的历史
   （`rebase`、`reset --hard` 后强推、`commit --amend` 已推送的提交）。
3. `git add -A` / `git add .`（必须逐文件暂存）。
4. 未经用户明确要求就执行 `commit` / `push` / 合并。
5. 提交包含密钥、`.env`、数据库文件的内容（`.gitignore` 已覆盖大部分，
   暂存前仍要自查）。
6. 修改 git 配置来放宽以上任何一条。

历史教训：本仓库曾因多个 AI 会话并发提交与强推导致历史被覆盖（2026-09-23），
此后历史全新重来，规范强制执行。

## 八、示例

合格：

```
feat(api): add download endpoint for file manager

Serves a single file as an attachment with the original filename so
the browser saves instead of renders. Path checks reuse the same
sandbox rules as listing, closing the gap where preview links could
escape the allowed root.

Signed-off-by: Zhang San <zs@example.com>
```

```
fix(theme): stop white text on selected radio buttons in dark mode

Element Plus declares --el-* variables on the component element
itself, so overrides on :root lose. Move the override onto
.el-radio-button to match the declaration site.

Signed-off-by: Zhang San <zs@example.com>
```

```
refactor(api)!: rename /v1/orders to /v1/checkout

Body explaining the motivation...

BREAKING CHANGE: clients on /v1/orders must migrate before release
2.0.0; the old route returns 410 after cutover.

Signed-off-by: Zhang San <zs@example.com>
```

不合格（钩子会拒绝）：

```
update                                          ← 无 type、无正文
fix: Correct minor typos                          ← description 大写开头
api: Add endpoint                               ← 缺 type 前缀（旧格式，已废弃）
feat: added the download endpoint.               ← 非祈使句、句末句号
feat(ui): tweak buttons\n\nSigned-off-by: ...     ← 缺正文
feat!: drop legacy config                        ← 有 ! 但缺 BREAKING CHANGE: 行
```
