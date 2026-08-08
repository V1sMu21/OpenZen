---
name: tauri-e2e
description: Launches the real OpenZen Tauri desktop app and drives it via macOS CGEvent for end-to-end testing — sending messages, clicking UI elements, and verifying responses. Covers app launch, message input, and result verification. Use for any Tauri desktop testing task.
---

# Tauri 桌面端 E2E 测试

## 前置条件

所有检查的脚本在 `scripts/e2e/` 下：

| 条件 | 检查命令 | 若不满足 |
|---|---|---|
| Tauri 已构建 | `ls target/debug/openzen-tauri` | `cargo build` |
| 后端运行 | `pgrep -f "openzen serve"` | `openzen serve --port 8421 --frontend-dir frontends/dist` |
| Vite 开发服务器 | `pgrep -f vite` | `cd frontends && npm run dev` |
| Python e2e venv | `ls /tmp/e2e_venv/bin/python` | `python3.12 -m venv /tmp/e2e_venv && /tmp/e2e_venv/bin/pip install pyobjc-framework-Quartz pillow` |
| cgtype.py | `ls /tmp/cgtype.py` | 已存在，无需额外安装 |
| cgclick.py | `ls /tmp/cgclick.py` | 已存在，无需额外安装 |
| cliclick | `which cliclick` | `brew install cliclick` |
| 输入法切到英文 | `scripts/e2e/switch_input_source.sh ABC` | 中文输入法会损坏 unicode 注入 |

## 启动 Tauri 桌面应用

```bash
# 在 openzen 项目根目录
./target/debug/openzen-tauri &
```

启动后 Tauri 窗口默认位置 `(1200, 80)`，大小 `700×850`（在 1920×1080 屏幕上位于右半部分）。

## 屏幕坐标参考（1920×1080，Tauri 右半部分）

| 元素 | 坐标 | 点击方法 | 备注 |
|---|---|---|---|
| Tauri 标题栏（聚焦用） | `(1500, 88)` | `cgclick.py 1500 88 60` | 确保窗口获得焦点 |
| Sidebar "+ New Chat" | `(1370, 204)` | `cliclick c:1370,204` | 标准 cliclick 即可 |
| Chat textarea（点击聚焦） | `(1620, 880)` | `cgclick.py 1620 880 80` | y=880 是 textarea 内部第一像素；y=870 在边框上会丢失点击 |
| **Chat Send 按钮** | **`(1872, 887)`** | `cgclick.py 1872 887 100` | 蓝色正方形，~55×55px；偏移 10px 以上会静默丢失 |
| AskUserDialog "Your response" | `(1700, 540)` | `cgclick.py 1700 540 80` | |
| AskUserDialog "Cancel" | `(1790, 620)` | — | |
| AskUserDialog "Send response" | **`(1830, 620)`** | `cgclick.py 1830 620 100` | |
| Tauri "Stop" 红色方块 | `(1835, 910)` | — | 停止正在运行的 agent |

> ⚠️ 坐标是实验验证得出的，**不要凭肉眼估计**。Send 按钮偏差 10px 就会静默吞掉点击。

## 发送消息到 Tauri

### 方法 1：命令行直接操作（推荐用于自动化测试）

```bash
# 1. 确保 Tauri 窗口在前台 + 输入法为英文
osascript -e 'tell application "System Events" to set frontmost of (first process whose name is "openzen-tauri") to true'
sleep 0.3
scripts/e2e/switch_input_source.sh ABC

# 2. 聚焦窗口
/tmp/e2e_venv/bin/python /tmp/cgclick.py 1500 88 60

# 3. 点击 "+ New Chat"（新建会话）
cliclick c:1370,204
sleep 1

# 4. 点击 textarea 并输入消息
/tmp/e2e_venv/bin/python /tmp/cgclick.py 1620 880 80
sleep 0.3
/tmp/e2e_venv/bin/python /tmp/cgtype.py "你好，请帮我写一段Python代码"

# 5. 点击 Send
/tmp/e2e_venv/bin/python /tmp/cgclick.py 1872 887 100
```

### 方法 2：使用已有 E2E 脚本

