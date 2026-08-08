# OpenZen Tauri 桌面端 UI & 键盘设计

> 生成日期：2026-06-20 · 修订：2026-06-20（Phase A/B/C 实现完成）
> 状态：✅ 已实现（Phase A/B/C 全部完成）
> 配套阅读：`roadmap.md` · `stream-protocol-migration.md` · `DESIGN.md` · `AGENTS.md`
>
> **实现完成总结**（2026-06-20）：
> - Phase A：ContextBar ✅ · 时间线折叠 ✅ · DiffWords 行内词级 diff ✅ · 键盘快捷键 ✅
> - Phase B：`/compact` 命令 ✅ · 自动压缩通知 ✅（TransientsBar 挂载 + backend emit）
> - Phase C：todowrite/todoupdate 工具 ✅ · data_todo_update 事件 ✅ · TodoProgress + TodoDetailBubble ✅

---

## 一、概述

本文档定义 OpenZen Tauri 桌面端 UI 的四个核心功能模块：

1. **上下文栏** — 会话上下文使用量的实时可视化
2. **Todo 追踪** — 长程任务的代办事项跟踪
3. **活动时间线折叠** — Agent 单轮回复中过多内容项的自动折叠
4. **代码 Diff 视图** — 代码修改的前端可视化 diff

以及配套的**键盘快捷键体系**和**`/compact` 命令**。

所有功能均遵循 `DESIGN.md` 定义的 Song Celadon 设计语言：
- 暖黑色画布 `#181715`，珊瑚色 `#cc785c` 点缀
- 无阴影，用 surface 色阶变化表达层级
- Inter + JetBrains Mono 字族
- 不使用 emoji，用 SVG 图标

---

## 二、顶部上下文栏

### 2.1 触发方式

始终显示在聊天消息区域最顶部，不再使用空格键 toggle。

### 2.2 显示位置

```
┌──────────────────────────────────────────┐
│ Sidebar │ 上下文栏 (始终显示)             │
│          │ 上下文: ████████░░░░ 65%       │
│          │ (21K / 32K) · 输入: 12.5K     │
│          │ · 输出: 4.8K                  │
│          ├────────────────────────────────┤
│          │  ChatMessage                   │
│          │  ChatMessage                   │
│          │  ...                           │
│          │  ChatInput                     │
│          ├────────────────────────────────┤
│          │  model-bar                     │
└──────────────────────────────────────────┘
```

### 2.3 数据来源

| 字段 | 来源 | 存在状态 |
|------|------|---------|
| `contextUsed` | 当前 session 已使用的 context tokens | `Message.contextUsed` 已定义但未填充 |
| `contextWindow` | 模型总上下文窗口大小，来自 `ModelInfo.contextWindow` | 由 `model_info` SSE 事件动态传入，每个模型不同（如 128K / 256K / 1M） |
| `tokensIn` | session 输入总 token 数 | `aggregateTokens()` 已有 |
| `tokensOut` | session 输出总 token 数 | `aggregateTokens()` 已有 |
| 实时 token 表 | `data_token_meter` 事件 | 协议已定义，需后端 emit |

> **注意：** `contextWindow` 不是固定值。文档中 "32K" 仅为示例，实际渲染时读取 `ModelInfo.contextWindow`（`stores/types.ts:10`），由 `model_info` SSE 事件（`chat.ts:477`）动态更新。

### 2.4 进度条颜色规则

```
< 70%  → var(--success) (#5db872)
70-90% → var(--warning) (#d4a017)
> 90%  → var(--error)   (#c64545)
```

进度条使用纯 CSS 实现（`background: linear-gradient(to right, ...)`），无需 JS 计算。

### 2.5 压缩状态通知

当后端触发自动压缩（`agent_loop.rs` 中调用 `compress_messages()`）时，emit `data_compressing_context` 事件。

> 📋 **实现现状**：✅ 已完成。前端 `TransientsBar` 已挂载到 `App.svelte`。后端 `StreamEvent::DataCompressingContext` 已在 `event.rs` 定义，`agent_loop.rs` 在每次压缩时 emit 事件（携带 before_tokens/after_tokens/saved_tokens）。

前端 `TransientsBar.svelte` 显示 4 秒自动消失的通知：

```
⚡ 上下文压缩: 124K → 18K tokens
```

如果压缩发生多次，上下文栏尾部显示压缩计数：

