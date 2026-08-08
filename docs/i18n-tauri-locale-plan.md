# 中英双语切换方案 — Tauri 统一管理（方案 B）

> 状态：✅ Phase 1-5 全部完成 | 日期：2026-06-28

## 一、目标

- 用户在 Tauri 桌面应用中可在中文/英文之间切换
- 一处切换，前后端统一生效——UI 文案、AI system prompt 同步变更
- 语言偏好持久化到 `~/Documents/apps/openzen/.openzen/locale.json`，重启后保持
- 不引入重型 i18n 框架，保持 OpenZen 的轻量风格

## 二、架构总览

```
┌──────────────────────────────────────────────────────────────┐
│  Frontend (Svelte 5)                                         │
│                                                              │
│  settings/locale/ → tauriInvoke("set_locale", { lang })──┐   │
│  onMount          → tauriInvoke("get_locale") → store    │   │
│  components       → import { t } from "../i18n"          │   │
│  i18n/{en,zh}.json ← 翻译键值对                           │   │
└──────────────────────────────────────────────────────────────┘
                              │ Tauri IPC
┌─────────────────────────────▼────────────────────────────────┐
│  Backend (Rust / Tauri v2)                                   │
│                                                              │
│  AppState { locale: Mutex<String> }                          │
│                                                              │
│  #[tauri::command]                                           │
│  fn get_locale(state) → String       // 读                    │
│  fn set_locale(lang, state, app)     // 写 + 通知前端          │
│                                                              │
│  run_agent_for_session():                                    │
│    ctx.lang = state.locale  // 替代 GA_LANG 环境变量          │
│    → load_system_prompt() 自动选 zh/en sys_prompt            │
└──────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────▼────────────────────────────────┐
│  Persistence                                                 │
│  ~/Documents/apps/openzen/.openzen/locale.json                  │
│  { "lang": "zh" }  或  { "lang": "en" }                      │
└──────────────────────────────────────────────────────────────┘
```

## 三、Rust 后端改动

### 3.1 AppState 扩展

文件：`src-tauri/src/lib.rs`

```rust
pub struct AppState {
    // ... 现有字段保持不变 ...
    pub locale: Mutex<String>,  // 新增："zh" 或 "en"
}
```

`AppState::new()` 中初始化时从文件读取：

```rust
fn load_locale() -> String {
    let path = data_dir().join("locale.json");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(lang) = parsed.get("lang").and_then(|v| v.as_str()) {
                let valid = match lang {
                    "zh" | "en" => true,
                    _ => false,
                };
                if valid {
                    return lang.to_string();
                }
            }
        }
    }
    // 默认中文
    "zh".to_string()
}
```

### 3.2 新增 Tauri Commands

```rust
#[tauri::command]
fn get_locale(state: State<'_, Arc<AppState>>) -> String {
    state.locale.lock().unwrap().clone()
}

#[tauri::command]
fn set_locale(
    lang: String,
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    let lang = lang.trim().to_lowercase();
    if lang != "zh" && lang != "en" {
        return Err(format!("Unsupported locale: {lang}"));
    }

    // 写入内存
    *state.locale.lock().unwrap() = lang.clone();

    // 持久化到磁盘
    let path = data_dir().join("locale.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = serde_json::json!({ "lang": lang });
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&content).unwrap_or_default());

    // 通知前端 language-changed 事件
    let _ = app.emit("language-changed", serde_json::json!({ "lang": lang }));

    Ok(())
}
```

### 3.3 System Prompt 语言绑定

修改 `run_agent_for_session()` 中 `tauri_ctx()` 的使用：

```rust
// 原来是：
let ctx = tauri_ctx();

// 改为：
let ctx = {
    let mut c = tauri_ctx();
    c.lang = state.locale.lock().unwrap().clone();
    c
};
```

`load_system_prompt()` 逻辑不变——已经根据 `ctx.lang == "en"` 选择 `sys_prompt_en.txt`。

### 3.4 Command 注册

在 `.invoke_handler(tauri::generate_handler![...])` 中加入：

```rust
get_locale,
set_locale,
```

### 3.5 影响范围估算

| 文件 | 改动量 |
|------|--------|
| `src-tauri/src/lib.rs` | +60 行（AppState 添加字段、初始化逻辑、2个命令、ctx.lang 绑定） |
| `src-tauri/Cargo.toml` | 无需改动（serde_json 已有） |