```bash
scripts/e2e/tauri_ask_user_e2e.sh
```

该脚本演示完整的 `ask_user` 工具流程，包括发送消息、等待对话框、回复、等待最终结果。

## 关键工具说明

### cgtype.py — 在 Tauri WebView 中输入文字

```bash
/tmp/e2e_venv/bin/python /tmp/cgtype.py "要输入的文字"
```

- 通过 Quartz `CGEventKeyboardSetUnicodeString` 注入原始 unicode 字符
- **绕过 IME**，即使 Pinyin 输入法激活也能正确输入
- **触发真实 `input` DOM 事件**，Svelte 5 的 `bind:value` 能正确捕获
- `cliclick t:` **不可用** — 它去掉多词输入的空格，且不给 WKWebView 产生 `input` 事件
- `pbcopy` + `keystroke v` **不可用** — Svelte 5 `bind:value` 不响应粘贴事件

### cgclick.py — 在 Tauri WebView 中点击

```bash
/tmp/e2e_venv/bin/python /tmp/cgclick.py <x> <y> [hold_ms]
```

- 通过 Quartz `CGEventCreateMouseEvent` 发送 mousedown + 延迟 + mouseup
- **默认 hold=100ms**，这是 WKWebView 接受小按钮点击的关键
- `cliclick c:` 在 Send 按钮这类小目标上太快，会被 WKWebView 静默吞掉

### switch_input_source.sh — 切换输入法

```bash
scripts/e2e/switch_input_source.sh ABC        # 切到英文
scripts/e2e/switch_input_source.sh "搜狗拼音"   # 切到中文
```

- 通过 `Cmd+Ctrl+Space`（选择下一个输入源）循环切换

## 辅助工具方法（可在脚本中内联使用）

### cgtype.py 内联（无文件依赖）

```python
import ctypes, ctypes.util, sys, time
_cg = ctypes.cdll.LoadLibrary(ctypes.util.find_library("ApplicationServices") or
    "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")
_cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
_cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
_cg.CGEventKeyboardSetUnicodeString.restype = None
_cg.CGEventKeyboardSetUnicodeString.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_wchar_p]
_cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
_kHID = 0x0010000000000000
for ch in sys.argv[1]:
    d = _cg.CGEventCreateKeyboardEvent(None, 0, True)
    u = _cg.CGEventCreateKeyboardEvent(None, 0, False)
    _cg.CGEventKeyboardSetUnicodeString(d, 1, ch)
    _cg.CGEventKeyboardSetUnicodeString(u, 1, ch)
    _cg.CGEventPost(_kHID, d); time.sleep(0.005)
    _cg.CGEventPost(_kHID, u); time.sleep(0.005)
```

### cgclick.py 内联

```python
# 需 pyobjc-framework-Quartz 已安装
import Quartz, sys, time
pt = Quartz.CGPointMake(int(sys.argv[1]), int(sys.argv[2]))
h = int(sys.argv[3])/1000 if len(sys.argv)>3 else 0.1
Quartz.CGEventPost(Quartz.kCGHIDEventTap, Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventLeftMouseDown, pt, Quartz.kCGMouseButtonLeft))
time.sleep(h)
Quartz.CGEventPost(Quartz.kCGHIDEventTap, Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventLeftMouseUp, pt, Quartz.kCGMouseButtonLeft))
```

## 日志位置

| 日志 | 路径 | 内容 |
|---|---|---|
| Tauri IPC 日志 | `~/.openzen/logs/openzen-tauri.log` | 仅 `send_message` 条目 |
| Vite 开发日志 | `/tmp/vite-dev.log` | 前端构建日志 |
| Backend `openzen serve` 日志 | `/tmp/openzen-server.log` | 仅在 MCP/SAVE 事件时写入 |
| E2E 截图（运行脚本时） | `/tmp/tauri-screenshots/` | 每步截图 |

## 验证方法

### 截图
```bash
screencapture -x -t png /tmp/tauri-test-$(date +%s).png
```

### 检查 Tauri 进程
```bash
pgrep -fl openzen-tauri   # 应返回 PID
```

### 检查后端连通性
```bash
curl -s http://localhost:8421/api/health
```