```
上下文: ████████░░░░ 65% (21K / 32K) · 输入: 12.5K · 输出: 4.8K · ⚡已压缩3次
```

### 2.6 改动文件

| 文件 | 改动 | 现状 |
|------|------|------|
| `frontends/src/App.svelte` | 在 `.chat-container` 顶部插入 ContextBar；挂载 TransientsBar | ✅ 已完成 |
| `frontends/src/lib/components/ContextBar.svelte` | **新建** — 上下文栏组件 | ✅ 已完成 |
| `frontends/src/lib/stores/chat.ts` | 处理 `data_compressing_context` 事件更新压缩计数（通过 TransientsBar） | ✅ 已完成（前端协议层处理） |
| `crates/ga-core-types/src/event.rs` | **新增** `DataCompressingContext` 变体 | ✅ 已完成 |
| `crates/ga-core/src/agent_loop.rs` | 压缩处 emit `StreamEvent::DataCompressingContext` | ✅ 已完成 |

---

## 三、`/compact` 命令

### 3.1 命令格式

```
/compact
```

在 ChatInput 中输入 `/compact` 触发当前 session 的上下文手动压缩。

### 3.2 行为

1. 前端调用后端 API 触发压缩
2. 后端读取当前 session 的 messages → 反序列化 → 调用 `compress_messages()` → 写回 → 返回 before/after 统计
3. 前端在 chat 中显示一条 assistant 消息报告结果

### 3.3 示例输出

```
Agent:
  ⚡ 上下文压缩完成
  压缩前: 85,320 chars · 压缩后: 21,140 chars
  释放: 64,180 chars (75.2%)
  压缩策略: 摘要 tool 结果 (24K) + 丢弃 3 条最旧消息对
```

### 3.4 改动文件

> 📋 **实现现状**：✅ 5/5 全部完成。

| 文件 | 改动 | 现状 |
|------|------|------|
| `src-tauri/src/lib.rs` | 新增 `compress_session` Tauri command | ✅ 已完成 |
| `crates/ga-server/src/webui/mod.rs` | 新增 `POST /api/sessions/:id/compress` | ✅ 已完成 |
| `frontends/src/lib/api/sessions.ts` | 新增 `compressSession(id)` 函数 | ✅ 已完成 |
| `frontends/src/lib/components/ChatInput.svelte` | `handleCommand` 新增 `/compact` case | ✅ 已完成 |
| `crates/ga-server/src/webui/sessions.rs` | `get_mut()` 已存在，直接调用 `compress_messages()` | ✅ 已有（无额外改动） |

### 3.5 Tauri Command 签名

```rust
#[tauri::command]
fn compress_session(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> serde_json::Value {
    let mut store = state.sessions.lock().unwrap();
    let entry = store.get_mut(&id)?;
    // 1. Deserialize messages to Vec<Message>
    // 2. Call compress::compress_messages()
    // 3. Serialize back
    // 4. Save
    // 5. Return { before, after, saved, strategy }
}
```

---

## 四、Todo 追踪

### 4.1 数据模型

```typescript
// frontends/src/lib/stores/types.ts

export interface TodoItem {
  id: string;
  content: string;           // "src/validation.ts: 添加邮箱验证函数 - 期望返回 boolean"
  status: 'pending' | 'in_progress' | 'completed' | 'cancelled';
  priority: 'high' | 'medium' | 'low';
  order: number;
  createdAt: string;
  updatedAt: string;
}
```

### 4.2 Protocol 事件

协议层新增 `data_todo_update` 事件：

```typescript
// frontends/src/lib/stores/parts.ts — ProtocolV1Event 新增变体

{ type: 'data_todo_update'; items: TodoItem[]; current: number; total: number }
```

payload 示例：

```json
{
  "type": "data_todo_update",
  "items": [
    { "id": "1", "content": "src/validation.ts: 添加邮箱验证函数", "status": "completed", "priority": "high", "order": 1 },
    { "id": "2", "content": "src/handler.ts: 接入验证逻辑", "status": "in_progress", "priority": "high", "order": 2 },
    { "id": "3", "content": "tests/test_validation.ts: 编写测试", "status": "pending", "priority": "medium", "order": 3 }
  ],
  "current": 2,
  "total": 3
}
```

### 4.3 状态流转

```
pending → in_progress → completed
                      ↘ cancelled
```

