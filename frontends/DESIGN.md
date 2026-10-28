---
name: OpenZen — Xuan Qing (玄青)
description: |
  An ink-night stoneware interface for OpenZen. The surface is a deep
  blue-black clay (#080b0b); the only chromatic accent is celadon jade
  (#6fd8c4) — glaze light seen across a dark room. Gold (#e2be7a) is
  reserved for what the hand touches (send, seal, approve). Depth comes
  exclusively from surface-color shifts and 1px hairlines, never drop
  shadows. Typography is calligraphic: serif (Songti) for inscriptions,
  sans (system/PingFang) for body, mono for code.
---

## 1. Visual Theme & Atmosphere

**宋韵天青 / Song Celadon.** A craftsman's dark studio: warm ink-night
canvas, celadon sky-azure as the single functional color, and cinnabar red
reserved for errors (the seal on a vessel). Light is communicated by glaze
layers (surface steps), not by shadows.

**Mood:** quiet, focused, precise. A long-running companion, not a flashy
tool.

**Density:** comfortable. Messages breathe; tool activity is collapsible.

---

## 2. Color Palette & Roles

```css
/* Surface hierarchy — 玄色陶胎 (darkest → lightest) */
--color-canvas:          #080b0b;  /* page background */
--color-surface:         #0c1111;  /* panels */
--color-surface-soft:    #111a19;  /* hover, input, tool cards */
--color-surface-elevated:#16201f;  /* cards, messages */
--color-surface-overlay: #1b2725;  /* modals, dropdowns */

/* Brand — 天青 (celadon jade), the sole structural accent */
--color-primary:         #6fd8c4;
--color-primary-hover:   #93ead8;
--color-primary-muted:   rgba(111, 216, 196, 0.12);
--color-accent:          #6fd8c4;
--color-accent-soft:     rgba(111, 216, 196, 0.07);

/* Text — moon white / celadon grey */
--color-ink:             #e9f1ee;
--color-body:            #b9c7c3;
/* Light theme (html.theme-light): muted #635c51, dim #5f594e — both AA on
   every light surface, including --bg-tertiary. */
--color-muted:           #7c8d89;  /* 5.46:1 on surface; ≥12px non-critical
                                      text only */
--color-dim:             #93a5a1;  /* 6.46:1 on elevated — secondary text
                                      that must stay readable in modals */

/* Borders — azure-tinted hairlines */
--color-hairline:        rgba(111, 216, 196, 0.14);
--color-hairline-strong: rgba(111, 216, 196, 0.26);

/* Semantic */
--color-success:         #5fd3a3;
--color-warning:         #e2be7a;
--color-error:           #f2705a;  /* cinnabar seal */
--color-info:            #6fd8c4;

/* Status aliases — the "state at a glance" trio used by run-state dots,
   banners and switches. `--color-status-ok` aliases the accent so the
   activity colour has one source. */
--color-status-ok:       var(--color-primary);
--color-status-warn:     #c8862a;
--color-status-danger:   #c0392b;

/* Diff stat (+N / −M). Not --color-success/--color-error: those are
   semantic-only hues (and stay dark-valued in light mode), so diff counts
   get their own pair that each theme tunes for contrast. */
--color-diff-add:        #5fd3a3;
--color-diff-del:        #f2705a;

/* Code */
--color-code-bg:         #070c0b;
--color-code-text:       #dfe4ea;
```

---

## 3. Typography Rules

Three families only: sans for body/UI, serif for inscriptions/titles, mono
for code. Do not import web fonts in the production document; use local
system stacks so the desktop app renders identically offline.

| Token | Size | Weight | Line-Height | Letter-Spacing | Use |
|-------|------|--------|-------------|----------------|-----|
| `display-xl` | 32px | 600 | 1.15 | -0.5px | Page titles |
| `display-md` | 24px | 600 | 1.2 | -0.3px | Section heads |
| `title-lg` | 18px | 600 | 1.3 | -0.2px | Card titles |
| `title-md` | 16px | 600 | 1.4 | 0 | Component titles |
| `body-md` | 15px | 400 | 1.55 | 0 | Default body |
| `body-sm` | 13px | 400 | 1.5 | 0 | Captions |
| `caption` | 12px | 500 | 1.4 | 0 | Badges |
| `caption-up` | 11px | 600 | 1.4 | 0.8px | Uppercase labels |
| `code-md` | 14px | 400 | 1.6 | 0 | Code blocks |
| `code-sm` | 12px | 400 | 1.5 | 0 | Inline code |
| `button` | 14px | 500 | 1.0 | 0 | Button labels |

**CSS variables in `src/app.css`:**
`--font-sans` = `-apple-system, BlinkMacSystemFont, "PingFang SC",
"Noto Sans SC", system-ui, sans-serif`
`--font-serif` = `"Songti SC", "Noto Serif SC", "Source Han Serif SC", serif`
`--font-kai` = `"Kaiti SC", "STKaiti", "KaiTi", serif`
`--font-mono` = `ui-monospace, "SF Mono", Menlo, "Fira Code", monospace`

---

## 4. Component Stylings

### Buttons

| Variant | Style |
|---------|-------|
| **Primary** | Jade fill `var(--color-primary)`, `--color-primary-ink` text, 9px radius, 10px/16px padding, weight 600. Hover: `--color-primary-hover`. |
| **Secondary** | Transparent, 1px `var(--color-hairline-strong)`, `var(--color-ink)` text, 9px radius. Hover: border → `var(--color-primary)`. |
| **Ghost** | Transparent, `var(--color-body)` text. Hover: `var(--color-ink)`. |
| **Icon** | 32×32px, 9px radius, transparent, `var(--color-dim)` icon. Hover: bg `var(--color-surface-soft)`, icon `var(--color-ink)`. |

### Inputs

| Element | Style |
|---------|-------|
| **Text input** | bg `var(--color-surface-soft)`, 1px `var(--color-hairline)`, 10px radius, 10px/14px padding. Focus: border `color-mix(primary 55%)` + 3px `--focus-ring`. |
| **Textarea** | Same as input. Min-height 42px, max-height 160px, resize none. |
| **Select** | Same as input. Custom chevron. |

### Assistant 气泡状态行（2026-10-28）

长任务不再把工具卡片留在气泡里（旧的“时间线折叠 + 保留最近 5 张卡”已移除）。气泡只呈现两态：

| 状态 | 内容 |
|------|------|
| 运行中 | 单行 `>_ 正在执行 <活动>`。`>_` 为 `--color-primary` 等宽字体；活动取**最新一个仍在执行的工具**（并发时只显示最新那张卡的内容，如「编辑文件 App.svelte」「运行代码」）；没有在跑的工具时显示「思考中…」。右侧 6×13px 天青光标以 1.1s steps 闪烁。 |
| 已完成 | 顶部单行 `✓ 已完成，用时 8分13秒`（`message.duration` 毫秒，>=1h 带小时；无耗时则只显示「已完成」），随后只渲染最终回复文本。 |

输入框上方的后台任务/子代理条：数量 > 1 时折叠成单颗计数胶囊（`5 个后台任务` / `3 个子代理`），点击展开完整列表（`▴` 收起）；只有 1 个时直接显示该条。胶囊上的圆点在有运行中任务时呼吸。

中间回复：只有**最后一段**文本算最终回复（直接显示在气泡里）；此前穿插在工具之间的"我先看一下…"等中间文本，按原时间顺序折进详情区（左侧虚线、字号小一号），不再拼进最终回复。

交互：状态行本身是按钮，**点击展开**本轮被折叠的思考块、工具卡、编辑卡与干预卡（`detailsOpen`，默认收起，全部渲染不截断；展开区最高 42vh 可滚动），再点收起。没有工具/思考时不可展开（无 chevron）。

约束：默认收起时运行中不渲染思考块、工具卡、编辑卡、干预卡，完成后也不渲染；历史消息同样适用；异常退出仍由底部 `exit-reason-banner` 说明。时长文案走 i18n（`message.completedIn` / `message.duration.minSec` 等）。

### Cards & Containers

| Element | Style |
|---------|-------|
| **Default card** | bg `var(--color-surface-elevated)`, 1px `var(--color-hairline)`, 12px radius, 16px padding. |
| **Message bubble (user)** | Right-aligned jade slab: gradient `primary 13% → 7%`, 1px jade 20% border, radius `16 16 5 16`, max-w 680px. |
| **Message bubble (assistant)** | Transparent ink-on-canvas paper; no container; a 9.5px `助手` label, max-w 100% of the 860px message column. |
| **Tool call row** | 32px row, radius 10, 6px status dot, name + mono arg + duration + status; expanded body indented 24px, radius 12, `--color-surface-soft`. |
| **Code block** | bg `var(--color-code-bg)`, 1px `var(--color-hairline-plain)`, 12px radius, 14px/16px padding, mono 12.5px. |

### Sidebar

| Element | Style |
|---------|-------|
| **Panel** | bg `var(--color-surface)`, 276px width, 1px `var(--color-hairline)` right border. |
| **Session item** | 32px row, 7px/8px padding, 10px radius, single line + short date. Hover: bg `var(--color-surface-soft)`. Active: jade 9% wash + 2px left bar. |
| **New chat button** | Filled jade gradient (`#43a996 → #2b7f71`), 38px, 11px radius, 12.5px 600 label + `⌘N` keycap. |

### Typing Indicator
Three dots, 6px diameter, `var(--color-primary)`, 1.2s bounce animation.
bg `var(--color-surface-elevated)` pill, 16px radius, 12px/20px padding.

---

## 5. Layout Principles

**Page structure:**
```
┌──────────┬──────────────────────────────────┐
│ Sidebar  │           Chat Area               │
│ 276px    │     (messages + input)            │
│          │                                    │
│ Sessions │   ┌─ message row ──────────────┐  │
│ list     │   │  user / agent content      │  │
│          │   └────────────────────────────┘  │
│          │                                    │
│          │   ┌─ input area ────────────────┐ │
│          │   │  [textarea         ] [Send] │ │
│          └────────────────────────────────────┘
```

**Grid:** single chat column plus optional right artifact panel. No nested
multi-column inside the message area.

**Spacing scale (8px base):**
`4 / 8 / 12 / 16 / 20 / 24 / 32 / 48 / 64`

**Max content width:** 860px for the whole message column (~44 CJK glyphs per
line). The composer spans the same column; wider windows get margins, not
longer lines.

---

## 6. Depth & Elevation

**Flat by default.** Message rows, cards, list items and panels communicate
elevation only by:
- surface steps (`--color-surface-soft` → `--color-surface-elevated` →
  `--color-surface-overlay`)
- 1px `--color-hairline` borders
- type weight contrast

A `box-shadow` on any of those elements is a defect.

### 6.1 Permitted shadow roles (executable exception list)

Exactly three roles may use `box-shadow`. Anything else — in particular a
depth shadow on a card, row, or list item — is a violation. The whitelist
below is the full set of call sites at the time of writing; `grep -rn
"box-shadow" src/` must not return a component outside it.

| Role | Recipe | Why it is not "depth" | Whitelisted call sites |
|---|---|---|---|
| **R1 · Overlay** | `0 20px 60px rgba(0,0,0,.4), 0 2px 8px rgba(0,0,0,.2)` (or a single softer step) | Separates a floating surface from the page so it reads as *above* the document; without it a modal over dense text is unreadable | `ApprovalModal`, `AskUserDialog`, `AuthDialog`, `ModelSwitcher`, `CommandPalette`, `ProjectList`, `ProjectItem` (context menu), `TodoDetailBubble`, `SettingsPanel` (edge shadow), `App.svelte` (toast) |
| **R2 · Glaze** | `var(--glaze-shadow)` — `inset 0 1px 0 <hairline>` + `0 12px 40px rgba(0,0,0,.38)` | The composer/inset "釉光" treatment: an inner highlight plus a soft halo that makes an *input* read as recessed. Theme-aware via the token | `app.css` (`.glaze`), `ChatInput`, `ToolCallCard` (`.tcc-body`), plus `--glaze-shadow` itself |
| **R3 · State ring / glow** | `0 0 0 <n>px <semantic token>` or `0 0 <n>px var(--state-color)` | Not depth at all: it is a focus/attention/status indicator drawn with a shadow primitive (ring, error pulse, run-state dot halo) | `app.css` (focus rings, primary glow), `ChatInput` (error ring + focus ring), `ChatMessage` (attention pulse), `App.svelte` (run-state dot) |
| **R4 · Inset hairline** | `inset -1px 0 0.5px <hairline>` | A 1px edge drawn inset because a real border would change the element's box size (the sidebar is width-animated). Purely an edge, never a gradient | `Sidebar` (right edge) |

Verification (run it after touching any component; every hit must be in
R1–R4):

```bash
grep -rn "box-shadow" src/ --include=*.svelte --include=*.css
```

Rules for all three roles:
- Colour comes from a token or from `rgba(0,0,0,α)`/`rgba(255,255,255,α)`
  neutral scrim — never a second brand hue.
- Rings must be `0 0 0 npx` (no blur) so they never read as depth.
- A new overlay component may reuse R1 without asking; a new *card* may not.

### 6.2 Brand inscriptions (deliberately not translated)

Two strings in the title bar are **brand calligraphy, not UI copy**, and are
exempt from the i18n rule (they are marked `aria-hidden` and carry no
meaning a user must act on):

- `OpenZen` — the product wordmark; `玄 青` — the empty-state inscription.
- `丙午 制` — "made in the 丙午 year" (the 2026 sexagenary year), a seal-style
  inscription. Rendering it in English would destroy the seal metaphor.

Every other user-visible string must go through `$t()`.

---

## 7. Do's and Don'ts

**Do:**
- Use `--color-primary` (#6fd8c4) as the sole structural accent; gold
  (#e2be7a) only for what the hand touches (send seal, approve, update);
  cinnabar is only for errors/destructive actions
- Use surface steps and hairlines for hierarchy
- Show streaming tokens character-by-character
- Display tool calls as collapsible cards between messages
- Show timestamp + token counts in `--color-muted` (≥12px; it clears 4.6:1 on the
  canvas/surface steps but not on `--color-surface-overlay` — use `--color-dim`
  for text inside modals)
- Keep all interactive targets ≥44px
- Use local system font stacks (offline-first)

**Don't:**
- Use blue/purple gradients or a second brand accent
- Add any shadow / glow / box-shadow outside the three roles in §6.1 —
  depth comes from color
- Mix more than three families (sans/serif/mono)
- Use emojis in UI chrome (message content may contain them)
- Show raw JSON to users — render tool calls as readable cards

---

## 8. Responsive Behavior

| Breakpoint | Sidebar | Message width |
|------------|---------|---------------|
| > 1180px | 276px, visible | 860px column, centred |
| 900–1180px | 276px, visible | column shrinks to 100% − 48px |
| 720–900px | Collapsed, toggle button | column − 32px |
| < 720px | Hidden, slide-over overlay | full width |

Touch targets: minimum 44px for all interactive elements.

---

## 9. Agent Prompt Guide

**Color reference:**
- Canvas: `#080b0b`
- Primary (celadon jade): `#6fd8c4`
- Gold (seals / approval): `#e2be7a`
- Elevated surface: `#16201f`
- Ink text: `#e9f1ee`
- Body text: `#b9c7c3`
- Hairline: `rgba(111, 216, 196, 0.14)`

**Prompt:** "Design a cool-dark, ink-night celadon interface for an AI
companion. Jade (#6fd8c4) is the only structural colour; gold (#e2be7a) is
reserved for seals and approval; cinnabar for errors. Canvas is #080b0b.
User messages are a jade-glazed slab on the right; assistant replies are
plain ink on canvas with 32px activity rows. Typography: system sans for
body, Songti for inscriptions, Kaiti for thinking, mono for code. No blur,
no blend modes, no shadows except floating overlays — depth comes from
surface steps plus 1px hairlines."
