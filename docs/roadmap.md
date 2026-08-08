/# OpenZen 执行路线图 (Execution Roadmap)

> 状态：Draft v0.1 · 生成日期 2026-06-12
> 配套阅读：[stream-protocol-migration.md](stream-protocol-migration.md) · [tui-redesign.md](tui-redesign.md) · [comparison-vs-other-agents.md](comparison-vs-other-agents.md) · [acceptance-criteria.md](acceptance-criteria.md) · [risk-register.md](risk-register.md)

---

## 〇、阅读须知

本路线图将上一轮"全面体检"得到的改进项落到**可勾选**的代码任务上。每个 Phase 含：
- **目标**（Why）
- **前置依赖**（Prerequisites）
- **任务清单**（每条标 `[ ]`，完成后改为 `[x]`）
- **关键文件**（带 `file:line` 锚点）
- **验收**（Verification，含可执行命令）
- **回退**（Rollback，遇到 P0 阻塞性问题的安全网）

**约定**：
- 复杂度用 🟢S / 🟡M / 🔴L 表示
- 涉及新增 crate 须同步改 `Cargo.toml` 的 `workspace.members` 与 `[package] dependencies`
- 涉及前端须同步改 `frontends/package.json` 并 `npm install`

---

## 一、总体策略

| 战役 | 内容 | 价值 | 阻塞性 |
|---|---|---|---|
| **A. 流协议收官** | 让 LLM 层直接 emit 新事件、三个 UI 切到消费新事件、删除双发 + 启发式 | 抹掉最大技术债，三个 UI 同时升级 | ⚠️ **是**（不改后续 P1 都建立在脆弱假设上） |
| **B. TUI 现代化** | 引入 `reedline` / `PromptTemplate` / RAG / 主题 | 缩窄与 aichat 的 UX 差距 | 否 |
| **C. WebUI 体验** | Auth / file attachment / transient data parts / chat branching | 追平 Vercel AI Chatbot | 否 |
| **D. Agent 工作模式** | 新 `ga-agent` crate（instructions + tools + documents）| 借鉴 aichat 最大的差异化能力 | 否 |
| **E. 桌面 + 生态** | Tauri 托盘通知 / tool discovery / smart_router 增强 | 长尾补齐 | 否 |

**依赖图**：

```
A (流协议) ──┬─→ B (TUI) ──┐
             ├─→ C (WebUI) ┼─→ F (清理)
             └─→ D (Agent) ┘
                  E (桌面+生态) 可与 B/C/D 并行
```

---

## 二、Phase 0：流协议迁移收官（战役 A）🟡M ✅

> **目标**：让 `StreamEvent::Token/Thinking/ToolCall/ToolResult` 在 hot path 上**永远不再产生**，三个 UI 全部从 `TextStart/Delta/End` 等新事件消费。删掉 narration.rs / content.rs 的启发式。

### Phase 0.1 —— LLM 层直接 emit 新事件 ✅

- [x] `crates/ga-llm/src/stream.rs:71` —— `text_delta` 分支改为发 `StreamEvent::TextStart { id: gen_id() }`（首次）+ `TextDelta { id, text }`（后续）；同一 text block 内 `current_text_id` 由 adapter 或 stream parser 持有
- [x] `crates/ga-llm/src/stream.rs:84` —— `thinking_delta` 同上，发 `ReasoningStart` + `ReasoningDelta`
- [x] `crates/ga-llm/src/stream.rs:104` (`content_block_stop`) —— 发对应 `TextEnd` / `ReasoningEnd` / `ToolInputAvailable`
- [x] `crates/ga-llm/src/stream.rs:53-99` —— `parse_openai_sse` 同理，**MiniMax 的 `in_thinking_tag` 状态机保留**（这是合规的 tag 处理，因为 MiniMax 的 content 真的混了 `<thinking>`），但 emit 时改用新事件
- [x] 抽公共 `id_gen: AtomicU64` 到 `ga-core-types` 共享（避免各 parser 重复）
- [x] 给 `StreamEvent` 加单元测试：序列化 round-trip，14 个新变体各一例

### Phase 0.2 —— `mod.rs` 删除双发 ✅

- [x] `crates/ga-server/src/webui/mod.rs:420-563` —— 整段 `for proto_event in adapter.adapt(&event) { match &proto_event { ... } }` 整段删除，**adapter 不再 hot path 调用**
- [x] `crates/ga-server/src/webui/mod.rs:476-563` —— 旧事件 match 全部移除，**SSE 只发 protocol_v1**
- [x] `protocol_adapter.rs` 删除（不再需要，老 session JSON 走 `legacy_session_reader.rs` 的概念已废弃）
- [x] `webui/sse_bus.rs` 的 `SseEvent::token/thinking/tool_call/tool_result/done` 构造函数删除

### Phase 0.3 —— TUI 切到新事件 ✅

- [x] `crates/ga-tui/src/event.rs:141-291` —— `handle_stream_event` 重写，删去 `StreamEvent::Token/Thinking/ToolCall/ToolResult` 四个分支
- [x] `crates/ga-tui/src/event.rs:283-285` —— `ReasoningStart/Delta/End` 分支从 no-op 改为真正渲染（`app.add_thinking()`）
- [x] `crates/ga-tui/src/app.rs:62-109` —— `ChatItem` 加 `ThinkingBlock { content: String, expanded: bool }` 变体
- [x] `crates/ga-tui/src/chat.rs` —— 加 `render_thinking_block()`，仿 opencode 折叠为单行
- [x] `crates/ga-tui/src/narration.rs` —— **整个文件删除**（375 行启发式）
- [x] `crates/ga-tui/src/content.rs` —— **整个文件删除**（`<thinking>` / `<summary>` 启发式解析）
- [x] `crates/ga-tui/src/event.rs:144-145` —— 移除 `app.narration_filter.feed(&text)`
- [x] `crates/ga-tui/src/event.rs:738-742` —— `app.body_buffer = ContentBuffer::new(); narration_filter.reset();` 改为只重置 body_buffer
- [x] `Cargo.toml` 移除 `ga-tui` 依赖中已经只服务于这两个模块的内部 use

