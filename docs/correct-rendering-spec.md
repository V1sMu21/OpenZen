# OpenZen Tauri 桌面端正确渲染规范 v1.0

> 生成日期：2026-06-21
> 用途：**修改 ChatMessage.svelte、chat.ts、App.svelte 等渲染相关文件时的必读文档**
> 原则：每次修改必须对照本文档描述的正确渲染行为验证，不可破坏已正确的功能
>
> 关联 Bug 记录：
> - `2026-06-21`: `isLive = storeIsProcessing && message.role === "assistant"` 无"是否为最后一条消息"检查 → 跨轮内容泄漏
> - `2026-06-21`: `collapsed={true}` 字面量 + 组件重建 → 卡片无法展开
> - `2026-06-21`: `hasFinished = message.streaming !== true || ...` → Running 状态被 OR 关系淹没

---

## 一、数据流架构（必读）

### 1.1 核心状态（chat store）

```
ChatState {
  messages: Message[]       // 所有消息（user + assistant）
  isProcessing: boolean     // 后端是否正在处理。这是唯一权威的"处理中"信号
  streamingParts: UIMessagePart[]  // 全局单例数组，只属于当前正在流式输出的一轮
  modelInfo: { model, provider, contextWindow, isLocal }
  todos: TodoItem[]
  pendingAskUser: PendingAskUser | null
}
```

### 1.2 消息生命周期

```
用户发送消息
    ↓
addUserMessage(text)   →  messages.push(userMsg)
  isProcessing = true   →  后端开始工作
  streamingParts = []   →  清空上一轮的 parts
    ↓
startAssistantMessage() →  messages.push(asstMsg { streaming: true, duration: undefined, parts: undefined })
    ↓
后端发送 protocol_v1 事件 → applyProtocolEvent(streamingParts, event)  每一帧更新 streamingParts
  handleSSEEvent 也会同步更新 messages[last].content
    ↓
后端发送 done 事件
    ↓
finalizeAssistantMessage()
  messages[last].streaming = false
  messages[last].parts = finalParts     ← 只在这一步，parts 才写入 message
  messages[last].duration = 实际耗时
  isProcessing = false
  streamingParts = []
```

### 1.3 关键设计约束

| 约束 | 说明 |
|---|---|
| `streamingParts` 是全局单例 | chat store 里只有一个 `streamingParts` 数组，不属于任何特定 message。所有 ChatMessage 组件通过 `chat.subscribe()` 读到的是同一个引用 |
| `message.parts` 只在 finalize 时写入 | 流式过程中 `message.parts` 为 `undefined`。只有 `finalizeAssistantMessage()` 才把 `streamingParts` 快照写入 `message.parts` |
| `isProcessing` 是唯一权威信号 | 判断"后端是否还在工作"只能用 `isProcessing`。`message.streaming`、`message.duration`、`message.exitReason` 都是从磁盘或 finalize 时写入的字段，**流式期间不可信** |
| `message.id` 格式为 `{sessionId}-msg-{idx}` | 切换会话时所有 `msg.id` 都变了，Svelte keyed each 会销毁全部 ChatMessage 并重建新的 |

---

## 二、时间态一：实时交互（用户发送消息，Agent 响应）

### 2.1 子态 A：用户刚按下发送（0ms ~ 第一个 protocol 事件到达前）