## 四、Svelte 前端改动

### 4.1 i18n 模块

新建目录和文件：

```
frontends/src/lib/i18n/
├── index.ts          ← 核心：init(), t(), switchLang()
├── en.json           ← 英文翻译
└── zh.json           ← 中文翻译
```

#### `index.ts` 核心实现

```typescript
import { writable, derived } from "svelte/store";
import { isTauri, tauriInvoke } from "../api/tauri";

// ── 翻译数据 ──
import en from "./en.json";
import zh from "./zh.json";

const translations: Record<string, Record<string, string>> = { en, zh };

// ── Store ──
export const locale = writable<string>("zh");
export const t = derived(locale, ($locale) => {
  return (key: string, fallback?: string): string => {
    return translations[$locale]?.[key] ?? fallback ?? key;
  };
});

// ── 初始化 ──
export async function initLocale(): Promise<void> {
  if (isTauri()) {
    try {
      const lang = await tauriInvoke("get_locale");
      locale.set(lang);
    } catch {
      locale.set("zh");
    }
  } else {
    // 非 Tauri 模式（浏览器开发）从 localStorage 读
    const saved = localStorage.getItem("openzen-locale");
    locale.set(saved === "en" ? "en" : "zh");
  }
}

// ── 切换 ──
export async function switchLocale(lang: "zh" | "en"): Promise<void> {
  locale.set(lang);
  if (isTauri()) {
    await tauriInvoke("set_locale", { lang });
  } else {
    localStorage.setItem("openzen-locale", lang);
  }
}
```

### 4.2 翻译文件结构

#### `zh.json`（中文）

```json
{
  "sidebar.newChat": "新建会话",
  "session.defaultName": "新会话",
  "session.msgs": "条消息",
  "session.delete": "删除会话",
  "chat.placeholder": "输入消息…（/ 查看命令）",
  "chat.placeholder.processing": "处理中…",
  "chat.stop": "停止运行",
  "chat.send": "发送消息",
  "chat.copy": "复制消息",
  "chat.regenerate": "重新生成",
  "chat.running": "运行中",
  "ask.title": "Agent 有问题需要确认",
  "ask.subtitle": "选择一个选项，或在下方输入你的回复。",
  "ask.suggestions": "建议",
  "ask.yourResponse": "你的回复",
  "ask.placeholder": "输入你的回复…（Enter 提交，Shift+Enter 换行，Esc 取消）",
  "ask.cancel": "取消",
  "ask.send": "发送回复",
  "ask.sending": "发送中…",
  "todo.title": "代办事项",
  "todo.completed": "已完成",
  "todo.inProgress": "进行中",
  "todo.pending": "待处理",
  "todo.cancelled": "已取消",
  "context.label": "上下文",
  "shortcuts.title": "键盘快捷键",
  "shortcuts.global": "全局",
  "shortcuts.new": "新建会话",
  "shortcuts.prevNext": "上一个/下一个会话",
  "shortcuts.toggleSidebar": "切换侧边栏",
  "shortcuts.showPanel": "显示本面板",
  "shortcuts.deleteSession": "删除当前会话",
  "shortcuts.messages": "消息操作",
  "shortcuts.copyLast": "复制最后一条回复",
  "shortcuts.regenerate": "重新生成",
  "shortcuts.focusSidebar": "将焦点移到侧边栏",
  "shortcuts.input": "输入框",
  "shortcuts.send": "发送消息",
  "shortcuts.newline": "换行",
  "shortcuts.cancel": "取消当前任务",
  "shortcuts.command": "命令",
  "shortcuts.dialog": "对话框",
  "shortcuts.dialogClose": "关闭",
  "shortcuts.dialogConfirm": "确认",
  "shortcuts.dialogFocus": "焦点移动",
  "sidebar.toggle": "切换侧边栏",
  "auth.required": "需要认证",
  "error.dismiss": "关闭",
  "help.title": "可用命令",
  "help.clear": "清空当前对话",
  "help.new": "开始新会话",
  "help.model": "显示 / 切换模型",
  "help.sessions": "列出所有会话",
  "help.export": "导出对话",
  "compact.title": "上下文压缩完成",
  "compact.before": "压缩前",
  "compact.after": "压缩后",
  "compact.saved": "释放",
  "compact.failed": "压缩失败",
  "model.title": "选择模型",
  "model.close": "关闭"
}
```