### 检查 Vite 前端
```bash
curl -s http://localhost:5173 | head -5
```

## 常见问题

| 问题 | 原因 | 解决 |
|---|---|---|
| 点击 Send 没反应 | WKWebView 需要 hold 手势 | 用 `cgclick.py` 而不是 `cliclick` |
| 输入中文变乱码 | IME 拦截了 unicode | 先运行 `switch_input_source.sh ABC` |
| `cliclick t:` 丢空格 | cliclick 已知 bug | 用 `cgtype.py` |
| Send 按钮坐标偏移 | Tauri 窗口位置变动 | 用 `screencapture` + Pillow `.crop` + 4× 放大确认坐标 |
| 窗口不在前台 | macOS 焦点保护 | 先 `osascript` set frontmost，再 click 标题栏 |

## ⚠️ 已知陷阱：Svelte 5 `$effect` 不会追踪 legacy `writable` store

**这是 2026-06-21 在修复 Tauri 对话界面自动滚动 bug 时踩到的最隐蔽的坑。**

### 症状

`$effect(() => { ... $chat.messages ... })` 这种写法看上去能读 `chat` store 的 `messages` 数组，**但实际只会在组件 mount 时跑一次**。后续 `chat` store 更新（新增消息、流式追加 token）**永远不会触发这个 `$effect` 重跑**。结果就是自动滚动代码只执行一次，聊天窗口在用户发送消息后永远停在原位不滚到底。

### 根本原因

Svelte 5 的 `$effect` 只追踪 **`$state` rune**（即 `let x = $state(...)` 创建的代理），不追踪 **Svelte 4 时代的 `writable` store**。

- `$chat.messages` 这种 `$store` 自动订阅语法在 `.svelte` 模板中**有效**（模板编译时会注入 `subscribe` + 反订阅）
- 但在 `$effect` 中，**编译产物是直接读 `chat.messages`，没有任何订阅逻辑**——它读到的只是 mount 那一刻的快照

### 试图过的错误方案（都没用）

1. ❌ `$effect(() => { ... $chat.messages ... })` — 只跑一次
2. ❌ `$effect` + `chat.subscribe(cb)` — 回调确实每次都触发，但 `$effect` 自身不会重跑，DOM 还没更新，scrollHeight 是旧的
3. ❌ `onMount` + `chat.subscribe(cb)` — 同上问题；store 触发先于 DOM 更新

### ✅ 唯一可靠的方案：`MutationObserver` 监听 `.messages-list` DOM 变更

```ts
onMount(() => {
  const scrollToBottom = () => {
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const scroller = document.querySelector<HTMLElement>('.chat-container');
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    }));
  };

  const setupObserver = () => {
    const messagesList = document.querySelector<HTMLElement>('.messages-list');
    if (!messagesList) { requestAnimationFrame(setupObserver); return; }
    const observer = new MutationObserver(scrollToBottom);
    observer.observe(messagesList, {
      childList: true,    // 新 <ChatMessage> 节点
      subtree: true,      // ChatMessage 内部子节点（流式文本）
      characterData: true, // 文本内容变化
    });
    scrollToBottom(); // 初始也滚到底（加载历史会话时）
  };
  setupObserver();
});
```

**为什么这个方案靠谱：**
- `MutationObserver` 在 Svelte 完成 DOM 更新**之后**才触发 → `scrollHeight` 是最新的
- 不依赖 Svelte 5 store 兼容性 → 与 legacy `writable` 完美共存
- 覆盖所有内容变更：新消息、streaming token、thinking→answer 切换
- `scrollToBottom` 幂等：同一帧内多次 mutation 只会触发一次有效滚动

### 验证

E2E 截图 `/tmp/tauri-shots/170_reload.png` 展示修复后效果：发送 "test scroll 1" 后，聊天窗口自动滚到最底部，agent 响应完整可见。截图 `/tmp/tauri-shots/211_session.png` 展示切换会话时也会自动滚到该会话最后一条消息。

### 推广到其他场景

任何「当 store 更新 → 副作用触发 → DOM 滚动/聚焦/动画」的场景，都应该用 `MutationObserver` 而不是 `$effect`。这条经验适用于：
- 自动滚动到新消息
- 自动展开折叠面板
- 自动高亮新插入的 token
- 自动聚焦到新出现的输入框