```
┌──────────────────────────────────────────────────────────────┐
│  Sidebar  │  会话标题                                        │
│           │                                                   │
│  ┌── User Message ──┐                                         │
│  │ YOU  复制        │                                         │
│  │ "用户的消息文本"   │                                         │
│  │  15:23            │                                         │
│  └───────────────────┘                                         │
│                                                                │
│  ┌── Assistant Message ── (新建，streaming=true) ────────────┐ │
│  │ AGENT  ● RUNNING    复制                                  │ │
│  │                                                            │ │
│  │   (气泡内容为空，没有卡片，没有文字)                          │ │
│  │   (如果没有 showTyping，则显示 typing dots: ● ● ●)         │ │
│  │                                                            │ │
│  │ ┌─ bubble-footer ────────────────────────────────────────┐│ │
│  │ │ ● RUNNING    🕐 15:23    Total 225ms    0 OUT   0 IN   ││ │
│  │ └────────────────────────────────────────────────────────┘│ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                │
│  ┌── ChatInput ──────────────────────────────────────────────┐ │
│  │ "Type a message..."                    [■ Stop]             │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                │
│  ┌── model-bar ──────────────────────────────────────────────┐ │
│  │ omlx/Qwen3.6-35B-A3B-8bit  Online  5msgs  1.2Kout  6.0Kin│ │
│  │ Context: 6.0K/200K ████░░░░ 3%                              │ │
│  └────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

**正确渲染状态：**

| 元素 | 正确值 | 来源 |
|---|---|---|
| header pill | `● RUNNING` (琥珀色 #f59e0b，pulse 动画) | `isRunning` derived = true |
| footer pill | `● RUNNING` (同上) | `isBackendStillWorking` derived = true |
| 时间 | 消息开始时的时间（如 `15:23`） | `message.timestamp` |
| 计时 | 从 0ms 开始递增（ticker 每 250ms 更新） | `liveElapsedMs` derived (tickerState.now - timestamp) |
| 卡片 | 无（`streamingParts` 为空） | `parts` derived → `storeStreamingParts.filter(...)` → `[]` |
| 文字 | 无（或有 typing dots） | `liveStreamingText` derived = `""` |
| Stop 按钮 | 显示红色方块 | `$chat.isProcessing === true` |
| 底部 model-bar | 模型名、Online/Local、消息数、IN/OUT tokens、Context 百分比 | `$chat.modelInfo` + `aggregateTokens` |

**关键 derived 值：**

```ts
storeIsProcessing = true
isLive = true          // 这是当前最后一条 assistant 消息
isBackendStillWorking = true
isRunning = true
hasFinished = false
showTyping = true (如果没有文字)
liveElapsedMs = 从 timestamp 开始递增
```

---

### 2.2 子态 B：Agent 正在流式输出（第一个 protocol 事件 → 最后一个协议事件）

**第 1 个 Thinking 卡片出现时：**

```
┌── Assistant Message ── (streaming) ──────────────────────────┐
│ AGENT  ● RUNNING    复制                                      │
│                                                                │
│ ┌─ ▼ Thinking (8 words) ────────────────────────────────────┐ │
│ │  "Let me think about this carefully..."                    │ │
│ └────────────────────────────────────────────────────────────┘ │
│                                                                │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ● RUNNING    🕐 15:23    Total 3.2s    0 OUT   0 IN       │ │
│ └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

**工具卡片出现时：**

```
┌── Assistant Message ── (streaming) ──────────────────────────┐
│ AGENT  ● RUNNING    复制                                      │
│                                                                │
│ ┌─ ▼ Thinking (22 words) ───────────────────────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│ ┌─ ▶ Run Code  # bash fib(40)  13ms Done ───────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│ ┌─ ▼ Thinking (9 words) ────────────────────────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│                                                                │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ● RUNNING    🕐 15:23    Total 5.5s    0 OUT   0 IN       │ │
│ └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

**回复文字流式输出时：**

```
┌── Assistant Message ── (streaming) ──────────────────────────┐
│ AGENT  ● RUNNING    复制                                      │
│                                                                │
│ ┌─ ▼ Thinking (22 words) ───────────────────────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│ ┌─ ▶ Run Code  # bash fib(40)  13ms Done ───────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│ ┌─ ▼ Thinking (9 words) ────────────────────────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│                                                                │
│ fib(40) = 102334155                                            │
│                                                                │
│ 矩阵快速幂求 Fibonacci — O(log n)                               │
│                                                          ▍     │ ← StreamingText 实时渲染
│                                                                │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ● RUNNING    🕐 15:23    Total 8.5s    0 OUT   0 IN       │ │
│ └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

**长任务长时间无新卡片时（过去几十秒都没有新的 Thinking 或工具卡片）：**