#### `en.json`（英文）

```json
{
  "sidebar.newChat": "New Chat",
  "session.defaultName": "New Chat",
  "session.msgs": "msgs",
  "session.delete": "Delete session",
  "chat.placeholder": "Type a message... (/ for commands)",
  "chat.placeholder.processing": "Processing...",
  "chat.stop": "Stop running task",
  "chat.send": "Send message",
  "chat.copy": "Copy message",
  "chat.regenerate": "Regenerate",
  "chat.running": "Running",
  "ask.title": "The agent has a question for you",
  "ask.subtitle": "Pick an option, or write your own response below.",
  "ask.suggestions": "Suggestions",
  "ask.yourResponse": "Your response",
  "ask.placeholder": "Type your reply… (Enter to submit, Shift+Enter for newline, Esc to dismiss)",
  "ask.cancel": "Cancel",
  "ask.send": "Send response",
  "ask.sending": "Sending…",
  "todo.title": "Tasks",
  "todo.completed": "completed",
  "todo.inProgress": "in progress",
  "todo.pending": "pending",
  "todo.cancelled": "cancelled",
  "context.label": "Context",
  "shortcuts.title": "Keyboard Shortcuts",
  "shortcuts.global": "Global",
  "shortcuts.new": "New session",
  "shortcuts.prevNext": "Previous / Next session",
  "shortcuts.toggleSidebar": "Toggle sidebar",
  "shortcuts.showPanel": "Show this panel",
  "shortcuts.deleteSession": "Delete current session",
  "shortcuts.messages": "Messages",
  "shortcuts.copyLast": "Copy last response",
  "shortcuts.regenerate": "Regenerate",
  "shortcuts.focusSidebar": "Focus sidebar",
  "shortcuts.input": "Input",
  "shortcuts.send": "Send message",
  "shortcuts.newline": "New line",
  "shortcuts.cancel": "Cancel current task",
  "shortcuts.command": "Commands",
  "shortcuts.dialog": "Dialog",
  "shortcuts.dialogClose": "Close",
  "shortcuts.dialogConfirm": "Confirm",
  "shortcuts.dialogFocus": "Move focus",
  "sidebar.toggle": "Toggle sidebar",
  "auth.required": "Authentication required",
  "error.dismiss": "Dismiss",
  "help.title": "Available commands",
  "help.clear": "Clear current conversation",
  "help.new": "Start a new chat session",
  "help.model": "Show / switch model",
  "help.sessions": "List all sessions",
  "help.export": "Export conversation",
  "compact.title": "Context compression complete",
  "compact.before": "Before",
  "compact.after": "After",
  "compact.saved": "Freed",
  "compact.failed": "Compression failed",
  "model.title": "Select Model",
  "model.close": "Close"
}
```

### 4.3 组件迁移模式

每个组件中，`import { t } from "../i18n"` 后使用 `$t("key")` 替换硬编码文本。

**迁移前**（Sidebar.svelte）：
```svelte
<button class="new-chat-btn" onclick={handleNewClick}>
  New Chat
</button>
```

**迁移后**：
```svelte
<script>
  import { t } from "../i18n";
</script>
<button class="new-chat-btn" onclick={handleNewClick}>
  {$t("sidebar.newChat")}
</button>
```

### 4.4 语言切换入口

在 `Settings` 或 `Sidebar` 中添加切换按钮：

```svelte
<script>
  import { locale, switchLocale } from "../i18n";
</script>

<button onclick={() => switchLocale($locale === "zh" ? "en" : "zh")}>
  {$locale === "zh" ? "EN" : "中文"}
</button>
```

- 点击后立即切换，无需刷新
- Tauri 事件 `language-changed` 可选监听，用于多窗口同步

### 4.5 需要改动的文件清单