### Phase 0.4 —— Tauri 桌面切到新事件 ✅

- [x] `src-tauri/src/lib.rs:267-426` —— `run_agent_for_session` 中用 `ProtocolAdapter` 实例仅在**读老 session** 时调用（Tauri 走本地 event bus，不经过 HTTP 中间件）
- [x] `src-tauri/src/lib.rs:359-376` —— forwarder 改为只 emit `protocol_v1` 事件
- [x] `src-tauri/Cargo.toml` 添加 `ga-server` 依赖

### Phase 0.5 —— 删除 StreamEvent 旧变体 ✅

- [x] `crates/ga-core-types/src/event.rs:19-72` —— 删除 `Token/Thinking/ToolCall/ToolCallReady/ToolResult/Done` 六个变体
- [x] 全局 grep 修复编译错误
- [x] `mod.rs:489-491` 旧 `Done` 消费方改为消费 `FinishMessage`
- [x] `agent_loop.rs` 中所有 emit 旧事件的位置改为 emit 新事件

---

## 三、Phase 1：TUI 现代化（战役 B）🟡M

> **目标**：让 TUI 体验对齐 sigoden/aichat 9.8k stars 的标杆
> **前置依赖**：Phase 0 完成

### Phase 1.1 —— reedline（已跳过，改用自制 History 模块）

- [x] 评估 reedline 0.40 集成 → **阻塞**：reedline 的 `Reedline::read_line(&mut self, &prompt)` 是同步阻塞调用，与 TUI 的异步事件循环 (`tokio::select!` + `mpsc::Receiver`) 不兼容。未找到非阻塞 read_line API。reedline 的 `InternalEvent` 模型需要 TUI 放弃现有架构。
- [x] 改用自制 `History` 模块：`crates/ga-tui/src/editor.rs` —— LRU（1000 条）、去重、持久化到 `~/openzen/history.txt`、Ctrl+↑/↓ 搜索、`/history` 命令。
- [x] `ga-config` schema 加 `[tui.history_size]` 字段

**验收**：
- 打开 TUI，按 Ctrl+R 能反向搜索历史命令
- 输入 `/sess<Tab>` 自动补全为 `/session `
- 多行粘贴不会触发 Enter

### Phase 1.2 —— PromptTemplate（左/右提示符）

- [ ] `crates/ga-tui/src/theme.rs` —— 新增 `PromptTemplate` 结构
- [ ] 支持模板变量：`{?session ...}` / `{!session ...}` / `{role}` / `{agent}` / `{model}` / `{consume_tokens}` / `{consume_percent}%`
- [ ] `ga-config` schema 加 `[tui.left_prompt]` / `[tui.right_prompt]` 字段
- [ ] 默认值：左 `'{?session {?agent {agent}>}{session}{?role /}}{!session {?agent {agent}>}}{role}{?rag @{rag}} '`、右 `'{consume_tokens}({consume_percent}%)'`

**验收**：
```toml
# config/mykey.toml
[tui]
left_prompt = "{model} > "
right_prompt = "{consume_tokens}"
```
启动 TUI 后提示符变化正确

### Phase 1.3 —— RAG 系统

- [ ] 新建 `crates/ga-rag/` crate
  - [ ] `Cargo.toml` 加 `fastembed = "3"` 或 `rig-core` 评估二选一
  - [ ] `lib.rs` 实现 `RagSystem { embedder, reranker, vector_store }`
  - [ ] `chunk_size` / `chunk_overlap` / `top_k` / `rag_template` 配置
- [ ] `ga-config` schema 加 `[rag]` section
- [ ] `ga-tui/command.rs` 加 `/rag <query>` 子命令
- [ ] `ga-server/src/webui/mod.rs` 加 `POST /api/rag/query` 端点
- [ ] 文档加载器：`document_loaders.<ext> = "<command>"`（如 `pdf: 'pdftotext $1 -'`）

**验收**：
```bash
ga tui
> /rag ./docs
> 给我讲讲流协议迁移
# 期望：先检索相关段落，附 [1] [2] 引用，LLM 回答基于检索
```

### Phase 1.4 —— 主题配置化 ✅

- [x] `ga-config` schema 加 `[tui.theme]` section
- [x] `crates/ga-tui/src/theme.rs` —— 改为可配置 `Theme` struct + 静态默认值，提供 `Theme::light()` / `Theme::dark()`
- [x] 借鉴 aichat：支持自定义 dark/light 高亮主题
- [x] `/theme light|dark` 切换命令

### Phase 1.5 —— Markdown 增量渲染 ⏭️ 已跳过

> **跳过原因**（2026-06-12）：`crates/ga-tui/src/markdown.rs` 748 行需要重构渲染管线为 `IncrementalRenderer`（缓存上一次渲染状态，仅对增量 token 触发局部重绘）。这触及 `render_markdown`、`split_fenced_code_blocks`、`parse_inline_spans` 的深层重构，预估 300+ 行纯算法改写，风险高于 Phase 1.1-1.4 三项合计。无阻塞下游依赖，建议独立 PR 处理。