视觉状态映射：
- `completed` → `--success` (#5db872) + checkmark SVG
- `in_progress` → `--warning` (#d4a017) + spinner SVG（旋转动画）
- `pending` → `--dim` (#5e5b52) + circle SVG
- `cancelled` → `--muted` (#8a877d) + strikethrough

### 4.4 UI 布局

```
消息气泡完整视图：

┌─ Agent Message bubble ───────────────────────┐
│  ... content ...                              │
│                                               │
│  ┌─ bubble-footer ──────────────────────────┐ │
│  └──────────────────────────────────────────┘ │
│                                               │
│  ┌─ todo-progress (折叠卡片) ───────────────┐ │
│  │  ◉  代办事项 4/7 · 2 completed ·         │ │
│  │  1 in progress · 1 pending             ▶ │ │
│  └──────────────────────────────────────────┘ │
│           ↓ 点击展开 / 自动折叠                │
└───────────────────────────────────────────────┘
         ┌─ sidecar bubble (右侧) ─────────────┐
         │  ◉ 代办事项 (2/7)                   │
         │                                     │
         │  ✅ src/validation.ts               │
         │    添加邮箱验证函数                  │
         │  ◉ src/handler.ts                   │
         │    接入验证逻辑             ← 进行中 │
         │  ⬜ tests/test.ts                    │
         │    编写测试                          │
         │  ⬜ docs/api.md                      │
         │    补充 API 文档                     │
         └─────────────────────────────────────┘
```

### 4.5 交互规则

| 操作 | 行为 |
|------|------|
| 消息 finalized（streaming=false）且 todos 不为空 | 显示 todo-progress 折叠卡片 |
| 点击折叠卡片 | 右侧弹出 sidecar bubble 显示全部 todo |
| 点击 sidecar 的 ✕ 按钮 | 关闭 sidecar，回到折叠卡片状态 |
| 再次点击折叠卡片 | 重新打开 sidecar |
| 窄屏（< 1100px） | sidecar 改为在消息下方原地展开，而非右侧浮出 |
| 消息流式过程中新增 todo 变化 | 实时更新折叠卡片的计数和状态颜色 |

### 4.6 后端 todo 工具（待设计）

后端新增两个工具供 Agent 模型调用：

```
todowrite(content: string, priority?: 'high' | 'medium' | 'low')
  → 创建一条 pending 状态的新 todo

todoupdate(id: string, status: 'in_progress' | 'completed' | 'cancelled')
  → 更新指定 todo 的状态
```

工具调用触发 `data_todo_update` 事件流式透传到前端。

### 4.7 改动文件

> 📋 **实现现状**：✅ 10/10 全部完成。

| 文件 | 改动 | 现状 |
|------|------|------|
| `crates/ga-tools/src/` | 新增 `todowrite`、`todoupdate` 工具 | ✅ 已完成 |
| `crates/ga-core-types/src/event.rs` | 新增 `TodoItem` struct + `DataTodoUpdate` 事件变体 | ✅ 已完成 |
| `crates/ga-core/src/agent_loop.rs` | 拦截 todo 工具、更新 WorkingMemory.todos、emit DataTodoUpdate | ✅ 已完成 |
| `crates/ga-core/src/handler.rs` | `WorkingMemory` 新增 `todos` 字段 | ✅ 已完成 |
| `frontends/src/lib/stores/parts.ts` | `ProtocolV1Event` 新增 `data_todo_update` | ✅ 已完成 |
| `frontends/src/lib/stores/protocol-processor.ts` | 新增 `data_todo_update` 处理分支（转发到 chat.ts） | ✅ 已完成 |
| `frontends/src/lib/stores/chat.ts` | `ChatState` 新增 `todos` 数组 + `setTodos()` 方法 | ✅ 已完成 |
| `frontends/src/lib/components/TodoProgress.svelte` | **新建** — 折叠卡片组件 | ✅ 已完成 |
| `frontends/src/lib/components/TodoDetailBubble.svelte` | **新建** — 侧边气泡组件 | ✅ 已完成 |
| `frontends/src/lib/components/ChatMessage.svelte` | 在 bubble-footer 下方插入 TodoProgress（仅 finished 消息可见） | ✅ 已完成 |

---

## 五、活动时间线折叠

### 5.1 触发条件

Agent 单轮回复中，`parts` 数组内**所有类型的项**（thinking、tool-invocation、text）总数超过 **5 个**时，自动将最早的超出部分折叠进"活动时间线"标题下。

### 5.2 折叠规则

```
流式过程中实时折叠：

第 1-5 项到达：全部正常显示（未超过阈值）
第 6 项到达：第 1 项被推入折叠区，保留第 2-6 项可见
第 7 项到达：第 2 项被推入折叠区，保留第 3-7 项可见
...

规则：始终保留最新的 5 项可见，最早进入的被折叠
```

### 5.3 折叠后的 UI

```
折叠状态（默认）：

┌─ Agent Message ────────────────────────────┐
│ ▶ 活动时间线 (N-5项已折叠 · 47s · 🔧12)   │
│                                             │
│ [工具 N-4]                                  │
│ [思考 N-3]                                  │
│ [工具 N-2]                                  │
│ [文本 N-1]                                  │
│ [工具 N]  ← 最新                           │
│                                             │
│ ┌─ bubble-footer ────────────────────────┐  │
│ └────────────────────────────────────────┘  │
└─────────────────────────────────────────────┘
          ↓ 点击标题展开

展开状态：

┌─ Agent Message ────────────────────────────┐
│ ▼ 活动时间线 (全部 N 项)                    │
│                                             │
│ [思考 0]  ← 最早（原被折叠）                │
│ [工具 1]                                    │
│ [文本 2]                                    │
│ ...                                         │
│ [工具 N]  ← 最新                           │
│                                             │
│ ┌─ bubble-footer ────────────────────────┐  │
│ └────────────────────────────────────────┘  │
└─────────────────────────────────────────────┘
```

### 5.4 折叠标题信息密度

```
▶ 活动时间线 (8项已折叠 · 12.3s · 🔧3)
```

- **8项已折叠** = 折叠区内 item 数量
- **12.3s** = 所有折叠项的总耗时（各 item `durationMs` 之和）
- **🔧3** = 其中 tool call 的数量
- 如果折叠区中有 item 的 `state === 'output-error'` → 标题红色警告标识

### 5.5 实现方案

```typescript
// ChatMessage.svelte

let timelineExpanded = $state(false);

let totalItems = $derived(parts.length);
let hasOverflow = $derived(totalItems > 5);

let visibleParts = $derived(
  hasOverflow && !timelineExpanded
    ? parts.slice(-5)   // 最近 5 项
    : parts              // 全部
);

let foldedParts = $derived(
  hasOverflow && !timelineExpanded
    ? parts.slice(0, -5)  // 前 N-5 项
    : []
);
```

所有 item 类型平等计数：thinking、tool-invocation、text 都算 1 项。不存在某类型"权重更高"。

### 5.6 改动文件

> 📋 **实现现状**：✅ 已完成。ChatMessage.svelte 新增 `timelineExpanded`、`foldedGroups`/`visibleGroups`、`foldedStats`、TimelineHeader 渲染及 CSS。

| 文件 | 改动 | 现状 |
|------|------|------|
| `frontends/src/lib/components/ChatMessage.svelte` | 新增 `timelineExpanded` 状态、`visibleParts`/`foldedParts` 计算、TimelineHeader 渲染、展开/折叠条件渲染 | ✅ 已完成 |

---

## 六、代码 Diff 视图

> 📋 **实现现状**：EditCard **已有自研 LCS diff**（`EditCard.svelte:77-186`），渲染 removed/added/context 三色行。**不需要 `diff` npm 包。** 仅缺失行内词级 diff。

### 6.1 现状

`EditCard.svelte` 已内置两级 diff 算法：
- **`lcsDiff()`**（line 103-165） — 完整 LCS DP 表 + 回溯，适用于 ≤500 行文件
- **`simpleDiff()`**（line 167-186） — 大文件回退方案，将所有旧行标 removed、新行标 added

展开 EditCard 后，diff 以统一表格式渲染：
- 删除行：红色背景 `rgba(220,90,90,0.07)`，红色 `-` 标记，红色文字
- 新增行：绿色背景 `rgba(101,184,145,0.07)`，绿色 `+` 标记，绿色文字
- 相同行（context）：正常无背景，行号半透明
- 行号列（`diff-old-num` / `diff-new-num`）

**当前可用视图**：
```
  ┌─ ▼ Edit  src/validation.ts  [Pasted ~4 lines] ──────────────┐
  │  1  - 2   - function foo() {                                │ ← 删除行（红色）
  │      + 2   + function foo(bar: string) {                    │ ← 新增行（绿色）
  │  2     3     const x = 1;                                   │ ← 相同行
  │  3     4     return x;                                      │ ← 相同行
  └──────────────────────────────────────────────────────────────┘
```

### 6.2 缺失：行内词级 diff

当前为纯行级 diff。对于单行内的局部修改（如参数名变更），用户看到的是"整行删 + 整行加"，而非行内高亮。需要补充 word-level diff：

```diff
- function foo() {               →   - function -foo-() {
+ function foo(bar: string) {    →   + function **foo(bar: string)**() {
                                    (行内删除词 strikethrough，新增词 bold)
```

这不需要 `diff` npm 包。可在现有 `computeDiff()` 中，对 adjacent removed+added 行对做 `diffWords()` 细分（约 30 行自研实现）。

### 6.3 改动文件

| 文件 | 改动 | 现状 |
|------|------|------|
| `frontends/src/lib/components/EditCard.svelte` | 新增 `diffWords()` LCS 词级 diff + `wordDiffTokens` derived + 行内高亮渲染（removed/added/context span） | ✅ 已完成 |

**验证**：`node scripts/verify_diffwords.mjs` — 10/10 pass。

---

## 七、键盘快捷键体系

### 7.1 设计原则

1. **Cmd/Ctrl + key** — 全局可用，包括 ChatInput 聚焦时（不干扰打字）
2. **无修饰键的单键** — 仅在 ChatInput **未聚焦**时可用
3. **Escape** — 统一退出/取消
4. 所有对话框支持 `Escape` 关闭 + `Tab` 循环焦点

### 7.2 快捷键表格

> 📋 **实现现状**：✅ 已完成。所有快捷键已通过 `App.svelte` 的 `handleGlobalKeydown` 实现。SessionList 已支持 ArrowUp/Down/Enter/Delete/Escape 键盘导航。ShortcutsPanel 已创建并通过 `Cmd+/` 触发。

#### 全局快捷键（ChatInput 聚焦时也可用）

| 快捷键 | 功能 | 实现 |
|--------|------|------|
| `Cmd+N` / `Ctrl+N` | 新建 chat | `App.svelte` → `handleNewChat()` |
| `Cmd+[` / `Ctrl+[` | 上一个 session | `sessions.previous()` |
| `Cmd+]` / `Ctrl+]` | 下一个 session | `sessions.next()` |
| `Cmd+Shift+S` / `Ctrl+Shift+S` | 切换侧边栏 | `toggleSidebar()` |
| `Cmd+/` / `Ctrl+/` | 快捷键帮助面板 | 弹出 ShortcutsPanel |
| `Cmd+Shift+D` / `Ctrl+Shift+D` | 删除当前 session | `sessions.remove()` + `handleNewChat()` |

#### 仅 ChatInput 未聚焦时可用

| 快捷键 | 功能 |
|--------|------|
| `C` | 复制最后一条 assistant 消息内容 |
| `R` | 重新生成最后一条 assistant 消息 |
| `↑` | 将 focus 移到侧边栏（侧边栏关闭时无操作） |

#### 侧边栏聚焦时

| 快捷键 | 功能 |
|--------|------|
| `↑` / `↓` | 上下切换选中的 session |
| `Enter` | 打开选中的 session |
| `Delete` / `Backspace` | 删除选中的 session |
| `Escape` | 焦点回到 ChatInput |

#### 对话框通用

| 快捷键 | 功能 |
|--------|------|
| `Escape` | 关闭对话框/弹窗 |
| `Enter` | 提交/确认 |
| `Tab` / `Shift+Tab` | 焦点前/后移动 |

### 7.3 快捷键帮助面板

`Cmd+/` 触发弹出 ShortcutsPanel：

```
┌─── 键盘快捷键 ────────────────────────────┐
│                                            │
│  全局                                       │
│  Cmd+N       新建会话                       │
│  Cmd+[ / ]   上一个/下一个会话              │
│  Cmd+Shift+S 切换侧边栏                     │
│  Cmd+/       显示本面板                     │
│  Cmd+Shift+D 删除当前会话                   │
│                                            │
│  消息操作（输入框未聚焦时）                  │
│  C           复制最后一条回复               │
│  R           重新生成                       │
│                                            │
│  输入框                                     │
│  Enter       发送消息                       │
│  Shift+Enter 换行                           │
│  Escape      取消当前任务                   │
│  /xxx        命令                           │
│                                            │
│  [关闭]                                     │
└────────────────────────────────────────────┘
```

### 7.4 侧边栏键盘导航实现

```
SessionList.svelte:

新增状态:
  - focusedIndex: number (当前焦点所在的 session index)
  - showFocus: boolean (侧边栏是否获得键盘焦点)

新增方法:
  - focusNext() → focusedIndex++
  - focusPrev() → focusedIndex--
  - selectFocused() → onSelectSession(items[focusedIndex].id)
  - deleteFocused() → sessions.remove(items[focusedIndex].id)

CSS:
  .session-item.focused { outline: 2px solid var(--color-primary); }
```

从 ChatInput 用 `↑` 或 `Cmd+Shift+S` 进入侧边栏后，焦点在侧边栏内循环。按 `Escape` 回到 ChatInput。

### 7.5 改动文件

| 文件 | 改动 | 现状 |
|------|------|------|
| `frontends/src/App.svelte` | `<svelte:window onkeydown={handleGlobalKeydown} />` + 全部快捷键处理 | ✅ 已完成 |
| `frontends/src/lib/components/SessionList.svelte` | 新增键盘导航（ArrowUp/Down/Enter/Delete/Escape + focusedIndex） | ✅ 已完成 |
| `frontends/src/lib/components/ShortcutsPanel.svelte` | **新建** — 快捷键帮助面板（`Cmd+/` 触发） | ✅ 已完成 |
| `frontends/src/lib/stores/sessions.ts` | 新增 `previous()`、`next()` 方法 | ✅ 已完成 |
| `frontends/src/lib/components/Sidebar.svelte` | 新增 `onSidebarEscape` prop 透传到 SessionList | ✅ 已完成 |

---

## 八、实现路线图

> **状态：✅ 全部完成（2026-06-20）**

```
Phase A — 纯前端，无后端依赖 ✅
├── ✅ 顶部上下文栏（ContextBar.svelte + App.svelte 挂载）
├── ✅ 活动时间线折叠（ChatMessage.svelte: foldedGroups/visibleGroups/TimelineHeader）
├── ✅ 代码 Diff 视图（EditCard.svelte: diffWords() 行内词级 LCS diff）
└── ✅ 键盘快捷键体系（App.svelte: handleGlobalKeydown + SessionList 导航 + ShortcutsPanel）

Phase B — 轻量后端改动 ✅
├── ✅ /compact 命令（Tauri compress_session + HTTP /compress + ChatInput case）
└── ✅ 自动压缩通知（agent_loop.rs emit DataCompressingContext + TransientsBar 挂载）

Phase C — 完整 todo 追踪 ✅
├── ✅ todowrite / todoupdate 工具（ga-tools 注册 + linkme）
├── ✅ data_todo_update protocol 事件（event.rs + parts.ts + protocol-processor.ts）
├── ✅ TodoProgress 折叠卡片
└── ✅ TodoDetailBubble 侧边气泡
```

### 验证状态

| 检查项 | 结果 |
|--------|------|
| `cargo check` | 通过 |
| `cargo test -p ga-core` | 104/104 pass |
| 前端 LSP（24 Svelte 文件） | 0 errors |
| `node scripts/verify_diffwords.mjs` | 10/10 pass |

### 新增文件清单

```
frontends/src/lib/components/ContextBar.svelte       (new)
frontends/src/lib/components/ShortcutsPanel.svelte   (new)
frontends/src/lib/components/TodoProgress.svelte     (new)
frontends/src/lib/components/TodoDetailBubble.svelte (new)
crates/ga-tools/src/todowrite.rs                     (new)
crates/ga-tools/src/todoupdate.rs                    (new)
scripts/verify_diffwords.mjs                         (new)
docs/phase-a-verification.md                         (new)
docs/phase-b-verification.md                         (new)
```

### 文档索引

| 参考文件 | 内容 |
|---------|------|
| `DESIGN.md` | Song Celadon 设计系统（颜色、字体、间距、组件风格） |
| `AGENTS.md` | Tauri E2E 驱动技巧、坐标、SSE 事件流 |
| `roadmap.md` | 项目整体路线图（v0.1-v0.6） |
| `stream-protocol-migration.md` | StreamEvent 协议迁移详情 |
| `acceptance-criteria.md` | 功能验收标准 |
| `comparison-vs-other-agents.md` | Rust GA 与其他框架对比 |