| 文件 | 改动类型 |
|------|---------|
| `frontends/src/lib/i18n/index.ts` | **新建** |
| `frontends/src/lib/i18n/zh.json` | **新建** |
| `frontends/src/lib/i18n/en.json` | **新建** |
| `frontends/src/App.svelte` | 初始化 `initLocale()`，监听 `language-changed` |
| `frontends/src/lib/components/Sidebar.svelte` | "New Chat" → `$t(...)`，添加切换按钮 |
| `frontends/src/lib/components/SessionList.svelte` | "New Chat"、"msgs" → `$t(...)` |
| `frontends/src/lib/components/ChatInput.svelte` | placeholder、aria-label、/help 输出、/compact 输出 → `$t(...)` |
| `frontends/src/lib/components/AskUserDialog.svelte` | 全部 UI 文案 → `$t(...)` |
| `frontends/src/lib/components/ShortcutsPanel.svelte` | 全部中文 → `$t(...)` 实现双语 |
| `frontends/src/lib/components/TodoProgress.svelte` | "代办事项" 等 → `$t(...)` |
| `frontends/src/lib/components/ContextBar.svelte` | "上下文" → `$t(...)` |
| `frontends/src/lib/components/ChatMessage.svelte` | "Running"、"Copy message" → `$t(...)` |
| `frontends/src/lib/components/AuthDialog.svelte` | "Authentication required" → `$t(...)` |
| `frontends/src/lib/components/ModelSwitcher.svelte` | 对话框标题等 → `$t(...)` |
| `src-tauri/src/lib.rs` | 后端改动（见 §三） |

## 五、实现顺序

按依赖关系和独立程度分为 5 个阶段：

### Phase 1：后端基础设施 ✅ 已完成（~30 分钟）
- [x] `AppState` 添加 `locale` 字段
- [x] 实现 `load_locale()` 从文件读取
- [x] 实现 `get_locale` / `set_locale` 两个 Tauri commands
- [x] 修改 `run_agent_for_session` 中 `ctx.lang` 绑定
- [x] `load_system_prompt` 改用 `ctx.lang`（不再读 `GA_LANG` 环境变量）
- [x] 注册 commands + 编译验证

### Phase 2：前端 i18n 模块 ✅ 已完成（~20 分钟）
- [x] 创建 `frontends/src/lib/i18n/index.ts`
- [x] 创建 `zh.json` / `en.json` 翻译文件
- [x] 在 `App.svelte` 的 `onMount` 中调用 `initLocale()`

### Phase 3：组件迁移 ✅ 已完成（~90 分钟）
- [x] **高优先**：Sidebar、SessionList、ChatInput
- [x] **中优先**：AskUserDialog、TodoProgress、ContextBar、ChatMessage
- [x] **低优先**：ShortcutsPanel、AuthDialog、ModelSwitcher
- [x] **遗漏检查**：EditCard、ToolCallCard、SkillMcpCard、ThinkingBlock、ApprovalModal、MessageTreeNav、AgentPicker、TodoDetailBubble、App.svelte
- [x] TypeScript + Rust 编译验证：零新增错误

### Phase 4：语言切换入口 ✅ 已完成（~15 分钟）
- [x] 在 Sidebar 底部添加 ZH/EN 切换按钮
- [x] 调用 `switchLocale()` 一键切换前后端同步
- [x] 非 Tauri 模式下使用 localStorage 兜底

### Phase 5：清理 & 测试 ✅ 已完成（~15 分钟）
- [x] `tauri_ctx()` 改用 `load_locale()` 替代 `GA_LANG` 环境变量
- [x] 其他模式（CLI、TUI、Web Server）保留 `GA_LANG`（各自独立管理）
- [x] Rust 编译验证：通过
- [x] 前端 TypeScript 编译：零新增错误

## 五.1、已实现文件清单