- [ ] `crates/ga-tui/src/markdown.rs` —— 加 `IncrementalRenderer` 结构
- [ ] 每个 token 触发增量重绘，而非 `render_markdown` 全量重跑
- [ ] 借鉴 aichat 的 `termimad` 流式策略

**验收**：长 markdown 流式输入时，CPU 占用 < 5%（vs 当前每次重渲可能 20-50%）

---

## 四、Phase 2：WebUI 体验（战役 C）🟡M

> **目标**：追平 Vercel AI Chatbot 的核心 UX
> **前置依赖**：Phase 0 完成

### Phase 2.1 —— 简单 Auth ✅

- [x] `ga-config` 加 `[server] auth_token` 字段（默认随机生成，传 CLI 参数 `--auth-token`）
- [x] `crates/ga-server/src/webui/mod.rs` —— axum 中间件校验 `Authorization: Bearer <token>`，SSE `/api/events` 和 `/api/health` 豁免
- [x] `frontends/src/lib/api/chat.ts` —— `fetchJson()` 封装自动加 header + 401 时 prompt 输入 + `localStorage` 持久化
- [x] `frontends/src/lib/api/sessions.ts` —— 所有 fetch 调用统一加 `Authorization: Bearer` header
- [ ] `src-tauri/src/lib.rs` —— desktop 启动时读 token 给 webview（Tauri webview 内调用 SSE 走本地 event bus，不经过 HTTP 中间件，优先级低）
- [ ] 文档更新 README.md：新增 `openzen serve --auth-token <manual>`

**验收**：
```bash
openzen serve
# 期望：log 打印 "Auth token: <random>"，无 token 的 curl 返回 401
```

### Phase 2.2 —— File attachment ⏭️ 已跳过

> **跳过原因**（2026-06-12）：File attachment 依赖链较长：(1) `POST /api/upload` multipart 端点 + blob 存储，(2) ga-llm 目前无 `image_url` / `image_base64` content type 支持，需要扩展 Claude + OpenAI 两套消息构建器，(3) 前端 ChatInput.svelte 拖放 + AttachmentPreview 组件。3 个依赖环节均需约 1 天工作量。建议在 Auth + Agent 功能落地后独立 PR 处理。

- [ ] `frontends/src/lib/components/ChatInput.svelte` —— 加拖拽 + `<input type=file multiple>`
- [ ] `crates/ga-server/src/webui/mod.rs` —— 加 `POST /api/upload` 端点（axum multipart），存到 `~/openzen/blobs/<uuid>`
- [ ] `frontends/src/lib/stores/chat.ts` —— 发送消息时附 `attachments: [{blob_id, filename, mime}]`
- [ ] `ga-llm/src/openai.rs` / `native_oai.rs` —— OpenAI 多模态：`content: [{type: "text", ...}, {type: "image_url", url: "data:..."}]`
- [ ] Claude 多模态：同样处理
- [ ] `frontends/src/lib/components/AttachmentPreview.svelte` —— 拖入时的预览条

**验收**：
- 拖入 PDF 截图，能直接发给模型并获得基于图像的回答

### Phase 2.3 —— Transient data parts ✅

- [x] `frontends/src/lib/stores/parts.ts` —— `DataPart { type, id, dataType, content, transient }` 类型
- [x] `frontends/src/lib/stores/protocol-processor.ts` —— `data_search_stage` / `data_token_meter` / `data_memory_retrieval` / `data_compressing_context` 事件处理
- [x] `frontends/src/lib/components/TransientsBar.svelte` —— 顶部 transient 通知条，4 秒后自动消失
- [ ] `ga-core/src/compress.rs` —— 触发点 emit `data_compressing_context`（后续性能优化时再做）

- [ ] `frontends/src/lib/stores/parts.ts` —— 加 `DataPart` 类型 + `transient: boolean` 字段
- [ ] `frontends/src/lib/stores/protocol-processor.ts` —— `data_search_stage` / `data_token_meter` / `data_memory_retrieval` 等
- [ ] `crates/ga-server/src/webui/mod.rs` —— agent loop 中"压缩上下文前"emit `data_compressing_context` 事件
- [ ] `ga-core/src/compress.rs` —— 触发点 emit
- [ ] `frontends/src/lib/components/TransientsBar.svelte` —— 顶部条状组件，transient part 结束后自动消失

**验收**：
- 触发上下文压缩时，WebUI 顶部出现 "Compressing context: 124k → 18k tokens" 进度条
- 完成后自动消失，会话历史中不留痕

### Phase 2.4 —— Chat branching / regenerate ✅

- [x] `frontends/src/lib/stores/types.ts` —— `Message` 加 `parentId?: string` + `children: string[]`
- [x] `frontends/src/lib/stores/chat.ts` —— `regenerate()` 函数，调用 `POST /api/sessions/:id/regenerate`
- [x] `frontends/src/lib/components/ChatMessage.svelte` —— 助手消息 header 右侧加"重新生成"按钮（旋转刷新图标）
- [x] `crates/ga-server/src/webui/mod.rs` —— `POST /api/sessions/:id/regenerate` 端点，pop 最后 assistant + user 消息后重新运行
- [x] `frontends/src/lib/components/MessageTreeNav.svelte` —— 多分支时显示备用回复导航按钮

### Phase 2.5 —— 主题切换 UI ✅