```
┌── Assistant Message ── (streaming) ──────────────────────────┐
│ AGENT  ● RUNNING    复制                                      │
│                                                                │
│ （如果上一个卡片出现了，且还在 stream 状态，应当保持可见）        │
│                                                                │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ● RUNNING    🕐 15:23    Total 45.2s   0 OUT   0 IN       │ │
│ └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

> ⚠️ **关键要求**：不应出现"过去几十秒才出现第一个 Thinking 卡片"的情况。每个 protocol 事件到达后，对应的卡片应当**立即**渲染（< 250ms 延迟）。

**正确渲染状态（子态 B）：**

| 元素 | 正确值 | 来源 |
|---|---|---|
| header pill | `● RUNNING` | `isRunning` = true |
| footer pill | `● RUNNING` | `isBackendStillWorking` = true |
| 计时 | 持续递增 | `liveElapsedMs` computed from ticker |
| Thinking 卡片 | 实时出现，streaming 动画 | `parts` → `reasoning part, state='streaming'` → `ThinkingBlock streaming={true}` |
| 工具卡片 | 实时出现，显示工具名和参数概要 | `parts` → `tool-invocation part` → `ToolCallCard` |
| 回复文字 | 流式追加，逐 token 出现 | `liveStreamingText` → `StreamingText` 组件 |
| 卡片展开/折叠 | ✅ 可展开/折叠 | `collapsed` 由 ToolCallCard 自身 `$bindable` 管理 |
| Stop 按钮 | ✅ 显示红色方块 | |
| 底部 model-bar | ✅ 显示 | |

**关键 derived 值：**

```ts
storeIsProcessing = true
isLive = true
isBackendStillWorking = true
isRunning = true
hasFinished = false
showTyping = (无 liveStreamingText 时显示) true
```

**`parts` derived 的行为（子态 B）：**

```ts
// isLive = true → 走第一个分支
parts = storeStreamingParts.filter(p => p.type !== 'text' || p.state !== 'streaming')
// 过滤掉正在 stream 的 text part（由 StreamingText 处理），保留所有 reasoning 和 tool-invocation parts
```

---

### 2.3 子态 C：Agent 执行完成（done 事件到达后）

```
┌── Assistant Message ── (已完成, streaming=false) ────────────┐
│ AGENT              复制                                       │
│                                                                │
│ ┌─ ▼ Thinking (22 words) 380ms ─────────────────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│ ┌─ ▶ Run Code  # bash fib(40)  13ms Done ───────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│ ┌─ ▼ Thinking (9 words) 270ms ──────────────────────────────┐ │
│ └────────────────────────────────────────────────────────────┘ │
│                                                                │
│ fib(40) = 102334155                                            │
│                                                                │
│ 矩阵快速幂求 Fibonacci — O(log n)                               │
│ ...完整代码和讲解...                                             │
│                                                                │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ✓ DONE    🕐 15:47    Total 23.2s    Tools 25ms·1×        │ │
│ │          941 OUT   6.0K IN                                 │ │
│ └────────────────────────────────────────────────────────────┘ │
│                                                                │
│ ┌─ todo-progress (if todos > 0) ────────────────────────────┐ │
│ │  ◉ 代办事项 4/7 · 2 completed · 1 in progress · 1 pending  │ │
│ └────────────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────┘
```

**正确渲染状态（子态 C）：**

| 元素 | 正确值 | 来源 |
|---|---|---|
| header pill | 无 Running pill（不显示） | `isRunning` = false |
| footer pill | `✓ DONE` (绿色 #22c55e) | `isBackendStillWorking` = false |
| 时间 | 完成时间 | `completedAt` derived |
| 计时 | 固定值，不再递增 | `message.duration` |
| 卡片 | 全部显示，带完成时间 | `parts` → `message.parts` (from disk or finalized) |
| 卡片展开/折叠 | ✅ 可展开/折叠并保持 | `collapsed` 由 ToolCallCard 自身管理 |
| 文字 | 完整渲染 | `parts` 中的 text parts → `renderMarkdown` |
| Todo 卡片 | 如果 `todos.length > 0`，显示折叠卡片 | `hasFinished && storeTodos.length > 0` |
| Regenerate 按钮 | ✅ 显示（最后一条 assistant 消息右侧） | `!message.streaming && !storeIsProcessing` |
| Stop 按钮 | ❌ 不显示（改为 Send 按钮） | `$chat.isProcessing === false` |

**关键 derived 值：**

```ts
storeIsProcessing = false
isLive = false
isBackendStillWorking = false
isRunning = false
hasFinished = true
```

**`parts` derived 的行为（子态 C）：**

```ts
// isLive = false → 走第二个分支
parts = message.parts  // 在 finalizeAssistantMessage 时已设置
```

---

## 三、时间态二：多轮对话 / 长程任务执行

### 3.1 第二轮消息发送时

用户在第一轮完成后，发送第二个消息：

```
┌── User Message (第1轮) ──────────────────────────────────────┐
│ YOU              15:20                                       │
│ "帮我写一个斐波那契函数"                                        │
└───────────────────────────────────────────────────────────────┘

