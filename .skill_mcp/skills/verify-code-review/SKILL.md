---
name: verify-code-review
description: Multi-agent orthogonal code review for Rust and Svelte/TypeScript code changes. Finds bugs, logic errors, type issues, and project convention violations in recently changed files.
tags: [code-review, bug-hunt, review, correctness, rust, svelte, verify, cargo-check, cargo-clippy, type-check]
---

# verify-code-review — Multi-Agent Code Review

Review recently changed code for correctness bugs, type errors, logic issues, and project convention violations. This skill runs multiple independent checks and aggregates findings.

## When to Use

- After implementing a feature or fixing a bug
- Before committing or opening a PR
- When asked to "review my changes" or "check for bugs"
- As the first step in the verification chain

## Required Tools

- bash (for cargo check, cargo clippy, cargo fmt, git diff)
- file_read (to read changed files)
- file_edit (to apply fixes, only with user approval)

## Procedure

### Step 1: Identify Changed Files

```bash
git diff --name-only HEAD
git diff --name-only   # uncommitted changes
```

Classify each file: `.rs` → Rust, `.svelte`/`.ts` → Frontend, `.toml`/`.json` → Config

### Step 2: Run Automated Checks

Run ALL of these in parallel (use background tasks if available):

```bash
# Rust: compile check (fastest)
cargo check --workspace 2>&1

# Rust: lint check
cargo clippy --workspace -- -D warnings 2>&1

# Rust: format check
cargo fmt --check 2>&1

# Frontend: TypeScript type check (if js/ts changed)
npx tsc --noEmit 2>&1
```

### Step 3: Manual Diff Review

For each changed file, read the diff and check for:

**Rust-specific:**
- `unwrap()` in production paths — should use `?` or `.context()`
- Missing `?` on Result types
- `clone()` that could be a move
- Empty catch blocks `catch(e) {}`
- `unsafe` blocks without documented justification
- Potential deadlocks (nested Mutex locks)
- Async functions called without `.await`

**Svelte 5 / TypeScript specific:**
- `$bindable` used for props that don't need two-way binding
- `$state` variable never mutated → should be `const`
- `as any` or `@ts-ignore` suppressing type errors
- Direct mutation of store state bypassing setter methods
- `$effect` with missing or redundant dependencies
- `{#each}` blocks missing `key` expressions

**Project conventions (from AGENTS.md / openzen skill):**
- No `$bindable` for internal UI state like `collapsed`
- No `@ts-ignore` / `as any` for type errors
- No refactoring unrelated code in bugfix PRs

### Step 4: Severity Classification

For each finding, classify severity:

| Severity | Criteria |
|----------|----------|
| 🔴 Critical | Will cause compile error, panic, or data corruption |
| 🟠 Major | Likely bug or convention violation |
| 🟡 Minor | Code smell, suboptimal but not broken |
| 🔵 Nit | Style preference, optional improvement |

### Step 5: Report

Output findings grouped by severity:

```
## Code Review Findings

### 🔴 Critical (N)
- file.rs:42 — unwrap() will panic on None (use .context("...")?)
- ...

### 🟠 Major (N)
- ...

### 🟡 Minor (N)
- ...

### ✅ No Issues Found
(if none)

### Summary
Critical: N | Major: N | Minor: N
```

### Step 6: Fix (if requested)

Only fix issues if the user explicitly asks or approves. Apply fixes with file_edit, then re-run step 2 to verify no new issues introduced.

## Important Rules

- Only flag issues in the CHANGED code, not pre-existing issues
- If uncertain whether something is a bug, mark it as 🟡 Minor with a note
- Do NOT fix issues automatically unless the user requests it
- After fixes, ALWAYS re-run `cargo check` to confirm no compile errors