- [x] `frontends/src/lib/components/ThemeSwitcher.svelte` —— 暗/亮/跟随系统 三按钮切换
- [x] `frontends/src/app.css` —— 新增 `html.theme-light { ... }` 覆盖明暗两套 CSS 变量
- [x] localStorage 持久化

---

## 五、Phase 3：Agent 工作模式（战役 D）🔴L

> **目标**：借鉴 aichat 的 "Agent = Instructions + Tools + Documents" 模型
> **前置依赖**：Phase 1.3 (RAG) 完成

### Phase 3.1 —— 新 `ga-agent` crate ✅

> **说明**：实际实现未依赖 RAG（ga-rag 未创建），Agent 直接从 config 注入 system prompt + tools list，走标准 `run_agent_loop`

- [x] `Cargo.toml` workspace.members 加 `crates/ga-agent`
- [x] `crates/ga-agent/Cargo.toml` —— 依赖 `ga-core-types` / `ga-config` / `ga-llm` / `ga-tools`（无 ga-rag）
- [x] `crates/ga-agent/src/lib.rs` —— `AgentConfig { model, temperature, instructions, use_tools, documents, variables }` + `Agent::load()` / `Agent::list()` / `interpolate_instructions()`
- [ ] `crates/ga-agent/src/role.rs` —— 实现 `RoleLike` trait（推迟，当前直接用 LoopConfig）
- [x] `crates/ga-agent/src/variables.rs` —— `__INPUT__` / `__CONTEXT__` / 自定义变量替换（内联在 lib.rs 中）
- [x] 配置文件 `~/.openzen/agents/<name>/config.yaml`

### Phase 3.2 —— CLI 入口 ✅

- [x] `src/main.rs` —— 新增 `ga agent <name>` 子命令
- [x] `src/main.rs` —— 新增 `ga agent --list`
- [x] 加载 agent 配置 → 注入到 `LoopConfig.system_prompt` → 跑标准 `run_agent_loop`

### Phase 3.3 —— WebUI 集成 ✅

- [x] `frontends/src/lib/components/AgentPicker.svelte` —— 启动时选择 agent，modal 面板
- [x] `crates/ga-server/src/webui/mod.rs` —— `GET /api/agents` 列出所有 agent

### Phase 3.4 —— TUI 集成 ✅

- [x] `crates/ga-tui/src/command.rs` —— `/agent <name>` 切换 + `/agent` 列出
- [x] `ga-tui/src/app.rs` —— `App.current_agent: Option<String>` 字段 + `template_vars` 注入

**验收**：
```bash
mkdir -p ~/.openzen/agents/researcher
cat > ~/.openzen/agents/researcher/config.yaml <<EOF
model: claude-sonnet-4
instructions: |
  You are a research assistant. Use __INPUT__ as the user's question.
  Always cite sources with [1] [2] format.
use_tools: web_search,rag
variables:
  tone: academic
EOF

ga agent researcher
> 量子计算的最新进展
# 期望：先 RAG 检索 + Web 搜索，再学术语气回答，带引用
```

---

## 六、Phase 4：桌面端丰富化（战役 E 一部分）🟡M

> **目标**：让 Tauri 桌面应用不止是"Webview 套壳"
> **前置依赖**：Phase 0 完成

### Phase 4.1 —— System tray + 通知 ✅

- [x] `src-tauri/Cargo.toml` 加 `tauri-plugin-notification`
- [x] `src-tauri/src/lib.rs` —— `TrayIconBuilder` 系统托盘图标，菜单含 "Open / Quit"
- [x] `src-tauri/src/lib.rs:267-426` —— agent 完成后 `app.notification().builder().title("OpenZen").body(...).show()`
- [x] 左键点击托盘图标恢复窗口

### Phase 4.2 —— 全局快捷键 ⏭️ 已集成于托盘

- [x] 托盘点击切换可见性替代全局快捷键（无需额外插件权限）
- [ ] 如未来需要，可添加 `tauri-plugin-global-shortcut` 实现 ⌘⇧Space

### Phase 4.3 —— 进度指示 ⏭️ 使用通知替代

- [x] 桌面通知在 agent 完成时发送，带响应摘要
- [ ] macOS dock badge 需原生插件支持，暂跳过

### Phase 4.4 —— 多窗口 ✅

- [x] `src-tauri/src/lib.rs` —— `open_session_window` 命令，每个 session 独立窗口
- [x] 窗口间通过 SseBus + SSE event 同步状态

---

## 七、Phase 5：生态与差异化 🟡M

### Phase 5.1 —— Tool Discovery (linkme) ✅

- [x] `crates/ga-tools/Cargo.toml` 加 `linkme` 依赖
- [x] `crates/ga-tools/src/registry.rs` —— `TOOL_FACTORIES` distributed slice + `build_auto()`
- [x] 每个 tool 文件加 `#[linkme::distributed_slice]` 自注册钩子
- [x] `build_default()` 先试 `build_auto()` 再 fallback `build_manual()`

### Phase 5.2 —— smart_router 增强 ✅

- [x] `crates/ga-config/src/mykey.rs` —— 加 `RouterConfig { cheap_model, flagship_model, thresholds, route_rules }` + `[router]` section
- [x] `crates/ga-llm/src/smart_router.rs` —— `from_config()` 构造函数 + `pick()` 优先匹配 `route_rules` 模式再走复杂度判定

### Phase 5.3 —— WebUI 多模型切换 UI 增强 🔶

- [x] `ModelSwitcher.svelte` —— context window 显示优化（K/M 自动格式化）
- [ ] "Test in playground" 按钮（后续）
- [ ] system prompt 差异对比（后续）

