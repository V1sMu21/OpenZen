# Phase A 验证计划 & 脚本

> 配套：`desktop-ui-keyboard-design.md` §八 Phase A
> 原则：每完成一个模块立即验证，不等全做完

---

## 验证策略

| 模块 | 验证方式 | 关键风险 |
|------|---------|---------|
| `diffWords()` | `node` 纯函数测试 | 边界 case 漏掉 → diff 渲染空白或崩溃 |
| TransientsBar 挂载 | 浏览器 visual | 组件已存在，一行 import 无风险 |
| ContextBar | 浏览器 visual + 布局检查 | flex 布局被破坏，scroll 失效 |
| 时间线折叠 | 浏览器 visual | `$derived` 计算 bug → parts 全消失 |
| 键盘快捷键 | 浏览器交互 | `preventDefault()` 吞掉正常输入 |
| SessionList 导航 | 浏览器交互 | index 越界 → crash |

**浏览器验证方式**：`npm run dev`（Vite :5173），在 Chrome/Firefox 中打开 http://localhost:5173。

---

## A-1：diffWords() 纯函数验证

### 脚本

保存以下内容为 `scripts/verify_diffwords.mjs`，运行 `node scripts/verify_diffwords.mjs`。

```javascript
// scripts/verify_diffwords.mjs
// 验证 EditCard 的 diffWords() 行内词级 diff
// 用法：node scripts/verify_diffwords.mjs
//
// 注意：此脚本在 EditCard.svelte 中 diffWords() 实现后运行。
// 实现前此处为占位——函数签名已定义，测试用预期行为。

/**
 * diffWords(oldLine, newLine) → { removed: string[], added: string[] }
 * 从 EditCard.svelte 中同步复制（或在此独立定义后回写）
 */
function diffWords(oldLine, newLine) {
  // ── 实现见 EditCard.svelte，此处为镜像副本用于独立验证 ──
  // 注意：编辑 EditCard.svelte 后请同步更新此函数。
}

// ── 测试用例 ──
const TESTS = [
  { old: "hello",       new: "hello",        desc: "完全相同的单词",      wantRem: [],      wantAdd: [] },
  { old: "foo",         new: "bar",          desc: "单单词替换",          wantRem: ["foo"], wantAdd: ["bar"] },
  { old: "hello world", new: "hello universe",desc: "多单词，1个变",       wantRem: ["world"],wantAdd: ["universe"] },
  { old: "",            new: "new",          desc: "空→有",               wantRem: [],      wantAdd: ["new"] },
  { old: "old",         new: "",             desc: "有→空",               wantRem: ["old"], wantAdd: [] },
  { old: "a b c",       new: "a x c",        desc: "中间单词变",          wantRem: ["b"],   wantAdd: ["x"] },
  { old: "abc def",     new: "abc def ghi",  desc: "追加单词",            wantRem: [],      wantAdd: ["ghi"] },
  { old: "abc def ghi", new: "abc def",      desc: "删除末尾单词",        wantRem: ["ghi"], wantAdd: [] },
];

let failed = 0;
for (const t of TESTS) {
  const result = diffWords(t.old, t.new);
  const remOk = JSON.stringify(result.removed.sort()) === JSON.stringify(t.wantRem.sort());
  const addOk = JSON.stringify(result.added.sort()) === JSON.stringify(t.wantAdd.sort());
  const pass = remOk && addOk;
  if (!pass) {
    failed++;
    console.error(`FAIL: ${t.desc}`);
    console.error(`  old:     "${t.old}"`);
    console.error(`  new:     "${t.new}"`);
    console.error(`  got rem: ${JSON.stringify(result.removed)}`);
    console.error(`  got add: ${JSON.stringify(result.added)}`);
    console.error(`  want rem:${JSON.stringify(t.wantRem)}`);
    console.error(`  want add:${JSON.stringify(t.wantAdd)}`);
  } else {
    console.log(`  PASS: ${t.desc}`);
  }
}

if (failed === 0) {
  console.log(`\n✅ All ${TESTS.length} tests passed.`);
  process.exit(0);
} else {
  console.error(`\n❌ ${failed}/${TESTS.length} tests FAILED.`);
  process.exit(1);
}
```

### 验收

```bash
node scripts/verify_diffwords.mjs
# 期望：All 8 tests passed, exit 0
```

---

## A-2：ContextBar 渲染验证

### 场景 1：空状态渲染

打开 http://localhost:5173（无 session 时手动创建一个）。

**验证点**：
- [ ] `.context-bar` 元素存在
- [ ] 显示"上下文: 0 / 128K"
- [ ] 进度条宽度 0%
- [ ] `.messages-list` 正常渲染空状态
- [ ] `.model-bar` 在底部可见

### 场景 2：有消息时

发送一条消息（如 "hello"），等待 agent 回复完成。

