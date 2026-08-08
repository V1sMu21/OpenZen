---
name: OpenZen — Song Celadon
description: |
  A Song-dynasty celadon interface for the OpenZen AI assistant. Based on
  Anthropic Claude's design language, inverted for a dark developer-tool surface.
  The system anchors on a warm near-black canvas with coral-terracotta accents,
  editorial serif display headlines, and a clean humanist sans for body/UI.
  The warmth distinguishes it from cool-blue / slate AI tools.
---

## 1. Visual Theme & Atmosphere

**Warm-dark editorial.** A developer's terminal meets a literary journal. The
canvas is warm near-black (`#181715`) — not the cool `#0a0a0a` of typical dev
tools. The coral accent (`#cc785c`) provides brand voltage without screaming.
Depth comes from surface-color shifts and thin hairlines, not drop shadows.

**Mood:** focused, warm, precise. Feels like a craft workspace at night.

**Density:** comfortable. Not sparse, not crowded. Messages breathe.

---

## 2. Color Palette & Roles

```css
/* Surface hierarchy — warm dark */
--canvas:         #181715;  /* page background */
--surface-elevated: #252320;  /* sidebar, cards */
--surface-soft:   #1f1e1b;  /* hover, input bg */
--surface-overlay: #2d2a25;  /* modals, dropdowns */

/* Brand */
--primary:        #cc785c;  /* coral — send button, active state */
--primary-hover:  #d4896f;  /* hover state */
--primary-muted:  rgba(204,120,92,0.15);  /* subtle bg */

/* Text */
--ink:            #faf9f5;  /* primary text */
--body:           #c4c1b8;  /* secondary text */
--muted:          #8a877d;  /* captions, timestamps */
--dim:            #5e5b52;  /* placeholder, disabled */

/* Borders */
--hairline:       #2a2824;  /* 1px subtle borders */
--hairline-strong:#3a3731;  /* stronger borders */

/* Semantic */
--success:        #5db872;
--warning:        #d4a017;
--error:          #c64545;
--info:           #5db8a6;

/* Code */
--code-bg:        #11100e;  /* inline code bg */
--code-text:      #e8e2d5;
```

### Light mode (future)
```css
--canvas:         #faf9f5;
--ink:            #141413;
--body:           #3d3d3a;
```

---

## 3. Typography Rules

**Display/headings:** Use Inter (sans-serif) at weight 600 with tight
negative letter-spacing. The warmth comes from spacing and color, not
typeface choice.

**Body:** Inter at weight 400, comfortable line-height (1.55).

**Code:** JetBrains Mono at 14px.

**UI labels:** Inter weight 500, uppercase with positive letter-spacing.

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

**Fallback stack:** `Inter, -apple-system, BlinkMacSystemFont, "Segoe UI",
Roboto, sans-serif` for body/UI. `JetBrains Mono, "SF Mono", "Fira Code",
monospace` for code.

---

## 4. Component Stylings

### Buttons

| Variant | Style |
|---------|-------|
| **Primary** | Coral fill `--primary`, white text, 8px radius, 10px/20px padding, weight 500. Hover: `--primary-hover`. |
| **Secondary** | Transparent, 1px `--hairline-strong`, `--ink` text. Hover: border → `--primary`. |
| **Ghost** | Transparent, `--body` text. Hover: `--ink` text. |
| **Icon** | 32x32px, transparent, `--body` icon. Hover: bg `--surface-soft`. |

### Inputs

| Element | Style |
|---------|-------|
| **Text input** | bg `--surface-soft`, 1px `--hairline`, 8px radius, 10px/14px padding. Focus: border `--primary`, no ring. |
| **Textarea** | Same as input. Min-height 42px, max-height 160px, resize none. |
| **Select** | Same as input. Custom chevron. |

### Cards & Containers

