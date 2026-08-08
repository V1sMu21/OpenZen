---
name: verify-code-review
description: >
  OpenZen 项目代码审查（AI 助手用）。4 个并行 agent 从不同维度审查变更：
  Rust 正确性、Svelte 5 类型安全、项目约定合规、跨 crate 集成风险。
 使用 /verify-code-review 触发。对应 Claude Code /code-review——找 bug 非风格。
argument-hint: "[--fix] [target: file|branch|ref-range]"
user-invocable: true
disable-model-invocation: false
allowed-tools: [Read, Bash, Grep, Glob, Edit, LSP, Task]
---

# verify-code-review — Multi-Agent Code Review for OpenZen

审查当前 diff 的正确性 bug、类型错误、约定违反、跨 crate 集成风险。
不关心代码风格——那是 simplify 的工作。

## 审查范围

默认：`git diff HEAD` 的变更。可指定 target。

---

## 执行流程

### Phase 1: 变更分类

1. `git diff --name-only HEAD` 获取变更文件列表
2. 按语言分类：`.rs` → Rust，`.svelte`/`.ts` → 前端
3. Rust 变更涉及 3+ crates → 标记需要跨 crate 审查
4. 前端变更涉及 `stores/` 或 `App.svelte` → 标记深度审查

### Phase 2: 启动并行 Agent 审查

**强制要求：** 用 `task()` 启动以下 4 个 agent，ALL `run_in_background=true`，
ALL `category="quick"`（审查任务原子化），ALL `load_skills=[]`。

#### Agent 1: Rust 正确性

```
[CONTEXT]: 审查 openzen 项目当前 git diff 中的 .rs 文件变更。
项目是 Rust workspace (20+ crates)，Tauri v2 桌面应用。

[GOAL]: 找出所有会导致编译失败、panic、逻辑错误的问题。

[MUST DO]:
- 读每个变更的 .rs 文件及其 diff
- 检查：类型不匹配、缺失 use、unwrap() 在生产路径、空 catch 块、unsafe 无注释
- 检查：Mutex 潜在死锁、Arc clone 位置不当、错误吞没（let _ =）
- 检查：async fn 缺少 .await、tokio::spawn 无 handle
- 每个发现给出 file:line + 原因 + 修复建议

[MUST NOT DO]:
- 不检查未变更的代码
- 不报告风格问题（那是 simplify 的工作）
- 不确定的不要报——误报比漏报更糟
- 不要修改任何文件

[EXPECTED OUTPUT]: JSON 格式的发现列表，每个含 file/line/severity/description/fix
```

#### Agent 2: Svelte 5 / TypeScript

```
[CONTEXT]: 审查 openzen 项目 frontends/ 中当前 git diff 的 .svelte/.ts 变更。
项目使用 Svelte 5 + TypeScript strict + Tailwind 4。

[GOAL]: 找出所有类型错误、响应式误用、store 绕过问题。

[MUST DO]:
- 读每个变更的 .svelte/.ts 文件
- 检查：$state vs $bindable 混用（不需要 bind: 的 prop 用了 $bindable）
- 检查：as any / @ts-ignore 抑制类型错误
- 检查：$effect 依赖缺失或冗余
- 检查：绕过 chat.ts setter 直接改内部状态
- 检查：ChatMessage.svelte 中 isLive 条件 4 是否正确
- 每个发现给出 file:line + 原因 + 修复建议

[MUST NOT DO]:
- 不检查未变更代码
- 不报告 Tailwind 类名风格（那不是 bug）
- 不要修改任何文件

[EXPECTED OUTPUT]: JSON 格式发现列表
```

#### Agent 3: 项目约定合规

```
[CONTEXT]: 审查 openzen 项目当前 diff 是否违反项目约定。
项目约定来自 .opencode/skills/SKILL.md 第五节"通用约束"。

[GOAL]: 检查 7 条硬约束 + 项目目录分层约定。

[MUST DO]:
- 对照以下规则逐条检查每个变更文件：
  1. 禁止 $bindable 用于纯内部 UI 状态（如 collapsed）
  2. 禁止 @ts-ignore / as any
  3. 禁止修改 isLive 条件 4 而不考虑跨轮内容泄漏
  4. 禁止重构/优化与 Bug 无关代码（在 bugfix 中）
  5. 禁止空 catch 块 catch(e) {}
  6. Rust: 禁止 unsafe 块（除非标注原因且最小化）
  7. Rust: 新 pub API 必须有文档注释
- 检查目录分层：frontends/src/lib/{api,components,stores,utils}/

[MUST NOT DO]:
- 不报告不在上述 7 条中的风格偏好
- 不要修改任何文件

[EXPECTED OUTPUT]: 违规列表，标注违反哪条规则
```

#### Agent 4: 跨 Crate 集成

```
[CONTEXT]: 仅当 .rs 变更涉及 3+ crates 时运行此 agent。否则返回空。

[GOAL]: 检测跨 crate 的集成风险。

[MUST DO]:
- 检查是否存在 crate 间循环依赖（A→B 且 B→A）
- 检查 Cargo.toml 依赖版本是否与 workspace Cargo.toml 一致
- 检查 pub API 变更是否影响下游 crate
- 检查 feature flag 变更是否正确传播

[MUST NOT DO]:
- 不检查单 crate 内部逻辑

[EXPECTED OUTPUT]: 风险列表，或 "no cross-crate risks"
```

### Phase 3: 置信度评分

对每个发现评 0-100 分。只保留 ≥50 分的。

| 评分 | 含义 | 示例 |
|------|------|------|
| 100 | 确定会 panic/编译失败 | `unwrap()` on `None` |
| 75 | 高置信 | 明显的 type mismatch |
| 50 | 可能需确认 | 潜在的逻辑错误 |
| 25 | 低置信 | 主观判断 |
| 0 | 误报 | 不报告 |

### Phase 4: 输出

```
## Code Review: [范围]

### 🔴 Critical (≥75)
- file:line — 描述 → 修复建议

### 🟡 Warning (50-74)
- file:line — 描述 → 修复建议

### ✅ No Issues Found
（如果没有发现）

### 📊 Summary
Total: N | Critical: N | Warning: N
```

如果 `--fix` 传入，对 ≥75 分的发现用 Edit 工具自动修复，修复后跑 `lsp_diagnostics`。