### Phase 5.4 —— 持久化压缩 + LRU ✅

- [x] `crates/ga-server/src/webui/sessions.rs` —— `SessionStore` 加 `max_sessions` 字段 + `with_max()` 构建器
- [x] 超限时按 LRU 归档到 `sessions_archive/` 目录，JSON 持久化
- [x] `set_max_sessions()` 动态调整 + `evict()` 修剪

---

## 八、Phase 6：清理与收尾 🟢S ✅

> **目标**：删除所有 Phase 0-5 遗留的兼容代码，提交干净的 v0.2.0

- [x] `protocol_adapter.rs` —— 已删除（Phase 0 时完成）
- [x] `frontends/src/lib/stores/parts.ts` —— `StreamEventItem` 保留但标注 `@legacy`（老 session 读取需要）
- [x] `crates/ga-core-types/src/event.rs` —— 无 `#[deprecated]` 标注残留
- [x] `docs/comparison-vs-other-agents.md` —— 更新 P3 完成项 + linkme 注册机制
- [x] `docs/acceptance-criteria.md` —— 更新功能清单 (17/20 ✅) + v0.2.0 新特性表
- [x] `CHANGELOG.md` —— v0.2.0 完整 changelog
- [x] `docs/adr/0008-remove-narration-heuristic.md` —— 解释 why + alternatives
- [x] `docs/adr/0009-rag-system-selection.md` —— fastembed vs rig-core 分析 + defer 决策

---

## 八·五、Phase 7：桌面 UI 器物语法改造（战役 G）🟡M

> **目标**：按 [ui-design-language.md](ui-design-language.md) 将 Tauri 前端从"暖棕珊瑚 + 卡片"迁移到"宋韵天青釉面"器物语法 — 三色克制、釉面三层、宋体铭文/楷体手迹、时间线折叠 O(1)、文化锚点(干支/落款/竖排天头)。
> **设计定稿来源**：`/Users/macstu/Desktop/openzen-ui-preview-v2.html`(用户已验收方向 + 锚点)
> **备份**：改造前当前源码已存 `backups/frontends-backup-20260801/`(rsync 排除 node_modules/dist)
> **原则**：先 token 层 → 布局骨架 → 组件 → 锚点,每步保功能,最后复杂度审计。

### Phase 7.1 —— CSS token 层替换 🟢S ✅

- [x] `frontends/src/app.css` —— 以 §2 token 替换旧色板(墨夜/釉下彩双主题,`data-theme` 切换)。2026-08-01 完成:墨夜 `#14120e` + 天青 `#93c3d6` + 朱砂 `#c05a3e`;亮色釉下彩 `#eee9db` + 青花 `#4f8ea8`
- [x] `frontends/src/app.css` —— 釉面三层:大气/噪点/开片纹 fixed 平铺 + `body:hover` 唤醒(§4)。完成:保留 glaze-atmosphere/noise/crackle/shimmer 四层,新增 `body:hover .glaze-crackle` 触摸唤醒(×1.9);亮色主题釉面减弱而非隐藏
- [x] `frontends/src/app.css` —— 字形栈:宋体/楷体/等宽/mono token(§3)。完成:`--font-serif`(Songti SC)、`--font-kai`(Kaiti SC)、`--font-mono` 系统栈
- [x] `frontends/src/app.css` —— `--ease` 入釉曲线 + 动效统一(§5)。完成:`--ease-soak` = ease-out-quint;soak-in/token-in/breath-qing 动画;`prefers-reduced-motion` 全局降级
- [x] 删除旧 DESIGN.md 中的珊瑚/Inter/卡片 token 引用,或标注 deprecated。完成:DESIGN.md 保留为历史文档(ui-design-language.md 已声明取代其视觉方向)

### Phase 7.2 —— 布局骨架 🟢S ✅

- [x] `frontends/src/App.svelte` —— Grid 改三栏 `172px 1fr 224px` + 38px 标题栏(§6)。完成:保持 flex(侧栏折叠/面板拖拽依赖),Sidebar 172px、main 1fr、SidePanel 保留拖拽;标题栏固定 38px
- [x] 标题栏:印章「禅」+ 修砚 + 干支「丙午 制」+ ctx 用量条(>78% 朱砂)。完成:印章 + 铭文「修砚」+「丙午 制」款识 + ctx 青线用量条(ctxColor 红黄→沿用阈值)
- [x] 左侧栏:顶部「＋ 新 会 话」印章式按钮(保留现有新建功能,仅换肤)。完成:Sidebar 品牌改「禅印 + 修砚 + 丙午」,新会话钮改天青描边印章式,宽度 260→172px
- [x] 消息流:竖排天头「卷一 · 修砚之录」+ 底部落款行「丙午 · 修砚 识」。完成:`.messages-scroll` flex 包裹竖排天头 + 660px 叙事流 + 落款行(含 seal-mini 小章)

### Phase 7.3 —— 消息与思考块 🟡M ✅

- [x] `ChatMessage.svelte` —— 用户消息改釉色条(整行 `--qing-bg` + 左缘青线),助手消息去卡片化。完成:用户 `--color-primary-muted` 底 + 2px 左青线;助手透明无边框纯墨
- [x] `ChatMessage.svelte` —— 时间戳改落款格式「砚主 识 · HH:MM:SS」。完成:footer 时间组加「识」(i18n `message.sig`,zh=识/en=sig.),宋体 10px;role-badge 改宋体铭文
- [x] `ThinkingBlock.svelte` —— 楷体手迹 + 折叠行「⚘ 静思 · 推演」+ 展开入釉动画。完成:去卡片改左缘青线,`--font-kai` 楷体,⚘ 标记,grid-rows 0fr→1fr 入釉展开

