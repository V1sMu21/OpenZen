---
name: verify-simplify
description: Post-implementation code cleanup and simplification. Reviews recent code changes for reuse opportunities, quality improvements, and efficiency gains. Automatically fixes redundant patterns without changing behavior.
tags: [simplify, cleanup, refactor, optimize, polish, code-quality, cargo-clippy, deduplicate]
---

# verify-simplify — Code Cleanup & Simplification

Review recently changed code for redundancy, over-complexity, and inefficiency. Apply fixes that make code simpler without changing behavior. This is the second step in the verification chain (after code-review, before verify-check).

## When to Use

- After code-review passes
- Before committing changes
- When you notice duplicated logic or overly complex code
- After accepting AI-generated code that needs polishing

## Required Tools

- bash (git diff, cargo clippy --fix)
- file_read (read changed files)
- file_edit (apply simplifications)
- grep (search for patterns across the codebase)

## Procedure

### Step 1: Identify Changes

```bash
git diff --name-only HEAD
git diff --name-only   # uncommitted
```

### Step 2: Three-Pass Analysis

For each changed file, run these three passes:

#### Pass A: Code Reuse

Search for duplicated patterns and missed abstractions:

**Rust:**
```bash
# Find functions that re-implement std library
grep -n "\.to_string()" changed_file.rs
grep -n "for .* in .*\.iter\(\)" changed_file.rs  # could be .for_each()?
grep -n "Arc::new\(Mutex::new" changed_file.rs   # redundant pattern?
grep -n "unwrap_or_else\(\|\|" changed_file.rs    # could be unwrap_or_default()?
```

Check if new code could use existing utilities in:
- `crates/oz-core/` — core agent utilities
- `crates/oz-tools/` — tool helpers
- Standard library (`std::fs`, `std::path`, etc.)

**Svelte/TypeScript:**
- 3+ components with identical slot/prop patterns → extract shared component
- Repeated CSS variable usage → use design system tokens
- Hand-written debounce/throttle → use existing util if available

#### Pass B: Code Quality

**Rust:**
- Unnecessary `.clone()` — can value be moved instead?
- `unwrap()` that could be `?` in functions returning Result
- Functions longer than 80 lines — suggest splitting
- Functions with 5+ parameters — suggest struct wrapping
- `String` parameters that could be `&str`
- `if let Some(x) = opt { x } else { return }` → use `let Some(x) = opt else { return }`
- Unused imports and variables

**Svelte/TypeScript:**
- `$state` declared but never mutated → `const`
- `$effect` that could be `$derived`
- Components larger than 200 lines → suggest splitting
- Duplicate prop type declarations

#### Pass C: Efficiency

**Rust:**
- `.clone()` inside loops
- `.to_string()` in hot paths
- `Vec::new()` followed by many `.push()` → use `Vec::with_capacity()`
- `collect::<Vec<_>>()` when only iteration is needed
- Independent async calls that could use `tokio::join!`
- `Mutex` where `RwLock` is more appropriate (read-heavy)

**Svelte:**
- Missing `key` in `{#each}` blocks (causes unnecessary DOM updates)
- `$derived` with too many dependencies (recomputes more than needed)

### Step 3: Apply Fixes

For each finding from Passes A-C:

1. Read the relevant code with file_read
2. Apply the simplification with file_edit
3. After EACH fix, run `cargo check` (Rust) to confirm no new errors
4. If a fix causes errors, revert it with file_edit and skip

### Step 4: Verify

```bash
# Confirm all fixes compile
cargo check --workspace 2>&1

# Run clippy to catch any issues
cargo clippy --workspace -- -D warnings 2>&1
```

### Step 5: Report

```
## Simplify Results

### ♻️ Reuse (N fixes)
- file.rs:42 — replaced hand-rolled clone loop with Vec::clone_from()

### ✨ Quality (N fixes)
- file.rs:15 — extracted 6-param function into Config struct

### ⚡ Efficiency (N fixes)
- file.rs:88 — added Vec::with_capacity() before loop

### Summary
Applied N simplifications. No behavior changed. All checks pass.
```

## Important Rules

- NEVER change behavior — only HOW code does things, not WHAT it does
- NEVER delete code that "looks unused" unless absolutely certain it's dead
- NEVER simplify code in files unrelated to the current change
- After each fix, verify with `cargo check` before proceeding
- If in doubt about a simplification, skip it and note it as a suggestion
