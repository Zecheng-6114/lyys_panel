# LYYS Panel

轻量级 Linux 服务器运维面板。单个 Rust 二进制内含前端构建产物，**部署时服务器无需 Node 环境**。

## 功能

| 模块 | 说明 |
|---|---|
| 仪表盘 | CPU / 内存 / 磁盘 / 网络实时快照，历史趋势折线图 |
| 进程 | 进程列表查看与结束 |
| 服务 | systemd 服务查看、启动 / 停止 / 重启 |
| 日志 | journal 日志查询、日志文件列表与实时 tail |
| 文件 | 目录浏览、在线编辑、上传下载、新建 / 重命名 / 删除 |
| 软件 | apt 包列表、可升级查询、搜索、安装 / 卸载 / 升级 |
| 计划任务 | crontab 增删改查 |
| 网络 | 网卡、路由、连接、DNS 查看 |
| Docker | 容器列表与启动 / 停止 / 重启 / 删除、容器日志、镜像拉取与删除、Compose 项目启停；未安装时页面上可直接安装 |
| AI 助手 | 面板内与模型对话，流式逐字返回，**对话自动存档**；带**只读工具调用**、**长期记忆**与**情绪识别**（见下） |

## 安全说明

面板能改文件、结束进程、装卸软件，且以 root 运行 —— 请务必理解以下几点：

- **登录限流**：同一「来源 IP + 用户名」连续登录失败会触发指数退避（1s、2s、4s…… 封顶 30s），
  触发期间返回 `429`。这是退避而非锁定，合法用户输错几次只会觉得变慢，不会被锁在门外。
  退避状态存于内存，重启即清空。
- **仍需注意**：面板默认明文 HTTP，对外暴露前请置于反向代理并启用 TLS；
  文件模块可访问整个文件系统，服务以 root 运行是管理 systemd / 进程所需 ——
  这两点决定了**只能在受信网络内使用**，不要暴露到公网。

## 技术栈

- **后端**：Rust · Axum 0.8 · Tokio · rusqlite（bundled，无外部 SQLite 依赖）· JWT + Argon2
- **前端**：Vue 3 · TypeScript · Vite 5 · Element Plus · Pinia · ECharts
- **前端嵌入**：`rust-embed` 编译期把 `frontend/dist` 打入二进制
- **AI 助手**：按 OpenAI 兼容的 `/chat/completions` 与 `/embeddings` 调用，出站请求用 `ureq`（同步客户端，置于 `spawn_blocking`，不占 tokio 工作线程）；对话流式转发用 `tokio-stream` + SSE
- **情绪识别**：`ort`（ONNX Runtime 绑定，`load-dynamic` 动态加载系统运行库）+ `tokenizers`，模型文件不内嵌

## 构建

> ⚠️ **必须先构建前端，再编译后端。**
> 后端用 `rust-embed` 在**编译期**读取 `frontend/dist`，该目录不存在会直接编译失败。
> `frontend/dist` 不纳入版本管理，所以 clone 后不能直接 `cargo build`。

### 前置要求

- Rust stable（含 rustfmt / clippy，见 `backend/rust-toolchain.toml`）
- Node.js 20+ 与 npm

### 一条命令构建

```bash
./build.sh
```

产物：`backend/target/release/lyys-panel`

### 分步构建

```bash
# 1. 前端（必须先做）
cd frontend
npm install        # 首次安装依赖；需严格按 lockfile 复现时用 npm ci
npm run build

# 2. 后端
cd ../backend
cargo build --release
```

## 本地运行

```bash
PANEL_ADMIN_PASSWORD='你的强密码' ./backend/target/release/lyys-panel
```

默认监听 `127.0.0.1:3789`，浏览器打开 http://127.0.0.1:3789 ，账号 `admin`。

前端热重载开发（后端需另起一个实例）：

```bash
cd frontend && npm run dev
```

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `PANEL_ADDR` | `127.0.0.1:3789` | 监听地址。`0.0.0.0:3789` 表示所有网卡 |
| `PANEL_DB` | `data/panel.db` | SQLite 数据库路径 |
| `PANEL_ADMIN_PASSWORD` | 随机生成 | 首次启动创建管理员时使用，**仅第一次生效** |

未设置 `PANEL_ADMIN_PASSWORD` 时会生成随机密码并打印到日志。

### AI 助手

