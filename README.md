# Zmllama

面向本地小模型的 **Windows 桌面 Agent 终端**：左侧是带多标签的 PowerShell 终端，右侧是任务面板。让 2–4B 级别的本地模型通过工具调用（读文件、搜代码、跑命令），在真实终端里一步步完成编程类任务。

```
┌──────────────────────────────┬─────────────────────────────┐
│  Terminal（xterm.js + PTY）   │  任务面板                    │
│  ┌─ >_ 1 ─┬─ >_ 2 ─┐         │  ┌ 你: 看看 main.rs        │
│  │ powershell      │         │  │ AI: (思考) TOOL: read    │
│  │ PS C:\workspace>│         │  │ [read] 成功 …            │
│  └─────────────────┘         │  └ 结论：…                  │
└──────────────────────────────┴─────────────────────────────┘
```

## 功能特性

- **双面板布局**：左侧 xterm 终端（多标签、可拆分拖拽宽度），右侧任务对话
- **对接本地 LLM**：调用 `http://localhost:8080/v1`（llama.cpp `llama-server` 的 OpenAI 兼容接口），启动时自动探测模型
- **SSE 流式输出**：逐字渲染回复，思考链（`reasoning_content`）折叠展示
- **结构化工具调用**：优先走 OpenAI `tools` 协议；服务端不支持时**自动降级**为纯文本 `TOOL:` 协议（配合 GBNF 语法约束）
- **内置工具**：
  - `read` — 读取文件（带行号）或列出目录
  - `search` — 工作区内递归搜索文本（区分大小写不敏感）
  - `run` — 在持久 PowerShell 中执行单条命令，返回输出与退出码
- **安全机制**：路径严格限制在工作区内（词法校验 + 符号链接解析）、`run` 执行前需人工审批、高危命令黑名单、重复工具调用自动中止
- **体验细节**：暗/亮主题、字号调节、工作区切换（同步终端目录）、任务取消与重置

## 技术栈

| 层 | 选型 |
|---|---|
| 语言 / GUI | Rust · Dioxus 0.6（desktop） |
| 终端 | portable-pty + 内嵌 xterm.js / xterm-addon-fit |
| HTTP | reqwest（SSE 流式 + JSON） |
| 模型后端 | llama.cpp `llama-server`（OpenAI 兼容 `/v1`） |

## 架构

```
src/
├── main.rs            窗口、菜单、主题、双面板布局与 CSS
└── terminal/          PTY 会话管理、Rust↔JS 桥接
    ├── pty.rs         伪终端启动、读写泵、CSI 6n 代答
    ├── state.rs       会话注册表、输入/调整大小分发
    ├── component.rs   标签栏与终端组件
    └── js.rs          xterm 初始化脚本、主题/字体注入
└── task/              LLM 调用、Agent 编排、工具执行
    ├── llm.rs         SSE 流式调用、工具参数聚合、降级逻辑
    ├── agent.rs       Agent 主循环、审批、重复调用检测
    ├── tools.rs       工具实现（read/search/run）与工作区路径防护
    ├── shell.rs       持久 PowerShell（UTF-8 管道、退出码捕获）
    ├── protocol.rs    系统提示词、工具定义、TOOL: 文本协议解析
    ├── panel.rs       任务面板 UI
    └── state.rs       全局状态（工作区、HTTP 客户端、审批通道）
```

## 环境要求

- **Windows 10/11**（内置 PowerShell；项目为 Windows 专用）
- Rust 工具链（`rustup` stable，2021 edition）
- 本地 [llama.cpp](https://github.com/ggml-org/llama.cpp) `llama-server`，监听 `8080` 端口

## 构建与运行

```powershell
# 1. 启动本地模型服务（需先下载 .gguf 模型）
llama-server -m 你的模型.gguf --port 8080

# 2. 构建并运行（项目根目录）
cargo build --release
cargo run --release
```

> 提示：为小模型启用文本协议降级路径时，可用 GBNF 语法约束输出（`res/tool_grammar.gbnf`）：
>
> ```powershell
> llama-server -m 你的模型.gguf --port 8080 --grammar-file res\tool_grammar.gbnf
> ```

## 使用说明

1. 左侧终端可直接使用（多标签、支持新建/关闭）
2. 右侧输入任务描述，例如"看看 main.rs"、"找出所有使用 connect 的地方"
3. 模型会先调用工具获取信息，再给出结论；执行命令类操作会弹出审批卡片，需点击 **允许** 才会真正执行
4. 菜单 **文件 → 打开文件夹** 可切换工作区，终端与任务模式会同步切换目录

## 安全设计

- 所有工具路径必须位于工作区内：先做符号链接解析，再对不存在路径做词法校验，防止 `..` 逃逸
- `run` 命令经过高危语法黑名单（`Invoke-Expression`、`Remove-Item`、下载类命令等）与元字符（`; & | > < \``）双重拦截
- 命令执行前必须人工审批；拒绝后模型会被要求换一种方式
- 连续重复的同签名工具调用自动中止，避免模型陷入死循环
- 请求与命令均有超时（180s / 60s），支持随时取消

## 已知限制

- 仅支持 Windows；`detect_model` 会在启动后缓存模型名，运行中切换模型需重启应用
- 小模型 + 长输出的场景做了截断处理（保留头尾、省略中间，上限约 4000 字符）
- 仓库未附带 LICENSE 文件