| 文件 | 改动 |
|------|------|
| `src-tauri/src/lib.rs` | +70 行：data_dir()、load_locale()、locale 字段、get_locale/set_locale 命令、ctx.lang 绑定 |
| `frontends/src/lib/i18n/index.ts` | **新建**：核心 i18n 模块（~50 行） |
| `frontends/src/lib/i18n/zh.json` | **新建**：61 个中文翻译键 |
| `frontends/src/lib/i18n/en.json` | **新建**：61 个英文翻译键 |
| `frontends/src/App.svelte` | import + initLocale() + 4 处文本迁移 |
| `frontends/src/lib/components/Sidebar.svelte` | import + "New Chat" → $t + **语言切换按钮** |
| `frontends/src/lib/components/SessionList.svelte` | import + 4 处文本迁移 |
| `frontends/src/lib/components/ChatInput.svelte` | import + localT helper + 8 处文本迁移 |
| `frontends/src/lib/components/AskUserDialog.svelte` | import + 8 处文本迁移 |
| `frontends/src/lib/components/TodoProgress.svelte` | import + 2 处文本迁移 |
| `frontends/src/lib/components/ContextBar.svelte` | import + 1 处文本迁移 |
| `frontends/src/lib/components/ChatMessage.svelte` | import + 6 处文本迁移 + 2 个 helper 函数 |
| `frontends/src/lib/components/ShortcutsPanel.svelte` | import + 全部 18 处中文 → $t |
| `frontends/src/lib/components/AuthDialog.svelte` | import + 6 处文本迁移 |
| `frontends/src/lib/components/ModelSwitcher.svelte` | import + 4 处文本迁移 |
| `frontends/src/lib/components/EditCard.svelte` | import + 4 处文本迁移 |
| `frontends/src/lib/components/ToolCallCard.svelte` | import + 4 处文本迁移 |
| `frontends/src/lib/components/SkillMcpCard.svelte` | import + 4 处文本迁移 |
| `frontends/src/lib/components/ThinkingBlock.svelte` | import + 1 处文本迁移 |
| `frontends/src/lib/components/ApprovalModal.svelte` | import + 8 处文本迁移 + 2 个 helper 函数 |
| `frontends/src/lib/components/MessageTreeNav.svelte` | import + 1 处文本迁移 |
| `frontends/src/lib/components/AgentPicker.svelte` | import + 1 处文本迁移 |
| `frontends/src/lib/components/TodoDetailBubble.svelte` | import + 1 处文本迁移 |

## 六、设计决策记录

### 为什么不引入 svelte-i18n / i18next？
- OpenZen 字符串量小（~60 个 key），不需要重型框架的插值、复数、格式化功能
- 自制 `derived` store 方案约 30 行，零依赖，完全可控
- 减少前端 bundle 体积，Tauri app 不需要 npm 依赖膨胀

### 为什么默认语言是中文而不是英文？
- 当前 `ShortcutsPanel`、`ContextBar`、系统提示词默认都是中文
- 目标用户以中文为主
- 如果需要改为跟随系统语言，可以在 `load_locale()` 中调用 OS API 检测（macOS: `Locale.current.identifier`）

### 为什么用 Tauri event 通知前端而不是返回值？
- `set_locale` 可能有多个窗口（`open_session_window` 创建的子窗口）
- `app.emit("language-changed")` 能让所有窗口同步更新
- 单窗口场景下，返回值也够用——两种方式都保留

## 七、风险与注意事项

| 风险 | 缓解措施 |
|------|---------|
| 翻译遗漏：某个组件忘了迁移 | Phase 3 按组件逐个迁移，每完成一个就 grep 原字符串确认无残留 |
| 翻译 key 拼写错误 | TypeScript 类型安全：`t()` 返回 `string`，key 错误在运行时表现为 fallback 原文，不会崩溃 |
| 子窗口不同步 | 监听 `language-changed` 事件即可 |
| 非 Tauri 开发模式（`npm run dev`） | `isTauri()` 检测 + localStorage 兜底 |
| 旧 `GA_LANG` 环境变量残留 | ✅ Tauri 端已清理；CLI/TUI/Web Server 保留 GA_LANG |

## 八、附录：当前所有硬编码字符串清单

> 用于 Phase 3 逐项核对，确保无遗漏。

```
Sidebar.svelte:    "New Chat"
SessionList.svelte: "New Chat", "msgs", "Delete session"
ChatInput.svelte:   "Processing...", "Type a message... (/ for commands)",
                    "Stop running task", "Send message",
                    /help 输出（8 行英文）,
                    /compact 输出（5 行中文）
AskUserDialog:     "The agent has a question for you",
                    "Pick an option, or write your own response below.",
                    "Suggestions", "Your response",
                    "Type your reply…",
                    "Cancel", "Send response", "Sending…"
ShortcutsPanel:    "键盘快捷键", "全局", "新建会话", "上一个/下一个会话",
                    "切换侧边栏", "显示本面板", "删除当前会话",
                    "消息操作", "复制最后一条回复", "重新生成",
                    "将焦点移到侧边栏", "输入框", "发送消息", "换行",
                    "取消当前任务", "命令", "对话框", "关闭", "确认", "焦点移动"
TodoProgress:      "代办事项", "completed", "in progress", "pending", "cancelled"
ContextBar:        "上下文"
ChatMessage:       "Running", "Copy message"
AuthDialog:        "Authentication required"
ModelSwitcher:     (aria-label / title 文本)
App.svelte:        "New Chat" (create param)
```