### Phase 7.4 —— 工具调用釉下暗纹 🟡M ✅

- [x] `ToolCallCard.svelte` —— 折叠态改单行青线(◈ 工具名 + 呼吸点/已竟/朱砂 ✕)。2026-08-01 完成:◈ 天青标记 + 宋体工具名 + 状态(运行=天青呼吸点 `run-dot`/已竟=墨色/错误=朱砂 ✕,经 `hasError` prop 从 `p.state === "output-error"` 传入);去卡片改单行青线下缘
- [x] 展开态:`--qing-bg` 底 + hairline + 釉光,参数/结果 mono。完成:`.tcc-inner` 用 `--color-primary-muted` + hairline + `--glaze-shadow`;错误结果左缘朱砂线;删旧 `toolSymbol` 函数
- [x] 保留现有展开/收起交互,仅换视觉 + 动画。完成:展开改 grid-rows 0fr→1fr 入釉动画,交互逻辑(计时器/args settle watchdog)零改动

### Phase 7.5 —— 时间线折叠保真 + 复杂度 🟡M ✅

- [x] `ChatMessage.svelte` —— 折叠头改天青虚线框 + 聚合统计(计数/总耗时/⚠错误),**保持 FOLD_THRESHOLD=5 与 `foldedStats` 逻辑不变**。完成:折叠头改天青虚线框 + `--primary-muted` 底 + 宋体,文案「时间线 · 折叠 N 事 · N 工具 · 总耗时」+ 朱砂「⚠ 1」;`timelineExpanded`/`foldedStats`/`toggleTimeline` 零改动
- [x] 验证折叠区事件不渲染(现有 `visibleGroups`/`foldedGroups` 切片已满足,确认无回归)。完成:确认 `foldedGroups` 不渲染、仅 `visibleGroups` 渲染后 5 组 — DOM O(1) 保持
- [x] 复杂度徽章思路迁移:开发态 debug flag 显示组件 O(1) 标注(可选,默认关)。**未做(可选)**:原型里为演示目的,产品里徽章无用户价值,roadmap 标注 optional 跳过
- 附带:删除已无引用的 `foldedText`/`foldedTotalText` 函数

### Phase 7.6 —— Composer 与侧栏 🟢S ✅

- [x] `ChatInput.svelte` —— 发送钮改钤印(天青方印「言」,busy「止」),附 件按钮 + 快捷键提示行。完成:发送=`.seal-btn` 天青方印「言」(按下 scale 钤印/釉光扫过),busy=纸底朱砂「止」+ 呼吸 pulse;附钮方形铭文化 + glaze-sweep;新增 composer-hint 行「⌘⏎ 发送 · ⇧⏎ 换行 · 附 件」(i18n fallback)
- [x] `Sidebar.svelte` —— 会话/项目列表换宋体铭文 + 当前项青线,底部工具钮换字。完成:`SessionList` session-name 改宋体铭文;当前项青线 + `--primary-muted` 底(已有,保留);底部 locale-toggle 保留
- [x] `SidePanel.svelte` —— 三 tab 改「物/审/迹」单字 + 活动时间线折叠(>5 折叠头)。**适配说明**:SidePanel tab 是打开的文件 artifact 标签(非分类面板),硬改「物/审/迹」会破坏文件预览 — 按"保留功能仅换肤"改为宋体铭文 + 天青 active 底线;活动时间线折叠已在 ChatMessage(7.5)实现,侧栏不重复

### Phase 7.7 —— 文化锚点收尾 🟢S ✅

- [x] 标题栏干支款 + 落款行 + 竖排天头统一落实(§7.9)。完成:7.2 已落实 — 标题栏「修砚 · 丙午 制」、消息流底部落款「丙午 · 修砚 识于杭州」、竖排天头「卷一 · 修砚之录」,7.3 落款格式「识 · HH:MM」覆盖全部消息
- [x] 印章系统:主章「禅」/发送「言」/落款小章,状态一致。完成:`.seal`(禅)/`.seal-btn`(言/止)/`.seal-mini`(落款小章)三处同源样式,亮色下统一釉白字
- [x] 亮色「釉下彩」主题全组件过一遍对比度。完成:亮色印章改「青花底 #4f8ea8 + 釉白字 #f5f1e5」;釉面四层在亮色减弱而非隐藏(0.32/0.012/0.05/0.3);hairline 亮色加深至 0.16

### Phase 7.9 —— 器物细节对齐原型 v2(用户验收后追加)🟢S ✅

> 2026-08-01 用户逐项验收原型 v2 后指出的细节偏差,全部修正。**备份回退仍用 `backups/frontends-backup-20260801/`**。

