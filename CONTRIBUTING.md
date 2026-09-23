# lyys-panel 提交规范（强制）

本文件定义本仓库唯一的 git 提交流程与提交信息格式，参考 Linux 内核提交规范
（`Documentation/process/submitting-patches.rst`）裁剪而来。

规则由 `.git/hooks/commit-msg` 钩子**机械强制执行**：不合格的信息会被 git 直接
拒绝。任何人（包括 AI）都不得绕过钩子（见「禁止事项」）。AI 的额外行为约束见
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

## 二、提交信息格式

```
<subsystem>: <以大写动词开头的祈使句描述，不超过 72 字符，句末无句号>

<正文：说明 what 与 why（不是 how）。每行不超过 72 字符，
可用空行分段。小改动也必须有至少一行正文。>

Signed-off-by: <Name> <email>
```

### 主题行（subject）

- 格式固定为 `subsystem: Description`——小写子系统名、冒号、空格、大写开头。
- 描述用**祈使句现在时**：`Add` / `Fix` / `Remove` / `Refactor`，
  不用 `Added` / `Fixes` / `Updating`。
- 长度 ≤ 72 字符；末尾不加句号。
- 全英文。

### 推荐的 subsystem

| 范围 | subsystem |
|---|---|
| 后端模块 | `api` `auth` `db` `distro` `docker` `files` `logs` `monitor` `network` `service` `process` `packages` `cron` `embed` |
| 前端 | `ui` `theme` `layout` `router` `views` |
| 构建与部署 | `build` `deploy` |
| 文档 | `docs` |
| 跨多处的仓库级改动 | `core` |

新增模块可自行取小写子系统名（钩子只校验格式，不校验白名单）。

### 正文（body）

- 必填。至少一行，说明**改了什么、为什么改**；动机、取舍、与旧行为的差异
  写在这里，而不是主题行。
- 每行 ≤ 72 字符（手动折行）。
- 全英文。

### 尾行（trailer）

- 必须有 `Signed-off-by: Name <email>`，用 `git commit -s` 自动生成。

## 三、示例

合格：

```
api: Add download endpoint for file manager

Serves a single file as an attachment with the original filename so
the browser saves instead of renders. Path checks reuse the same
sandbox rules as listing, closing the gap where preview links could
escape the allowed root.

Signed-off-by: Zhang San <zs@example.com>
```

```
theme: Fix white text on selected radio buttons in dark mode

Element Plus declares --el-* variables on the component element
itself, so overrides on :root lose. Move the override onto
.el-radio-button to match the declaration site.
```

不合格（钩子会拒绝）：

```
update                                          ← 无 subsystem、无正文
fixed a bug in api.rs                           ← 过去式、小写开头、有句号
api: added the download endpoint.               ← 非祈使句、句末句号
feat(ui): tweak buttons                         ← 不是 subsystem: 格式（Conventional Commits 的 type(scope) 不被接受）
api: Add endpoint\n\nSigned-off-by: ...         ← 缺正文
```

## 四、禁止事项

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