┌── Assistant Message (第1轮，已完成) ──────────────────────────┐
│ AGENT  ✓ DONE    15:43  Total 23m 3s  复制                   │
│ (第1轮的完整回复内容，包含卡片和文字)                             │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ✓ DONE    15:47  Total 23m 3s  941 OUT  6.0K IN          │ │
│ └────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────┘

┌── User Message (第2轮) ──────────────────────────────────────┐
│ YOU              15:48                                       │
│ "你没写完啊"                                                   │
└───────────────────────────────────────────────────────────────┘

┌── Assistant Message (第2轮，正在执行) ─────────────────────────┐
│ AGENT  ● RUNNING   15:48  复制                               │
│ (新的流式内容，不应受第1轮影响)                                   │
│ ┌─ bubble-footer ───────────────────────────────────────────┐ │
│ │ ● RUNNING   🕐 15:48   Total 1.2s   0 OUT  0 IN          │ │
│ └────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────┘
```

**⚠️ 关键约束（防止跨轮内容泄漏）：**

| 约束 | 说明 |
|---|---|
| 第1轮 Assistant Message **绝不**渲染第2轮的卡片 | `isLive` 必须为 `false`（第1轮不是最后一条 assistant 消息） |
| 第1轮 Assistant Message **绝不**显示 `● RUNNING` | 同上 |
| 第1轮 Assistant Message 的 `parts` **绝不**从 `storeStreamingParts` 取 | 必须从 `message.parts`（已 finalize 的数据）取 |
| 第2轮 Assistant Message 的 `parts` **只**从 `storeStreamingParts` 取 | `isLive = true`，走 streaming 分支 |
| 第1轮的计时器**不**跳动 | `isBackendStillWorking = false` → 显示 `message.duration` |
| 第2轮的计时器**必须**跳动 | `isBackendStillWorking = true` → 显示 `liveElapsedMs` |

---

### 3.2 `isLive` 必须只对最后一条 Assistant 消息为 true

```ts
// ❌ 错误（当前代码 — 2026-06-21）
let isLive = $derived(
  storeIsProcessing && message.role === "assistant"
);
// 问题：当 isProcessing = true 时，所有历史 assistant 消息的 isLive 都是 true
// 后果：第1轮读 storeStreamingParts → 拿到第2轮的 parts → 内容泄漏

// ✅ 正确（必须额外检查"是否为最后一条"）
let isLive = $derived(
  storeIsProcessing
  && message.role === "assistant"
  && storeMessages.length > 0
  && storeMessages[storeMessages.length - 1].id === message.id
);
// 此时只有最后一条 assistant 消息的 isLive 为 true
```

---

## 四、时间态三：切换会话

### 4.1 切换流程

```
用户点击侧边栏另一个会话
    ↓
loadSession(newSessionId)
  → 从磁盘 / API 加载 messages
  → set({ messages, isProcessing: false, streamingParts: [], ... })
    ↓