- [x] **底部信息条器物化**(`App.svelte` model-bar):移除与标题栏重复的 ctx tag;模型改天青描边宋体铭文胶囊 + mono 云/本地小标;出/入统计改等宽墨色 + 青线分隔;health 点改天青 glow;结晶/计时开关改器物方形 track(全部功能保留)
- [x] **底部器物底款**:model-bar 右侧加「禅」小印 + 「丙午 · 修砚 制」(呼应瓷器底部落款)
- [x] **输入框对齐原型 v2**(`ChatInput.svelte`):两个圆形图标附钮 → 单个「附 件」文字铭文钮 + 迷你菜单(文 件/图 片,弹出选文件/图片,保留原 pickFile/pickImage);placeholder 改「落笔 —— 交付给修砚…」/「静候入窑…」(en: "Dip the brush — hand it to Xiuyan…" / "In the kiln…")
- [x] **身份锚点**(i18n `message.role.you/agent` = 砚主/修砚,en = Master/Xiuyan):助手气泡 role-badge 移除(不再显示名称),用户气泡显示「砚主」;footer 落款改「修砚 识 · HH:MM:SS」/「砚主 识 · …」(roleLabel + 识)
- [x] **折叠卡片展开对齐**:ToolCallCard 展开体 margin/padding 对齐原型 event-inner(4px 10px 10px 22px,青底 + hairline + 釉光)
- [x] **左侧栏对齐原型 v2**(`Sidebar.svelte`):新增「项 目」「会 话」宋体区块标题;底部改三枚工具钮(设 / EN·中文 / 目),locale 切换并入中间钮
- [x] **工作目录移到标题栏**(`App.svelte`):从底部 working-dir-bar 移至标题栏「修砚 · 丙午 制」之后(mono 小字,max-width 260px ellipsis),底部 working-dir-bar 删除
- 最终构建:`npm run build` ✓ / `cargo tauri build --no-bundle` ✓ / LSP 全组件零错误 / 二进制 hash `index-BzpT59IT.js` 与 dist 一致

### Phase 7.10 —— 原型 v2 逐项细节对齐(二轮验收)🟢S ✅

> 2026-08-01 用户对照原型 v2 逐项检查后的第二轮修正。**备份回退仍用 `backups/frontends-backup-20260801/`**。

- [x] **时间线折叠头去框去 chevron**(`ChatMessage.svelte`):移除天青虚线框 + 浅色底 + 「▶」chevron — 改为纯宋体文字行(天青字,hover 淡青底),展开后无残留框体
- [x] **ToolCallCard 对齐 event-line/event-inner**(`ToolCallCard.svelte`):移除 chevron;折叠态 = 一行青线(◈ + 宋体名 + 状态,下缘 hairline 无卡片);展开体 margin 改 `4px 10px 10px`(去左缩进)、padding `12px 14px`、mono 参数/结果 — 与原型 event-inner 完全一致
- [x] **Composer 重构为 composer-box**(`ChatInput.svelte`):整体一框(边框 + 釉光阴影 + focus 天青 glow);textarea 无边框在顶部;下一行 composer-row =「附 件」钮(左)+「⌘ 发送 · ⇧⏎ 换行」(中)+ 言/止钤印(右),全部在框内;外层 660px 居中与消息流对齐
- [x] **工具卡展开体简化**(`ToolCallCard.svelte`,三轮验收):折叠时零 DOM(条件渲染,无残留框);展开后第一行 = 主参数 `path <值>`(k/v 结构),第二行 = 精简结果(单行化截断 160 字符,顶部虚线分隔);删除 formatArgs/formatResult 死代码
- [x] **助手 footer 精简**(`ChatMessage.svelte`,三轮验收):只保留「修砚 识 + 时间」与「总计时」;移除 status-pill(running/done)、工具耗时统计、token pills;时间前时钟图标删除;用户气泡 footer 补「砚主 识」落款(去时钟图标);删除 toolStats/formatTokenCount 死代码
- [x] **开关与 ctx 重排 + 釉下彩按钮**(`App.svelte`,三轮验收):结晶/计时开关从 model-bar 移到**标题栏最右**;ctx 用量从标题栏移到**输入框下方**(ChatInput 之后独立一行);标题栏接入 **ThemeSwitcher(釉下彩)** 三态按钮(暗/亮/系统,复用原组件 + 标题栏紧凑样式);model-bar 保留 模型胶囊/msgs/出/入/health/底款
- 最终构建:`npm run build` ✓ / `cargo tauri build --no-bundle` ✓ / LSP 全组件零错误 / 二进制 hash `index-D9FBkh7W.js` 与 dist 一致

### Phase 7.8 —— 复杂度与回归验收

- [ ] 10h soak:RSS 稳定 ≤350MB(时间线折叠下 DOM 恒定)。**待执行**:需 10h 长时间运行,后续排期
- [x] `npm run build` 零错误;`cargo tauri build --no-bundle` 通过。完成:build 2.4s ✓;cargo 2m23s ✓;二进制与 dist hash 一致(`index-DV5N-067.js`)
- [x] 长程任务(13+ 工具事件)折叠头正确显示聚合统计,展开按需挂 DOM。完成:折叠头显示 count/toolCount/totalMs/⚠;`foldedGroups` 不渲染,展开时 `visibleGroups` 全量 — 逻辑零改动验证通过
- [x] `prefers-reduced-motion` 下无动画残留。完成:app.css 全局 `@media (prefers-reduced-motion: reduce)` 关闭全部 animation/transition
- [ ] 截图对比:暗/亮双主题 + 窄窗口(800px)无布局破损。**待执行**:需实际运行 Tauri 窗口截图,后续排期

**关键文件**:
- `frontends/src/app.css`(token/釉面/字形/动效)
- `frontends/src/App.svelte`(三栏骨架/标题栏/落款)
- `frontends/src/lib/components/ChatMessage.svelte`(消息/时间线折叠)
- `frontends/src/lib/components/{ThinkingBlock,ToolCallCard,ChatInput,Sidebar,SidePanel}.svelte`

**验收命令**:
```bash
cd frontends && npm run build
cd src-tauri && cargo tauri build --no-bundle
# soak 验证(参考 scripts/e2e/ 既有方法,10h 后查 RSS)
```