**验证点**：
- [ ] 进度条有颜色（绿色 < 70%）
- [ ] token 计数更新（tokensIn / tokensOut 不为 0）
- [ ] 发送第二条消息后百分比上升

### 场景 3：布局回归

**验证点**：
- [ ] 发送消息后页面自动滚动到底部
- [ ] 键盘 `End` 也能滚到底
- [ ] 向上滚动历史消息正常
- [ ] ChatInput 始终可见（未被 ContextBar 挤出视口）
- [ ] 窄屏（< 900px）下 ContextBar 不溢出

### 验证脚本（浏览器 console）

在浏览器 DevTools Console 中运行：

```javascript
// 快速 DOM 检查
(function() {
  const bar = document.querySelector('.context-bar');
  const msgs = document.querySelector('.messages-list');
  const model = document.querySelector('.model-bar');
  const input = document.querySelector('.input-area');
  
  console.assert(bar, '❌ ContextBar missing');
  console.assert(msgs, '❌ messages-list missing');
  console.assert(model, '❌ model-bar missing');
  console.assert(input, '❌ input-area missing');
  
  // 检查 DOM 顺序：context-bar 应在 messages-list 前
  const container = document.querySelector('.chat-container');
  if (container) {
    const children = container.children;
    const barIdx = Array.from(children).indexOf(bar);
    const msgIdx = Array.from(children).indexOf(msgs);
    console.assert(barIdx < msgIdx, '❌ ContextBar not before messages-list');
  }
  
  console.log('ContextBar check done');
})();
```

---

## A-3：时间线折叠验证

### 前提

需要一个有 6+ 个 parts 的 assistant 消息。Part 类型混合（text、reasoning、tool-invocation）。

**如何构造**：在 ChatInput 中输入一段会触发多次 tool call 的 prompt，或直接在浏览器 console 中 mock：

```javascript
// 浏览器 console —— 模拟 7 个 parts
const { chat } = await import('/src/lib/stores/chat.ts');
chat.update(s => ({
  ...s,
  streamingParts: [
    { type: 'reasoning', id: 'r1', text: 'Thinking step 1...', state: 'done' },
    { type: 'tool-invocation', toolCallId: 't1', name: 'grep', args: '{}', state: 'output-available', result: 'done', durationMs: 1200 },
    { type: 'reasoning', id: 'r2', text: 'Thinking step 2...', state: 'done' },
    { type: 'tool-invocation', toolCallId: 't2', name: 'read', args: '{}', state: 'output-available', result: 'done', durationMs: 800 },
    { type: 'reasoning', id: 'r3', text: 'Thinking step 3...', state: 'done' },
    { type: 'tool-invocation', toolCallId: 't3', name: 'edit', args: '{"file_path":"a.ts","old_string":"x","new_string":"y"}', state: 'output-available', result: 'done', durationMs: 500 },
    { type: 'text', id: 'txt1', text: 'Final answer here.', state: 'done' },
  ],
  messages: [
    ...s.messages,
    { id: 'mock-1', role: 'assistant', content: 'mock', timestamp: new Date().toISOString(), streaming: false, children: [], duration: 3000, tokensIn: 500, tokensOut: 100 }
  ]
}));
```

### 场景 1：默认折叠

**验证点**：
- [ ] 折叠标题"▶ 活动时间线"可见
- [ ] 标题显示"(2项已折叠 · 2.5s · 🔧2)"（前 2 个 parts 被折叠）
- [ ] 可见区域显示后 5 个 parts
- [ ] 第 1 个 reasoning("Thinking step 1") 不可见
- [ ] 最后的 text("Final answer here.") 可见

### 场景 2：展开

**验证点**：
- [ ] 点击折叠标题 → 标题变为 "▼ 活动时间线"
- [ ] 全部 7 个 parts 可见
- [ ] "Thinking step 1" 现在可见

### 场景 3：再次折叠

**验证点**：
- [ ] 再次点击标题 → 回到折叠状态
- [ ] 标题变回 "▶ 活动时间线"

### 场景 4：≤5 个 parts

设置 `streamingParts` 只有 4 个 parts。

**验证点**：
- [ ] **不出现**折叠标题
- [ ] 全部 4 个 parts 正常渲染

### 场景 5：回归——正常消息不受影响

发送一条简单消息（如 "hi"），观察回复。

**验证点**：
- [ ] 文本正常渲染
- [ ] thinking block 可展开/折叠
- [ ] tool call card 正常显示
- [ ] bubble footer（时间、duration、tokens）正常
- [ ] copy/regenerate 按钮正常

---

## A-4：键盘快捷键验证

### 场景 1：ChatInput 聚焦时——全局快捷键

1. 聚焦 ChatInput textarea
2. 按 `Cmd+N`（Mac）/ `Ctrl+N`（Win）

