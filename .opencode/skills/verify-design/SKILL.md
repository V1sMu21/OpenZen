---
name: verify-design
description: >
  OpenZen UI 设计合规检查（AI 助手用）。对照 frontends/DESIGN.md（Song Celadon 宋韵天青）
  逐条验证颜色、字体、组件、布局、深度规则。前端变更后运行。
  对应 Anthropic 团队内部 /design skill——UI 守门员。
argument-hint: "[--fix] [path: component or file]"
user-invocable: true
disable-model-invocation: false
allowed-tools: [Read, Bash, Grep, Glob, Edit, LSP]
---

# verify-design — Song Celadon UI Gatekeeper

前端 diff 中检查是否遵守 Song Celadon 设计规范。不对非 UI 变更运行。

---

## 执行

### Phase 0: 触发判断

```bash
git diff --name-only HEAD | grep -E '\.(svelte|css)$'
```

无前端变更 → 输出 "⊘ No UI changes — design check skipped."

### Phase 1: 加载规范

读取：
- `frontends/DESIGN.md` — 完整设计系统
- `frontends/src/app.css` — 当前 CSS 变量

---

### Phase 2: 十项合规检查

对每个变更的 `.svelte` / `.css` 文件执行以下检查：

#### 1. 颜色调色板 🔴

| 检查 | 方法 |
|------|------|
| 禁止蓝/紫色 | `grep '#[0-9a-fA-F]\{6\}'` → 对照 DESIGN.md 调色板 |
| 主色必须是 sky-azure | 检查 Tailwind class 是 `bg-primary` 或 CSS `var(--color-primary)` |
| 表面色层级正确 | 亮度: canvas < surface-soft < surface-elevated < surface-overlay |
| 语义色正确 | success=#7ab3a8, warning=#c4a877, error=#c44d4d, info=#81b5c7 |

#### 2. 字体排版 🟠

| 检查 | 方法 |
|------|------|
| 仅 Inter + JetBrains Mono | `grep font-family` |
| 无 emoji in UI chrome | grep emoji in 组件（非消息内容） |
| Display 级 weight 600 + 负 letter-spacing | 检查 heading 样式 |

#### 3. 按钮样式 🟠

Primary: `bg-primary` white text `rounded-lg` `px-5 py-2.5`
Secondary: transparent `border border-hairline-strong`
Ghost: transparent hover `text-ink`
Icon: 32×32 hover `bg-surface-soft`

#### 4. 输入框 🟡

bg `bg-surface-soft`, border `border-hairline`, radius `rounded-lg`, focus `border-primary` NO ring

#### 5. 卡片/容器 🟠

Card: `bg-surface-elevated` + `border-hairline` + `rounded-xl` + `p-4`
Message user: `bg-primary` 15% + `rounded-xl rounded-br-sm`
Message assistant: `bg-surface-elevated` + `rounded-xl rounded-bl-sm`
Tool call: `bg-surface-soft` + `rounded-lg` + `p-3`
Code: `bg-code-bg` + `rounded-lg` + `p-4`

#### 6. 侧边栏 🟡

Width 240px, bg `bg-canvas`, right border `border-hairline`, active item `bg-primary-muted` + left accent

#### 7. 布局 🟡

单列 chat area, max-w 720px for msg text, 间距 8px 基数 (4/8/12/16/20/24/32/48/64)

#### 8. 深度与阴影 🔴

| 检查 | 方法 |
|------|------|
| **NO drop shadows** | `grep 'shadow\|box-shadow\|drop-shadow'` — ALL uses flagged |
| 深度通过颜色 | 只允许 surface-* 色阶，不允许阴影 |

**这是最重要的检查。Song Celadon 是扁平设计——零阴影。**

#### 9. 响应式 🟡

>900px: sidebar 240px visible; 600-900px: collapsed toggle; <600px: overlay
Touch target ≥44px

#### 10. 动画 🟡

Typing indicator: 3 dots 6px `--primary` 1.2s bounce
禁止 3D transforms

---

### Phase 3: 严重性分级

| 级别 | 条件 | 示例 |
|------|------|------|
| 🔴 Critical | 品牌色违规、阴影 | 蓝色 accent、box-shadow |
| 🟠 Major | 组件样式违规 | 卡片用了错误 surface 色 |
| 🟡 Minor | 排版/间距偏差 | 字号差 1px |
| 🔵 Nit | 建议 | 间距非 8px 倍数 |

### Phase 4: 输出

```
## Design Check: [范围]

### 🔴 Critical (N)
- file:line — 违规 → 期望 → 修复

### 🟠 Major (N)
- file:line — 违规 → 期望 → 修复

### 🟡 Minor (N)
- file:line — 说明

### 🔵 Nits (N)
- file:line — 建议

### 📊 Summary
Critical: N | Major: N | Minor: N | Nits: N
Verdict: ✅ PASS / ⚠️ NEEDS FIX / ❌ BLOCKED
```

`--fix` 自动修复 Critical + Major；修复后跑 `lsp_diagnostics`。

---

## 约束

- ❌ 不检查 `.rs` `.toml` `.json` 等非 UI 文件
- ❌ 不强制 Nits 修复
- ✅ 每次检查前重新读取 `frontends/DESIGN.md`
- ✅ `--fix` 后验证 `lsp_diagnostics`