**经验法则：Svelte 5 + legacy store = 用 `MutationObserver` 做 DOM 副作用，千万别赌 `$effect` 会被触发。**

## ⚠️ 已知陷阱：`isLive` / `isBackendStillWorking` 不能依赖 `message.duration` / `message.exitReason`

**这是 2026-06-21 在修复 "Running 状态消失 + 实时卡片不渲染" bug 时踩到的坑。**

### 症状

用户报告："状态栏一直显示 Done，但计时正确，过去几十秒都没有第一个思考卡片被渲染出来。" — `isBackendStillWorking` 一直是 `false`（显示 Done），`isLive` 一直是 `false`（`parts` 走 `message.parts` 分支，但 finalize 之前没值），计时器却在跳动（`liveElapsedMs` 正常工作）。

### 根本原因

初版 `isLive` / `isBackendStillWorking` 写成：

```ts
let isBackendStillWorking = $derived(
  storeIsProcessing
  && message.role === "assistant"
  && storeMessages[storeMessages.length - 1]?.id === message.id
  && (message.duration == null || message.duration === 0)
  && (message.exitReason == null || message.exitReason.length === 0)
);
```

看似严密，但**三个条件都可能在 streaming 期间为 `false`**：

1. **`message.duration == null`** — Tauri 后端在 finalize 之前就把消息存到 session store（`streaming: false, duration: 0`）。当用户 `Cmd+R` 刷新页面、session 被重新加载时，**新的**用户消息对应的 assistant 消息从磁盘读出来，`duration` 可能是 `0` 或 `undefined`，但关键是**该消息是上一轮的旧消息**，不是当前正在 streaming 的消息。
2. **`message.exitReason == null`** — 同上，旧消息可能带着 `exitReason`。
3. **`message.streaming || storeStreamingParts.length > 0`** — `message.streaming` 可能是 `false`（从磁盘加载），`storeStreamingParts` 可能是空（Svelte 5 `$effect` 不追踪 legacy `writable` store，导致订阅延迟一拍）。

只要三个条件中有一个为 `false`，`isBackendStillWorking` 就是 `false`，状态栏显示 Done，`isLive` 是 `false`，`parts` 走 fallback 分支（`message.parts` / `message.streamEvents`），两者都为空 → 实时卡片不渲染。

### ✅ 唯一可靠的方案：`storeIsProcessing` 是唯一权威信号

```ts
let isLive = $derived(
  storeIsProcessing && message.role === "assistant"
);

let isBackendStillWorking = $derived(isLive);

let isRunning = $derived(isLive);

let hasFinished = $derived(
  message.role === "assistant" ? !storeIsProcessing : false
);
```

**为什么这个方案靠谱：**
- `storeIsProcessing` 由 `startAssistantMessage()` 设置为 `true`，由 `finalizeAssistantMessage()` / `done` handler 设置为 `false`，**完全在聊天 store 内部管理**，不受 Tauri 后端或磁盘加载影响
- 计时器（`liveElapsedMs`）独立于 `isLive` — 依赖 `tickerState.now` + `message.timestamp`，所以即使 `isLive` 错也不会让计时器一起错
- `isBackendStillWorking` 就是 `isLive` — 单一来源，header pill 和 footer pill 永远一致

**验证**

E2E 截图 `/tmp/tauri-shots/230_t1.png` ~ `230_t6.png` 展示修复后效果：
- 发送 "Calculate fib(35) using bash..." 后，header pill 显示 `● RUNNING`（琥珀色），footer pill 也显示 `● RUNNING`
- TOTAL 从 225ms → 2.0s → 5.5s → 8.5s 一路滚动
- Thinking 卡片（55 words）实时出现，工具卡片 `Run Code # Bash iterative Fibonacc... 0ms Done` 实时出现，第二个 Thinking 卡片（9 words）也实时出现
- 文本 `fib(35) = 9,227,465 ✓` 和 `矩阵快速幂求 Fibonacci — O(log n)` 在 streaming 期间就开始显示
- 完成后 footer pill 切回 `✓ DONE 16:37 TOTAL 23.2s`，计时器停止