App.svelte: {#each $chat.messages as msg (msg.id)}
  → 所有 msg.id 都变了（格式: {sessionId}-msg-{idx}）
  → 全部 ChatMessage 组件被销毁再重建
    ↓
新 ChatMessage 组件 mount
  → $effect() → chat.subscribe() → callback fires → storeIsProcessing = false
  → isLive = false
  → parts = message.parts (从磁盘加载)
```

### 4.2 正确渲染状态

| 元素 | 正确值 | 来源 |
|---|---|---|
| 所有 assistant 消息 | `isLive = false`, header 无 Running pill | `storeIsProcessing = false` |
| 所有 assistant 消息 footer | `✓ DONE` | `isBackendStillWorking = false` |
| 卡片 | 全部渲染（从 `message.parts` 加载），已折叠 | `parts` derived → `message.parts` |
| 卡片展开/折叠 | ✅ 点击可展开，展开后**不会自动缩回** | `collapsed` 由 ToolCallCard 自身 `$state` 管理 |
| 计时 | 固定值 `message.duration` | 不再跳动 |
| 文字 | 完整渲染 | `parts` → text parts → `renderMarkdown` |
| 模型栏 | 显示（从 `modelInfo` event 或 message.modelInfo） | |

### 4.3 卡片展开/折叠的规范

```
✅ 正确行为：
  - 点击 chevron 后，卡片展开显示 Arguments 和 Result
  - 展开状态下，即使 ticker State.now 每 250ms 更新，也不会缩回
  - 再次点击 chevron 可以缩回
  - 折叠状态在切换会话或刷新页面后重置为 collapsed

❌ 错误行为（Bug 2026-06-21）：
  - 切换会话后点击 chevron 无反应
  - 点击 chevron 展开后立即缩回（被 ticker 重新渲染重置）
  - 从当前会话切换到另一个会话后，卡片无法展开
```

**原因分析**：`ChatMessage.svelte` 中 `collapsed={true}` 是字面量。当 ChatMessage 组件被销毁再重建（切换会话），新组件的 `collapsed` 初始为 `true`。如果 livelapsedMs 的 ticker 更新触发了 ChatMessage 的全模板重新渲染，字面量 `{true}` 会被重新传递给 ToolCallCard 的 `$bindable`，重置用户手动展开的状态。

**修复方向**：要么不让 `livelapsedMs` 的推导依赖触发卡片区域的重新渲染（将 ticker 放到独立的 `$effect` 里），要么将 collapsed 状态提升到 ChatMessage 层用 `$state` map 管理。

---

## 五、时间态四：重新打开 Tauri 桌面应用

### 5.1 加载流程

```
App.svelte onMount
    ↓
connectSSE()  → Tauri listen("sse_event") 或 HTTP EventSource
    ↓
如果之前有选中的 session → loadSession(selectedSessionId)
    ↓
messages 从磁盘加载，每条 message 包括：
  - content (text)
  - parts: UIMessagePart[]  （从 streamEvents 转换或直接从磁盘读）
  - duration (ms)
  - tokensIn / tokensOut
  - exitReason
  - modelInfo
  - streaming = false（从磁盘的消息永远是 false）
    ↓
storeIsProcessing = false（初始值，从 chat store 的 writable 初始值）
    ↓
所有 ChatMessage 组件渲染
```

### 5.2 正确渲染状态

| 元素 | 正确值 |
|---|---|
| 所有消息 | `isLive = false`, `isBackendStillWorking = false` |
| 所有 assistant footer | `✓ DONE` + 固定 duration |
| 卡片 | 已折叠，点击可展开 |
| 计时 | 固定不递增 |
| 文字 | 完整渲染 |
| 模型栏 | 如果 message.modelInfo 存在则显示，否则显示最后一次 model_info event 的值 |
| 最后一条 assistant 的 Regenerate 按钮 | ✅ 显示 |

### 5.3 ⚠️ 关键陷阱

| 陷阱 | 说明 |
|---|---|
| `message.streaming` 从磁盘加载一定是 `false` | 不能用 `message.streaming` 判断是否在流式 |
| `message.duration` 从磁盘可能为 `0` 或 `undefined` | `loadSession` 会检查 `(m as any).duration`，但某些旧会话可能没有这个字段 |
| `message.parts` 可能为 `undefined`（旧会话无 parts） | `loadSession` 会回退到 `m.streamEvents → convertStreamEventsToParts()` |
| `isProcessing` 初始为 `false` | 直到用户发送新的消息，`startAssistantMessage` 才设为 `true` |

---

## 六、底部模型信息栏（model-bar）

### 6.1 正常数据显示

```
┌── model-bar ──────────────────────────────────────────────────┐
│ │omlx/Qwen3.6-35B│ │Online│  │8msgs│ │1.2K out│ │6.0K in│    │
│ │Context: 6.0K/200K ████░░░░ 3%│                                │
└────────────────────────────────────────────────────────────────┘
```

### 6.2 各字段含义和来源

| 字段 | 来源 | 显示条件 |
|---|---|---|
| 模型名 | `$chat.modelInfo.model` | `modelInfo` 不为 null |
| Online/Local | `$chat.modelInfo.isLocal ? "Local" : "Online"` | `modelInfo` 不为 null |
| msgs 数量 | `$chat.messages.length` | `messages.length > 0` |
| out tokens | `aggregateTokens(messages).out` | `in > 0 或 out > 0` |
| in tokens | `aggregateTokens(messages).in` | `in > 0 或 out > 0` |
| Context 百分比 | `aggregateTokens(messages).in / modelInfo.contextWindow * 100` | 始终显示 |
| Context 颜色 | <70% 绿色, 70-90% 黄色, >90% 红色 | 始终显示 |

### 6.3 空状态

```
┌── model-bar ──────────────────────────────────────────────────┐
│ │omlx/Qwen3.6-35B│ │Online│    (无 msgs/token/context 行)     │
└────────────────────────────────────────────────────────────────┘
```

---

## 七、卡片渲染规范

### 7.1 ThinkingBlock

| 属性 | 流式期间 | 完成后 |
|---|---|---|
| `thinking` | 实时追加的思考内容 | 完整的思考内容 |
| `streaming` | `true` → header 有脉冲动画 | `false` → header 显示固定 duration |
| `durationMs` | `undefined` (流式) 或实时累加 | 实际耗时 |
| 可展开/折叠 | ✅ | ✅ |
| 内容 | 实时更新 | 静态 |

### 7.2 ToolCallCard

| 属性 | 流式期间 | 完成后 |
|---|---|---|
| `toolCall.name` | 工具名 | 不变 |
| `toolCall.arguments` | 流式追加 (input-streaming → input-available) | 完整的 JSON |
| `completed` | `false` (显示 "Running...") | `true` (显示 "Done") |
| `collapsed` | 默认 `true`，可展开 | 默认 `true`，可展开 |
| `durationMs` | `undefined` 或实时的 | 实际耗时 |

### 7.3 EditCard

| 属性 | 正确行为 |
|---|---|
| `filePath` | 显示文件路径 |
| `oldString` | red 删除行 |
| `newString` | green 新增行 |
| `collapsed` | 默认 `true`，展开后显示完整 diff |
| 行内词级 diff | ✅ 已实现（LCS diff in EditCard.svelte） |

---

## 八、已知 Bug 及修复注意事项

### 8.1 `isLive` 使所有 Assistant 消息都变 live

**现象**：发送第二轮消息后，第一轮的气泡里出现第二轮的卡片，第一轮也显示 `● RUNNING`。

**根因**：`isLive = storeIsProcessing && message.role === "assistant"` 没有"是否为最后一条"检查。

**修复后代码**：
```ts
let isLive = $derived(
  storeIsProcessing
  && message.role === "assistant"
  && storeMessages.length > 0
  && storeMessages[storeMessages.length - 1].id === message.id
);
```

### 8.2 `hasFinished` 的 OR 关系淹没 Running 状态

**现象**：状态栏一直显示 `✓ DONE`，但计时在跳动，Stop 按钮还在。

**根因**：`hasFinished` 的第一个条件 `message.streaming !== true` 使任何 `streaming: false` 的消息都判定为 "finished"。

**修复后代码**：
```ts
let hasFinished = $derived(
  (message.duration != null && message.duration > 0) ||
  (message.exitReason != null && message.exitReason.length > 0)
);
// 移除 message.streaming !== true 条件
```

### 8.3 `isBackendStillWorking` 不应依赖 `message.streaming`

**现象**：状态栏显示 `✓ DONE`，但 Agent 确实在运行中。

**根因**：`isBackendStillWorking = isLive && storeIsProcessing`。`isLive` 如果加了 `message.streaming` 条件（从磁盘加载是 false），就会导致 `isBackendStillWorking = false`。

**正确做法**：`isBackendStillWorking` 必须只用 `storeIsProcessing` 和最近一条 assistant 消息来判断。不要依赖 `message.streaming`、`message.duration`、`message.exitReason`。

### 8.4 卡片无法展开

**现象**：切换会话后点击卡片 chevron 无反应，或在当前会话展开后几秒又缩回。

**根因**：`ChatMessage.svelte` 中 `collapsed={true}` 是字面量。当 ChatMessage 全模板重新渲染时，这个字面量会重置 ToolCallCard 的 `$bindable` 状态。

**修复方向**：在 ChatMessage 层维护 `collapsedCards` 的 `$state` map，传递给 ToolCallCard 时用 binding：`collapsed={collapsedCards[p.toolCallId] ?? true}`。

---

## 九、修改代码前必须验证的检查清单

每次修改 `ChatMessage.svelte`、`chat.ts`、`App.svelte`、`protocol-processor.ts` 中任何与渲染相关的代码后，必须验证以下场景：

### 实时交互测试（Tauri 桌面端）

- [ ] 发送一条简单消息（如 "1+1"?），确认 header/footer 显示 `● RUNNING`，计时跳动
- [ ] 发送复杂任务（包含思考+工具调用），确认 Thinking 卡片和 ToolCallCard **实时出现**（< 1s 延迟），不是等任务完成后才出现
- [ ] 确认 `liveStreamingText` 逐 token 追加，没有双重显示
- [ ] 确认计时在任务执行期间持续递增
- [ ] 确认任务完成后 footer pill 变为 `✓ DONE`，计时停止，显示最终 duration
- [ ] 确认任务完成后 Notify 按钮显示正確

### 多轮对话测试

- [ ] 发送第一条消息，等完成，发送第二条消息
- [ ] 确认第一条消息的卡片、文字不变（不应出现第二条的卡片或文字）
- [ ] 确认第一条消息 footer 为 `✓ DONE`，第二条消息 footer 为 `● RUNNING`
- [ ] 确认第一条的计时不跳动，第二条的计时跳动

### 切换会话测试

- [ ] 切换到一个有历史记录的会话
- [ ] 确认所有消息正常渲染（卡片、文字、footer）
- [ ] 确认所有消息 footer 为 `✓ DONE`
- [ ] 点击卡片 chevron，确认卡片展开**且不自动缩回**
- [ ] 再次点击 chevron，确认卡片可以缩回
- [ ] 确认底部 model-bar 正常显示

### 重新打开

- [ ] 关闭 Tauri 再重新打开
- [ ] 确认上次选中的会话自动恢复
- [ ] 确认所有消息正常渲染
- [ ] 确认卡片可展开

---

## 十、代码审查速查表（review checklist）

审查 PR 时，检查以下变更是否会影响渲染正确性：

| 变更位置 | 检查项 |
|---|---|
| `ChatMessage.svelte:isLive` | ❌ 不可移除 `storeMessages[last].id === message.id` 检查 |
| `ChatMessage.svelte:hasFinished` | ❌ 不可加回 `message.streaming !== true` 条件 |
| `ChatMessage.svelte:isBackendStillWorking` | ❌ 不可加 `message.duration` 或 `message.exitReason` 作为条件 |
| `ChatMessage.svelte:collapsed={true}` | ❌ 不要用字面量，改为 state map + bind |
| `chat.ts:streamingParts` | ❌ 不能改名为 per-message 字段（后端协议不支持），确保 `startAssistantMessage` 时清空 |
| `chat.ts:finalizeAssistantMessage` | ❌ 必须设置 `message.parts = finalParts` 且 `streamingParts = []` |
| `App.svelte:{#each}` | ❌ key 必须是 `msg.id`，不可改为 index |
| `protocol-processor.ts` | ❌ 确保 `tool_input_start` 对 `respond/no_tool` 跳过（防止双重渲染） |
