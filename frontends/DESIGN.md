---
name: OpenZen — Song Celadon (宋韵天青)
description: |
  A Song-dynasty Ru-ware celadon interface for OpenZen. The surface is a
  warm near-black ink night; the only chromatic accent is sky-azure
  (#93c3d6) — glaze light on celadon. Depth comes exclusively from
  surface-color shifts and 1px hairlines, never drop shadows. Typography is
  calligraphic: serif (Songti) for inscriptions, sans (system/PingFang) for
  body, mono for code.
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
/* Surface hierarchy — ink night (darkest → lightest) */
--color-canvas:          #14120e;  /* page background */
--color-surface:         #1a1712;  /* panels */
--color-surface-soft:    #1d1a14;  /* hover, input, tool cards */
--color-surface-elevated:#221e17;  /* cards, messages */
--color-surface-overlay: #28231b;  /* modals, dropdowns */

/* Brand — 天青 (Ru ware sky azure), the sole chromatic accent */
--color-primary:         #93c3d6;
--color-primary-hover:   #b6dbe8;
--color-primary-muted:   rgba(147, 195, 214, 0.12);
--color-accent:          #93c3d6;
--color-accent-soft:     rgba(147, 195, 214, 0.07);

/* Text — rice paper / moon white */
--color-ink:             #e4ddca;
--color-body:            #b8b0a3;
--color-muted:           #7a7366;
--color-dim:             #8a8170;  /* AA on canvas/surface */

/* Borders — azure-tinted hairlines */
--color-hairline:        rgba(147, 195, 214, 0.12);
--color-hairline-strong: rgba(147, 195, 214, 0.22);

/* Semantic */
--color-success:         #7ab3a8;
--color-warning:         #c4a877;
--color-error:           #c05a3e;  /* cinnabar seal */
--color-info:            #93c3d6;

/* Code */
--color-code-bg:         #0f0d0a;
--color-code-text:       #e0d9cc;
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
| **Primary** | Azure fill `var(--color-primary)`, `#14120e` text (dark-on-azure), 8px radius, 10px/20px padding, weight 500. Hover: `--color-primary-hover`. |
| **Secondary** | Transparent, 1px `var(--color-hairline-strong)`, `var(--color-ink)` text. Hover: border → `var(--color-primary)`. |
| **Ghost** | Transparent, `var(--color-body)` text. Hover: `var(--color-ink)`. |
| **Icon** | 32×32px (min 44×44 touch target on touch-capable layouts), transparent, `var(--color-body)` icon. Hover: bg `var(--color-surface-soft)`. |

### Inputs

| Element | Style |
|---------|-------|
| **Text input** | bg `var(--color-surface-soft)`, 1px `var(--color-hairline)`, 8px radius, 10px/14px padding. Focus: border `var(--color-primary)`, no ring. |
| **Textarea** | Same as input. Min-height 42px, max-height 160px, resize none. |
| **Select** | Same as input. Custom chevron. |

### Cards & Containers

| Element | Style |
|---------|-------|
| **Default card** | bg `var(--color-surface-elevated)`, 1px `var(--color-hairline)`, 12px radius, 16px padding. |
| **Message bubble (user)** | full-row translucent azure wash + 2px left azure hairline (`--color-primary-muted` / `--color-primary`), 2px radius. max-w clamp(280px, 68%, 100%) — scales with the message column. |
| **Message bubble (assistant)** | transparent ink-on-canvas paper; no container background; max-w 100% of the message column (column itself capped at 1200px, see `.messages-list`). |
| **Tool call card** | bg `var(--color-surface-soft)`, 1px `var(--color-hairline)`, 8px radius, 12px padding, mono text. |
| **Code block** | bg `var(--color-code-bg)`, 1px `var(--color-hairline)`, 8px radius, 16px padding. |

### Sidebar

| Element | Style |
|---------|-------|
| **Panel** | bg `var(--color-canvas)`, 280px width, 1px `var(--color-hairline)` right border. |
| **Session item** | 6px/12px padding, 8px radius. Hover: bg `var(--color-surface-soft)`. Active: bg `var(--color-primary-muted)` + left accent. |
| **New chat button** | Secondary-outline variant, full-width, azure text, serif label. |

### Typing Indicator
Three dots, 6px diameter, `var(--color-primary)`, 1.2s bounce animation.
bg `var(--color-surface-elevated)` pill, 16px radius, 12px/20px padding.

---

## 5. Layout Principles

**Page structure:**
```
┌──────────┬──────────────────────────────────┐
│ Sidebar  │           Chat Area               │
│ 280px    │     (messages + input)            │
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

**Max content width:** 720px for message text; assistant rows may clamp to
`clamp(360px, 78%, 720px)`. Input uses the chat column width.

---

## 6. Depth & Elevation

**Flat by default. No drop shadows, box-shadows, or drop-shadow filters.**
Elevation is communicated only by:
- surface steps (`--color-surface-soft` → `--color-surface-elevated` →
  `--color-surface-overlay`)
- 1px `--color-hairline` borders
- type weight contrast

**Exceptions:**
- Modals: `--color-surface-overlay` with a stronger hairline border
- Tool call cards: `--color-surface-soft` with a hairline border; no shadow

---

## 7. Do's and Don'ts

**Do:**
- Use `--color-primary` (#93c3d6) as the sole chromatic accent; cinnabar is
  only for errors/destructive actions
- Use surface steps and hairlines for hierarchy
- Show streaming tokens character-by-character
- Display tool calls as collapsible cards between messages
- Show timestamp + token counts in `--color-muted`
- Keep all interactive targets ≥44px
- Use local system font stacks (offline-first)

**Don't:**
- Use blue/purple gradients or a second brand accent
- Add any shadow / glow / box-shadow — depth comes from color
- Mix more than three families (sans/serif/mono)
- Use emojis in UI chrome (message content may contain them)
- Show raw JSON to users — render tool calls as readable cards

---

## 8. Responsive Behavior

| Breakpoint | Sidebar | Message width |
|------------|---------|---------------|
| > 1100px | 280px, visible | clamp(360px, 78%, 720px) assistant |
| 720–1100px | Collapsed, toggle button | 88% assistant / 78% user |
| < 720px | Hidden, slide-over overlay | Full width |

Touch targets: minimum 44px for all interactive elements.

---

## 9. Agent Prompt Guide

**Color reference:**
- Canvas: `#14120e`
- Primary (sky azure): `#93c3d6`
- Elevated surface: `#221e17`
- Ink text: `#e4ddca`
- Body text: `#b8b0a3`
- Hairline: `rgba(147, 195, 214, 0.12)`

**Prompt:** "Design a warm-dark, Song-celadon chat interface for an AI
companion. Use sky-azure (#93c3d6) as the only chromatic accent on a warm
ink-night canvas (#14120e). Messages are paper-like rows — user rows carry
a translucent azure wash with a left hairline, assistant rows are plain ink
on canvas. Tool calls appear as collapsible cards. Typography: system sans
for body/UI, Songti serif for inscriptions, mono for code. Absolutely no
shadows — use surface-color steps for elevation."