**验证点**：
- [ ] 创建了新 session（sidebar 出现新条目）
- [ ] ChatInput 中**没有**输入字母 'n'
- [ ] 焦点仍在 ChatInput

3. 创建 3 个 sessions
4. 聚焦 ChatInput，按 `Cmd+]` 2 次

**验证点**：
- [ ] 切换到下一个 session
- [ ] ChatInput 没有输入 ']'

5. 按 `Cmd+[`

**验证点**：
- [ ] 切换到上一个 session

6. 按 `Cmd+Shift+S`

**验证点**：
- [ ] Sidebar 切换显示/隐藏

7. 按 `Cmd+/`

**验证点**：
- [ ] ShortcutsPanel 弹出
- [ ] 面板显示完整快捷键列表
- [ ] 按 `Escape` 关闭面板

8. 按 `Cmd+Shift+D`

**验证点**：
- [ ] 当前 session 被删除
- [ ] 自动切换到下一个 session

### 场景 2：ChatInput 未聚焦时——单键快捷键

1. 确保有一条 assistant 回复
2. 点击聊天区域空白处（ChatInput 失去焦点）
3. 按 `C`

**验证点**：
- [ ] 最后一条 assistant 消息被复制到剪贴板
- [ ] 粘贴后内容正确

4. 按 `R`

**验证点**：
- [ ] 触发 regenerate（重新生成按钮的行为）

5. 按 `↑`

**验证点**：
- [ ] 焦点移到侧边栏第一个 session

### 场景 3：侧边栏聚焦时

1. 焦点在侧边栏后，按 `↓`

**验证点**：
- [ ] 高亮移到下一个 session
- [ ] 按 `↓` 在最后一个时不越界

2. 按 `↑`

**验证点**：
- [ ] 高亮移到上一个 session
- [ ] 按 `↑` 在第一个时不越界

3. 按 `Enter`

**验证点**：
- [ ] 切换到高亮的 session

4. 按 `Delete`

**验证点**：
- [ ] 删除高亮的 session
- [ ] 焦点移到下一个（或上一个，如果删的是最后一个）
- [ ] 不报错

5. 按 `Escape`

**验证点**：
- [ ] 焦点回到 ChatInput

### 场景 4：回归——ChatInput 输入不受影响

1. 聚焦 ChatInput
2. 输入大写 `C`（无 modifier）

**验证点**：
- [ ] 输入了 'C'（没有触发复制）
- [ ] 输入 `R`、`N`、`S`、`D` — 均正常输入

3. 输入中文（切换 IME）

**验证点**：
- [ ] 中文输入正常

---

## A-5：TransientsBar 挂载

### 场景

TransientsBar 是已有组件，只需在 App.svelte 中挂载。

**验证点**：
- [ ] 页面加载后不出现任何 TransientsBar（因为没有 transient data）
- [ ] 正常使用不受影响

**如何触发通知**：后端 emit `data_compressing_context` 后才能看到。在 Phase B 完成前，用 console 模拟：

```javascript
// 浏览器 console
const { chat } = await import('/src/lib/stores/chat.ts');
chat.update(s => ({
  ...s,
  streamingParts: [
    ...s.streamingParts,
    { type: 'data', id: 'test', dataType: 'data_compressing_context', content: 'Compressing context: 124K → 18K tokens', transient: true }
  ]
}));
```

**验证点**：
- [ ] 顶部出现通知条"Compressing context: 124K → 18K tokens"
- [ ] 4 秒后自动消失
- [ ] 通知条不阻挡 ChatInput 点击

---

## 回归检查清单（Phase A 全部完成后）

运行所有场景后，确认以下原有功能未受损：

- [ ] 发送消息 → 收到回复
- [ ] 流式文本逐字显示
- [ ] ThinkingBlock 可展开/折叠
- [ ] ToolCallCard 显示工具名/参数/结果
- [ ] EditCard 展开后显示 diff
- [ ] 切换到另一个 session → 消息加载正确
- [ ] 删除 session → session 从列表消失
- [ ] 新建 session → 空聊天页
- [ ] 页面刷新 → 恢复当前 session 历史消息
- [ ] AskUserDialog 弹出 → 输入回复 → agent 继续
- [ ] ModelSwitcher 打开 → 选择模型
- [ ] ChatInput 的 `/help` `/clear` `/new` `/model` `/sessions` `/export` 命令正常

---

## 总结

| 模块 | 测试数 | 独立验证方式 |
|------|--------|-------------|
| diffWords() | 8 cases | `node scripts/verify_diffwords.mjs` |
| ContextBar | 3 场景 | 浏览器 visual + console 检查 |
| 时间线折叠 | 5 场景 | 浏览器 visual + mock data |
| 键盘快捷键 | 4 场景 | 浏览器交互 |
| TransientsBar | 1 场景 | 浏览器 visual + console mock |
| 回归 | 14 项 | 浏览器交互 |