「AI 助手」页需要自行填写接口地址（如 `https://api.deepseek.com/v1`）、模型名与 API Key，
保存后写入数据库 `settings` 表（键为 `ai.base_url` / `ai.api_key` / `ai.model`，另有
`ai.timeout_secs` 供直接改库时调整），因此换模型不必改配置文件。API Key 不会下发到浏览器，页面只显示「是否已填」；再次保存时留空
表示不改动。本地模型（如 Ollama）不需要 Key，留空即可。

只接受 OpenAI 兼容接口，模型名不必手填：页面按接口地址自动拉取 `GET /models` 的列表放进下拉框
（拉不到时仍可直接输入）。请求发往 `<base_url>/chat/completions`。

等待模型响应的上限固定 600 秒：本地跑大模型时首次调用要先把整个模型读进显存，普通云端的等待
时间在这里远远不够；连接超时固定 10 秒 —— 地址不通要立刻报错，不跟着长超时一起干等。

助手默认走流式（SSE，`/api/ai/chat/stream`），模型吐字即逐块显示；另保留非流式的
`/api/ai/chat` 供程序化调用。

#### 工具调用（只读）

助手能通过工具**查询**这台服务器：系统概览（CPU / 内存 / 磁盘 / 网络）、进程、
systemd 服务状态、日志（journal 与文件）、Docker（容器 / 镜像 / Compose / 状态 / 容器日志）、
计划任务。它问起这类事会先查再说，气泡上方会显示它查了什么。

也能**联网**：`web_search` 搜索（走 `cn.bing.com` —— 服务器实测 google 与 duckduckgo
都不通，百度与 bing 通）与 `fetch_url` 抓取网页正文。抓取有两道防护：**拒内网地址**
（网页内容不可信，不能让它代面板去打内网）与**正文截断**（默认 3000 字，免得把上下文挤爆）。
提示词里也写明了网页内容是外部输入、不是指令。

**一律只读** —— 不提供启停服务、杀进程、改文件、装卸软件的能力，模型判断错时最多是
查错一次，不会把服务器搞坏。工具循环最多 5 轮，防止来回调个不停；个别本地模型不支持
工具调用时，可在配置条上关掉「工具调用」开关。

#### 对话存档

每轮对话结束后，前端把整份消息列表存回 `ai_session` 表（消息量小，整份覆盖比增量更不容易出错）；
刷新页面会自动载入最近一次会话，顶部「历史」里可以切换或删除旧对话，「新对话」则另起一条。
存档失败不会打断对话，只在顶部提示一下。

#### 长期记忆

存在数据库 `ai_memory` 表里，页面上「记忆」按钮可查看、手动增删。写入有两条路：
每轮对话后由模型自己抽取值得记的事（后台任务，不占等回复的时间，可用「自动记忆」关掉），
以及手动添加。

召回优先走向量：把当前这句话发给同一个接口的 `/embeddings`（Ollama 原生支持，
「向量模型」从同一个模型列表里选，如 `nomic-embed-text`；列表接口不区分模型用途，
所以按名字挑出像向量模型的条目，挑不到就显示全部），与记忆库里的向量算余弦取前几条。
**没有向量也能用** —— 取不到时自动退化为关键词重合度召回（中文按二元字组切），
所以不配向量模型不会让记忆功能失效，只是召回精度差些。

#### 情绪识别

本地跑一个多语言的三分类模型（positive / neutral / negative），判定结果作为
「情绪状态」注入系统提示词，只影响音量和节奏、不换人设；页面上每条回复下方会标出判定结果。

- 模型文件放数据目录的 `models/emotion/`（默认与数据库同级），运行时依赖系统的
  libonnxruntime：`apt install libonnxruntime1.21`，找不到可通过 `ORT_DYLIB_PATH` 指定。
- 置信度低于 0.65 一律当中性 —— 实测命令式句子（「帮我看下内存用了多少」）会被
  这个模型给出 0.37~0.57 的「正面」分，照它调语气会让回答莫名欢快起来。
- 模型不可用时整个情绪环节静默跳过，不影响对话。

## 部署（systemd）

### 代码同步

代码经由 GitHub 在本地与服务器之间同步；构建与运行在服务器上进行
（服务器需有 Node.js 与 Rust 工具链）。
## 已知限制与待办

**安全**

- 面板默认明文 HTTP，对外暴露前建议置于反向代理并启用 TLS
- 文件模块可访问整个文件系统（不设根目录约束）—— 仅限受信网络内使用
- 登录限流基于内存，重启后清零；且未接入反向代理时按来源 IP 计数，
  若置于 NAT 之后，同一出口的多个用户会共享退避额度

