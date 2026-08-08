---
name: verify-design
description: UI design compliance checker for OpenZen's Song Celadon (宋韵天青) design system. Verifies that frontend changes follow the color palette, typography, component styling, layout, and depth rules defined in frontends/DESIGN.md.
tags: [design, ui, check, style, css, color, typography, layout, svelte, frontend, song-celadon, DESIGN.md]
---

# verify-design — Song Celadon UI Compliance

Check frontend changes against the Song Celadon design system defined in `frontends/DESIGN.md`. This is the final step in the verification chain — only run when `.svelte` or `.css` files have changed.

## When to Use

- After making UI changes (Svelte components, CSS, Tailwind classes)
- Before merging frontend PRs
- When adding new components or modifying existing UI
- Fourth step in the verification chain (only if frontend changed)

## Required Tools

- bash (git diff)
- file_read (DESIGN.md, changed .svelte/.css files)
- grep (search for color values, font declarations, shadow usage)

## Pre-requisite

First, read the design system:

```bash
file_read frontends/DESIGN.md
file_read frontends/src/app.css
```

## Procedure

### Step 0: Scope Check

```bash
# Only run if frontend files changed
git diff --name-only HEAD | grep -E '\.(svelte|css)$'
```

If no `.svelte` or `.css` files changed → report "⊘ No UI changes — design check skipped."

### Step 1: Color Palette Check

The Song Celadon palette uses warm ink-black canvas with sky-azure accents. 

Read each changed file and check:

| Rule | Check Method |
|------|-------------|
| **NO blue or purple accents** | `grep -n '#[0-9a-fA-F]\{6\}'` in changed files. Flag any `#4a90d9`, `#7c3aed`, `#6366f1` etc. |
| **Primary MUST be sky-azure** | All accent colors should use `--color-primary` (#81b5c7) or tailwind `bg-primary` |
| **Canvas MUST be ink-black** | Background should be `--color-canvas` (#080808) |
| **Surface hierarchy correct** | Elevated > Soft > Canvas in brightness. Check for wrong color on wrong layer |
| **Semantic colors correct** | Success=#7ab3a8, Warning=#c4a877, Error=#c44d4d, Info=#81b5c7 |

**Key violations to flag:**
- 🔴 Any use of blue, purple, or other non-celadon accent colors
- 🔴 Primary accent color replaced with wrong value
- 🟠 Surface color used on wrong layer (e.g., canvas color on a card)

### Step 2: Typography Check

| Rule | Check Method |
|------|-------------|
| **Only Inter + JetBrains Mono** | `grep -n 'font-family'` for any other fonts |
| **No emoji in UI chrome** | Grep for emoji characters in component files (not chat messages) |
| **Display headings: weight 600, negative letter-spacing** | Check `font-weight` and `letter-spacing` on headings |
| **Code: 14px, JetBrains Mono** | Check `font-size` and `font-family` on code blocks |

### Step 3: Component Styling Check

For each changed Svelte component, verify against these rules:

**Buttons:**
- Primary: `bg-primary`, white text, `rounded-lg`, `px-5 py-2.5`
- Secondary: transparent, `border border-hairline-strong`
- Ghost: transparent, hover `text-ink`
- Icon: 32×32px, hover `bg-surface-soft`

**Inputs:**
- Background: `bg-surface-soft`
- Border: `border border-hairline`
- Border-radius: `rounded-lg`
- Focus: `border-primary`, NO ring/outline

**Cards:**
- Default: `bg-surface-elevated`, `border-hairline`, `rounded-xl`, `p-4`
- Message (user): `bg-primary` 15% opacity, `rounded-xl rounded-br-sm`
- Message (assistant): `bg-surface-elevated`, `border-hairline`, `rounded-xl rounded-bl-sm`
- Tool call: `bg-surface-soft`, `border-hairline`, `rounded-lg`, `p-3`
- Code block: `bg-code-bg`, `border-hairline`, `rounded-lg`, `p-4`

**Sidebar:**
- Width: 240px
- Background: `bg-canvas`
- Right border: `border-r border-hairline`
- Session item active: `bg-primary-muted` + left accent bar

### Step 4: Layout Check

| Rule | Check |
|------|-------|
| Single column in chat area | No multi-column layout in main content |
| Max content width 720px | Check `max-w` constraints |
| Spacing follows 8px base | Check padding/margin: 4,8,12,16,20,24,32,48,64 |

### Step 5: Depth Check

| Rule | Check Method |
|------|-------------|
| **NO drop shadows** | `grep -n 'shadow\|box-shadow\|drop-shadow'` — flag ALL uses |
| Depth via color shifts | Check that elevation is expressed through `--surface-*` colors, not shadows |

This is the most important check. Song Celadon uses flat design — NO shadows anywhere.

### Step 6: Animation Check

| Rule | Check |
|------|-------|
| Typing indicator: 3 dots, 6px, `--primary`, 1.2s bounce | Verify if indicator exists |
| No 3D transforms | `grep -n 'rotate[XYZ]\|scale3d\|matrix3d'` |

### Step 7: Report

```
## Design Check Results

### 🔴 Critical Violations (block merge)
- file.svelte:42 — used blue (#4a90d9) instead of sky-azure (--color-primary)
- file.css:15 — box-shadow on card component (Song Celadon uses flat design)

### 🟠 Major Violations (should fix)
- file.svelte:88 — card uses wrong surface color (--surface-soft instead of --surface-elevated)
- file.svelte:120 — button missing rounded-lg

### 🟡 Minor Issues (nice to fix)
- file.css:30 — margin is 7px, not on 8px grid

### 🔵 Nits (optional)
- file.svelte:55 — consider using --color-primary-muted for subtle highlight

### Summary
Critical: N | Major: N | Minor: N | Nits: N
Verdict: ✅ PASS / ⚠️ NEEDS FIX / ❌ BLOCKED
```

## Important Rules

- Only check CHANGED files (from git diff), not the whole codebase
- Cross-reference with `frontends/DESIGN.md` — it is the source of truth
- Song Celadon is FLAT design — any `box-shadow` or `drop-shadow` is an automatic 🔴 Critical
- Color violations involving blue/purple are 🔴 Critical
- Do NOT modify code unless user explicitly requests `--fix`
- The DESIGN.md may have been updated — always read it fresh before checking