**推广到其他场景**

任何"is the backend still working on this turn?"的判断，都应该用 `storeIsProcessing` + `message.role` 作为唯一信号。**不要**叠加 `message.duration` / `message.exitReason` / `message.streaming` 等可以从磁盘加载或后端写入的字段 — 这些都是 stale-prone 字段，会让 `isLive` 误判。

**经验法则：判断"是否在处理中"时，只信全局 `isProcessing` 标志。`message.*` 字段一律不可信。**

---

## Qwen 3.5 / 3.6 模型输出标签全集

**这是 2026-06-21 在用户报告 "模型输出的 `<summary>`, `<code>` 等标签未被正确识别与分配" 时调研的结果。**

### 1. 思考 / Reasoning 标签

| 标签 | 说明 | 出现位置 |
|---|---|---|
| `<think>...</think>` | 思考内容。Qwen 3.5+ 的 prompt 已经包含 `<think>`，所以**只有 `</think>` 出现在输出中** | reasoning channel（→ ThinkingBlock） |
| `</think>` | `<think>` 的结束标签 | reasoning channel |

⚠️ **关键陷阱**（来源：[vllm/qwen3_reasoning_parser.py](https://github.com/vllm-project/vllm/blob/3e49479c/vllm/reasoning/qwen3_reasoning_parser.py)）："Qwen3.5 models may emit `<tool_call>` inside the thinking block without closing `</think>` first. `<tool_call>` is treated as an implicit end of reasoning." — 即小模型（Qwen3.5-35B-A3B、12B-A10B）经常在 `</think>` 之前就开始 `<tool_call>`，**后台 parser 必须把 `<tool_call>` 当作隐式 reasoning 结束**。

### 2. 工具调用标签

Qwen 系列根据模型变体使用**两种不同格式**：

#### 2a. Hermes-style（Qwen3-Instruct）

```
<tool_call>
{"name": "function_name", "arguments": {...}}
</tool_call>
```

→ 走 `tool-invocation` part，渲染为 `ToolCallCard` / `EditCard` / `SkillMcpCard`

#### 2b. XML-style（Qwen3-Coder）

```
<tool_call>
  <function=function_name>
    <parameter=param1>
      value_1
    </parameter>
    <parameter=param2>
      value_2
    </parameter>
  </function>
</tool_call>
```

→ 走 `tool-invocation` part（args 是 XML 格式字符串），渲染为 `ToolCallCard` / `EditCard` / `SkillMcpCard`

⚠️ **关键陷阱**（来源：[QwenLM/Qwen3.6 issue #125](https://github.com/QwenLM/Qwen3.6/issues/125)、[ggml-org/llama.cpp issue #20837](https://github.com/ggml-org/llama.cpp/issues/20837)）：XML 风格工具调用**经常在 thinking block 内部出现**（即在 `</think>` 之前就 emit `<tool_call>`），导致：
- API 层 `tool_calls=[]` 为空
- XML 出现在 `reasoning_content` 里
- `finish_reason="stop"`

大模型（Qwen3.5-397B-A17B / Plus）已部分修复；小模型仍频繁触发。**后台 parser 必须：(1) 把 thinking block 内的 `<tool_call>` 当作隐式 reasoning 结束，(2) 在 reasoning 之外也能解析 XML-style 工具调用**。

#### 2c. Stray fragments

如果模型在未闭合的 tag 上被打断，streaming 文本里可能残留：
- `<function=...>` 或 `</function>` （无匹配）
- `<parameter=...>` 或 `</parameter>` （无匹配）

→ 必须从 `message.parts[].args` 和最终渲染的 markdown 里**strip**掉，否则用户会看到裸 XML。

### 3. 工具响应标签

| 标签 | 说明 | 出现位置 |
|---|---|---|
| `<tool_response>...</tool_response>` | 工具执行结果 | 不渲染（由后端包装成 `tool_output_available` 事件 → `ToolCallCard.result`） |

### 4. 内部 scratch-space 标签（不应渲染给用户）

| 标签 | 说明 | 处理方式 |
|---|---|---|
| `<summary>...</summary>` | 模型的 working-memory summary | **必须 strip**。内容已通过 `strip_summary_tags` 在后端提取到 `full_response` 之外的字段（`crates/ga-core/src/agent_loop.rs:142`）。如果 streaming 时漏到 text 通道，前端 `renderMarkdown` 必须 drop 整个 `<summary>...</summary>` 块（`frontends/src/lib/utils/markdown.ts:11`） |
| `<code>...</code>` | 内联代码 | 由 markdown ``` 围栏处理（`markdown.ts:25-30`），不需要特殊处理 |
| `<respond>...</respond>` | 最终回答的 marker | strip（`markdown.ts:12`） |
| `<antThinking>...</antThinking>` | 替代思考格式 | strip（`markdown.ts:13-14`） |
| `<tool_code>...</tool_code>` | 工具代码块 | strip（`markdown.ts:17-18`） |

⚠️ **关键陷阱**（发现于 2026-06-21）：streaming 文本可能**跨多次 `text_delta` 事件拆分**，导致 `</summary>` 单独出现而 `<summary>` 在另一个 part。**前端 `renderMarkdown` 必须同时 strip 散落的 `</summary>` / `</respond>` / `</antThinking>` / `</thinking>` / `</tool_code>` 闭合标签**（不带匹配 opening tag 的情况），否则用户会在历史会话里看到裸的 `</summary>` 文本。

### 5. ChatML 控制 token（不在输出里，只在 prompt 里）

| Token | 说明 |
|---|---|
| `<\|im_start\|>` / `<\|im_end\|>` | turn 边界 |
| `<\|im_start\|>system/user/assistant/tool` | role markers |
| `<tools>...</tools>` | 工具定义（仅在 system prompt） |

→ 这些 token 在用户可见的输出里**不会出现**，因为后端 tokenizer 会处理。**如果出现，说明模型 tokenizer 配置错误**。

### 6. 后端处理职责分工

| 层 | 职责 | 失败模式 |
|---|---|---|
| **Tauri backend（Rust）** | `crates/ga-core/src/agent_loop.rs` 用 `strip_summary_tags` 提取 `<summary>` 内容到 `full_response` 之外；用 tool parser 解析 `<tool_call>` JSON 或 XML → `tool_calls` 事件 | parser 漏掉 XML-style → 工具不执行 |
| **SSE event channel** | 把 `StreamEvent::ReasoningDelta` / `TextDelta` / `ToolCallReady` / `ToolCallResult` 转发到前端 | 事件丢失 → 前端看不到 |
| **前端 markdown.ts** | strip 残留的 `<summary>` / `</summary>` / `<respond>` / `<antThinking>` / `<thinking>` / `<tool_code>` / `<function>` / `<parameter>` | 漏 strip → 用户看到裸 XML |

### 7. 验证

- ✅ 后端 `strip_summary_tags` 在 `agent_loop.rs:142` 提取 `<summary>` 内容
- ✅ 前端 `renderMarkdown` 在 `markdown.ts` strip 所有已知 Qwen 内部标签（含 stray 闭合标签）
- ✅ `protocol-processor.ts` 处理 `reasoning_start/delta/end` 和 `text_start/delta/end` 事件
- ⚠️ **未验证**：Qwen3-Coder 的 XML-style `<tool_call>` 在 thinking block 内部的解析 — 需要检查 `tool_parser` 配置是否用 `qwen_xml` 而非 `qwen3_coder`（见 [allanchan339/vLLM-Qwen3-3.5-3.6-chat-template-fix](https://github.com/allanchan339/vLLM-Qwen3-3.5-3.6-chat-template-fix)）

**经验法则：Qwen 3.5+ 的输出 tag 集合 = `{<think>, </think>, <tool_call>, </tool_call>, <function=...>, </function>, <parameter=...>, </parameter>, <tool_response>, </tool_response>, <summary>, </summary>, <code>, </code>, <respond>, </respond>, <antThinking>, </antThinking>, <thinking>, </thinking>, <tool_code>, </tool_code>}`。任何不在这集合里的 XML 标签出现在用户面前 = parser 漏处理。**