| Element | Style |
|---------|-------|
| **Default card** | bg `--surface-elevated`, 1px `--hairline`, 12px radius, 16px padding. |
| **Message bubble (user)** | bg `--primary` at 15% opacity + 1px `--primary` at 30%, 12px radius bottom-right 4px, max-w 80%. |
| **Message bubble (assistant)** | bg `--surface-elevated`, 1px `--hairline`, 12px radius bottom-left 4px, max-w 80%. |
| **Tool call card** | bg `--surface-soft`, 1px `--hairline`, 8px radius, 12px padding, mono text. |
| **Code block** | bg `--code-bg`, 1px `--hairline`, 8px radius, 16px padding. |

### Sidebar

| Element | Style |
|---------|-------|
| **Panel** | bg `--canvas`, 240px width, 1px `--hairline` right border. |
| **Session item** | 6px/12px padding, 8px radius. Hover: bg `--surface-soft`. Active: bg `--primary-muted` + 1px `--primary` left border. |
| **New chat button** | Primary variant, full-width, 8px/12px padding. |

### Typing Indicator
Three dots, 6px diameter, `--primary` color, 1.2s bounce animation.
bg `--surface-elevated` pill, 16px radius, 12px/20px padding.

---

## 5. Layout Principles

**Page structure:**
```
┌──────────┬──────────────────────────────────┐
│ Sidebar  │           Chat Area               │
│ 240px    │     (messages + input)            │
│          │                                    │
│ Sessions │   ┌─ message bubble ──────────┐   │
│ list     │   │  user / agent content     │   │
│          │   └───────────────────────────┘   │
│          │                                    │
│          │   ┌─ input area ────────────────┐ │
│          │   │  [textarea         ] [Send] │ │
│          └────────────────────────────────────┘
```

**Grid:** Single sidebar-column layout. No multi-column inside chat.
Message area fills remaining space.

**Spacing scale (8px base):**
`4 / 8 / 12 / 16 / 20 / 24 / 32 / 48 / 64`

**Max content width:** 720px for message text. Input full-width.

---

## 6. Depth & Elevation

**Flat by default.** No drop shadows. Elevation via:
- Surface color shifts (`--surface-soft` → `--surface-elevated` → `--surface-overlay`)
- 1px `--hairline` borders
- Type weight contrast

**Exceptions:**
- Modals: `--surface-overlay` bg with subtle inner border glow
- Tool call cards: `--surface-soft` with 1px `--primary-muted` left accent bar

---

## 7. Do's and Don'ts

**Do:**
- Use `--primary` (#cc785c) as the sole chromatic accent — one coral moment per view
- Let surface-color shifts communicate hierarchy
- Show streaming tokens character-by-character
- Display tool calls as collapsible cards between messages
- Show message timestamp + token count in `--muted` text
- Use `/` commands for power-user actions

**Don't:**
- Use blue or purple accents — they clash with the coral warmth
- Add drop shadows — depth comes from color
- Mix more than two typefaces (Inter + JetBrains Mono only)
- Use emojis in UI chrome
- Show raw JSON to users — render tool calls as readable cards

---

## 8. Responsive Behavior

| Breakpoint | Sidebar | Message width |
|------------|---------|---------------|
| > 900px | 240px, visible | 80% max |
| 600–900px | Collapsed, toggle button | 90% max |
| < 600px | Hidden, slide-over overlay | Full width |

Touch targets: minimum 44px for all interactive elements.

---

## 9. Agent Prompt Guide

**Color reference:**
- Canvas: `#181715`
- Primary (coral): `#cc785c`
- Elevated surface: `#252320`
- Ink text: `#faf9f5`
- Body text: `#c4c1b8`
- Hairline: `#2a2824`

**Prompt:** "Design a warm-dark chat interface for an AI agent. Use a
coral-on-warm-black palette. Messages are bubbles — user on right, agent on
left. Include a sidebar with session history. Show streaming text as it
arrives. Tool calls appear as collapsible cards between messages.
Typography: Inter for body/UI, JetBrains Mono for code. No shadows — use
surface-color shifts for elevation."