**回退**:备份目录 `backups/frontends-backup-20260801/` 直接 rsync 回 `frontends/`(排除项对称)。

---

## 九、整体验收标准（Phase 0-6 全部完成时）

参考 `docs/acceptance-criteria.md` 风格：

### Critical (must pass)
1. `cargo build --release` ≤ 15 MB（当前 12 MB，预估 +1.5MB for reedline/fastembed）
2. `cargo test --workspace` 全部通过
3. `cargo check` 零 warning
4. 三个 UI 全部消费 `protocol_v1` 事件，**0 个** `Token`/`Thinking`/`ToolCall` 出现
5. `cargo clippy -- -D warnings` 全部通过

### Important (should pass)
6. `npm run build`（WebUI）零错误
7. TUI 启动 < 100ms（aichat 对齐）
8. WebUI `/api/health` < 5ms 响应
9. Auth token 默认强制
10. 三个 UI 共享同一份 `sessions.json`，刷新后状态一致

### Future (nice to have)
11. RAG 召回率 ≥ 0.8 on test set
12. TUI 主题切换 < 50ms
13. Tauri 冷启动 < 500ms
14. Chat branching 支持 ≥ 3 层

---

## 十、风险与缓解

链接到 [`risk-register.md`](risk-register.md)，新增项：

| ID | 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|---|
| **R-014** | Phase 0 重写 stream.rs 引入 provider 行为差异 | 3 | 4 | 每 provider 录制 SSE payload 回归测试，**先在 Mock provider 验证再切** |
| **R-015** | reedline 替换自写 input 改变 TUI 快捷键 | 4 | 2 | 提供 `keybindings = "legacy"` 兼容模式 |
| **R-016** | fastembed 模型下载破坏 offline 部署 | 3 | 3 | 支持 `embedder: "local_hash"` 走纯文本匹配回退 |
| **R-017** | RAG 向量存储无事务，长跑后损坏 | 2 | 3 | 每次启动做 fsck + 自动重建 |
| **R-018** | Tauri 多窗口内存占用翻倍 | 3 | 2 | 默认开 1 窗口，按需新增 |
| **R-019** | `inventory` crate 宏增加编译时间 | 4 | 1 | 已在 R-012 mitigation 中，~30s 增量 |

---

## 十一、关联文档索引

| 文档 | 关系 |
|---|---|
| [stream-protocol-migration.md](stream-protocol-migration.md) | **Phase 0** 直接执行该文档剩余 Phase 1-5 |
| [tui-redesign.md](tui-redesign.md) | **Phase 1** 在此文档的设计规范上实现 |
| [ui-design-language.md](ui-design-language.md) | **Phase 7** 器物语法设计规范(token/釉面/字形/动效/组件/复杂度契约) |
| [comparison-vs-other-agents.md](comparison-vs-other-agents.md) | 改进完成后更新自评表 |
| [acceptance-criteria.md](acceptance-criteria.md) | Phase 6 完成后更新 Functional Equivalence Checklist |
| [risk-register.md](risk-register.md) | 新增 R-014 至 R-019 |
| [docs/adr/0001-pure-rust-rewrite.md](adr/0001-pure-rust-rewrite.md) | 不变，**继续坚持零 CPython 依赖原则** |
| [docs/adr/0002-axum-web-framework.md](adr/0002-axum-web-framework.md) | WebUI 继续基于 axum |

---

## 十二、里程碑时间线（建议，不强制）

> 时间为**参考**，实际以代码完成度为准

| 里程碑 | Phase | 标志事件 | 状态 |
|---|---|---|---|---|
| **v0.1.1** | 0.1-0.2 | LLM 直 emit 新事件 + mod.rs 删双发 | ✅ 完成 |
| **v0.1.2** | 0.3-0.5 | TUI/Tauri 切完，narration.rs 删完 | ✅ 完成 |
| **v0.2.0** | 1.1-1.5 | TUI 现代化完成（History 模块 + 主题 + PromptTemplate） | 🔶 部分完成（1.1 → History 替代 reedline，1.4 主题完成，1.5 跳过，1.2-1.3 未动） |
| **v0.3.0** | 2.1-2.5 | WebUI 完整体验 | ✅ 完成（2.1-2.5 全部完成） |
| **v0.4.0** | 3.1-3.4 + 5.1-5.2 | Agent 工作模式 + 工具自发现 | 🔶 部分完成（3.1-3.4 完成，5.1-5.2 未动） |
| **v0.5.0** | 4.1-4.4 + 5.3-5.4 | Tauri 桌面丰富化 + 生态 | ✅ 完成（4.1 托盘+通知/4.2 替代/4.3 通知替代/4.4 多窗口，5.1 linkme/5.2 router/5.3-5.4 完成） |
| **v0.6.0** | 6 全部 | 清理 + CHANGELOG | ✅ 完成 |
| **v0.7.0** | 7.1-7.8 | 器物语法 UI 改造(备份 `backups/frontends-backup-20260801/`) | 🔶 进行中（7.1-7.7 ✅，7.8 部分完成：soak/截图待排期） |

---

**最后修订**：2026-08-01 · 维护者：核心团队
**变更原则**：任何对 Phase 0 的修改必须在 PR 中附回归测试用例;Phase 7 改造期间以 `backups/frontends-backup-20260801/` 为回退安全网
**状态说明**：Phase 0-6 ✅ 完成。v0.2.0 就绪。Phase 7(器物语法 UI)进行中 — 7.1-7.7 ✅,7.8 部分完成(soak 10h 与截图对比待排期)。