**其他**

- 外部命令调用无超时（Docker 与 AI 请求除外，见各自实现）；apt 同步操作无并发锁
- 助手有只读工具与联网工具，但**没有任何执行类工具**：启停服务、杀进程、改文件等仍需
  用户在面板页面上自己做
- 搜索依赖 cn.bing.com 结果页的结构，Bing 改版会导致搜不到；抓取只做纯文本剥离，
  动态渲染的页面（前端 JS 出内容的）抓不到正文
- 情绪只分正 / 中 / 负三档（细粒度的 GoEmotions 没有可用的多语言 ONNX 导出，
  英文模型对中文输入会因分词失效而不准）；且「自动记忆」每轮会多跑一次模型调用，
  本地大模型上这会多占一次显存与算力，不需要时可在页面上关掉
- Docker 的 Compose 项目在「独立 `docker-compose` 命令」这一路径下，会以容器 label 反推项目，
  容器被全部删除的项目不可见
- `frontend/package.json` 声明了 `lint` / `format` 脚本，但仓库尚未提交对应的 ESLint / Prettier 配置，直接执行会失败
- 前后端均无自动化测试；无 CI
- 缺少「修改管理员密码」界面：遗忘密码需停服务删 `panel.db` 重建，**会丢失监控历史**

已解决（原先列在本节）：

- ~~登录接口无失败限流~~ → 已实现指数退避（`auth::LoginThrottle`）

## 目录结构

```
lyys_panel/
├── backend/            Rust 后端
│   ├── src/
│   │   ├── main.rs         入口、配置、路由装配
│   │   ├── api.rs          HTTP 处理器与路由表
│   │   ├── auth.rs         登录、JWT、Argon2、管理员引导、登录退避
│   │   ├── db.rs           SQLite 连接池与建表
│   │   ├── embed.rs        内嵌前端资源服务（含 SPA 回退与缓存头）
│   │   ├── monitor.rs      系统指标采集
│   │   ├── files.rs        文件浏览 / 读写 / 上传下载
│   │   ├── packages.rs     apt 包管理
│   │   ├── crontab.rs      计划任务
│   │   ├── network.rs      网络信息
│   │   ├── logs.rs         日志查询
│   │   ├── docker.rs       Docker 容器 / 镜像 / Compose（含一键安装）
│   │   ├── ai.rs           AI 助手：配置读写、模型调用、流式转发
│   │   ├── aimemory.rs     AI 长期记忆：向量化、召回、抽取与遗忘
│   │   ├── aitools.rs      只读工具：系统 / 进程 / 服务 / 日志 / Docker / 定时任务
│   │   ├── websearch.rs    联网取信息：搜索（cn.bing.com）与网页抓取
│   │   ├── emotion.rs      情绪识别：本地 ONNX 三分类模型
│   │   ├── opservice.rs / rprocess.rs   systemd 服务与进程操作
│   │   └── ...
│   └── Cargo.toml
├── frontend/           Vue 3 前端
│   └── src/
│       ├── layout/AppLayout.vue   侧边栏 + 顶栏 + 内容区骨架
│       ├── views/                 各功能页面
│       ├── router/                路由与登录守卫
│       ├── stores/                主题等全局状态
│       ├── api/                   axios 封装与拦截器
│       └── styles/theme.css       Element Plus 变量覆盖（黑白主题）
├── deploy/             systemd 单元与环境变量模板
└── build.sh            一键构建脚本
```

## 界面约定

- **无边框设计**：所有 `--el-border-color*` 均为 `transparent`，层次靠背景色差表达。请勿添加实色边框。
- **全部圆角**：统一 6px，基准变量为 `theme.css` 中的 `--radius`。新增组件优先引用该变量。
- 主题为纯黑白灰，无彩色、无阴影。注意浏览器自动填充的输入框底色由浏览器绘制（暗色下是一层暗黄），不受面板变量控制。
- 按钮与输入框并排时不要给按钮写死高度（会变成正方形），也不要给文本域写死 `rows`；让容器 `align-items: stretch`，按钮跟随输入框高度。
- 圆角容器若内部子元素带背景（表头、hover 行、加载遮罩等），**必须配 `overflow: hidden`**，否则背景会填满四角、把圆角盖成直角。
