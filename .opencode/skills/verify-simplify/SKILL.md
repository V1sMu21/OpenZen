---
name: verify-simplify
description: >
  OpenZen 项目代码简化（AI 助手用）。3 个并行 agent（复用/质量/效率）
  审查变更，自动修复冗余、重复、性能问题。不改行为只改结构。
  对应 Claude Code /simplify——实现后 / 提交前抛光。
argument-hint: "[focus: error handling | store patterns | async code | memory]"
user-invocable: true
disable-model-invocation: false
allowed-tools: [Read, Bash, Grep, Glob, Edit, LSP]
---

# verify-simplify — Code Cleanup for OpenZen

实现后运行，自动清理冗余代码。不改行为，只改结构。提交前必跑。

---

## 执行流程

### Phase 1: 变更识别

```bash
git diff --name-only HEAD && git diff HEAD
```

### Phase 2: 三 Agent 并行审查

#### Agent 1: Code Reuse（代码复用）

```
[CONTEXT]: 审查 openzen 项目 git diff 中的 Rust 和 Svelte 代码是否有重复逻辑。
项目是 Rust workspace + Svelte 5 前端 + Tauri v2。

[GOAL]: 找出可复用的已有工具/函数替代手写代码。

[MUST DO - Rust]:
- 检查手写的字符串处理/路径拼接 → 能否用 std::path 或已有 oz-* 工具
- 检查重复的 Arc<Mutex<T>> 包装 → 能否提取类型别名
- 检查多个 crate 中相同错误转换 → 能否统一到某个 crate
- 检查手写 serialize/deserialize → 能否用 #[derive] 宏
- 检查 inline logic → 能否用 oz-core / oz-tools 已有函数

[MUST DO - Svelte]:
- 检查 3+ 组件相同的 slot/prop 组合 → 建议提取
- 检查重复 CSS 变量声明 → 应使用 app.css @theme
- 检查手写 debounce/throttle → 用已有 util

[MUST NOT DO]:
- 不建议删除实际上不同的代码（表面相似但逻辑不同）
- 不强制合并导致过度抽象

[EXPECTED OUTPUT]: 每个发现 file:line + 替代方案 + 修复代码
```

#### Agent 2: Code Quality（代码质量）

```
[CONTEXT]: 审查 openzen 项目 diff 的代码质量问题。

[GOAL]: 找出降低可读性和可维护性的模式。

[MUST DO - Rust]:
- 不必要的 clone()：值已不用，可直接 move
- unwrap() → ? 或 .context("...")
- 函数 >80 行 → 建议拆分
- 参数 >5 → 建议 struct 封装
- String 参数 → &str（如果不需要所有权）
- if let + else { panic!()/return } → let ... else 语法
- 注释与代码不一致
- 未使用的 import/use

[MUST DO - Svelte]:
- $state 从未修改 → const
- $effect → $derived（如果只是计算）
- 组件 >200 行 → 建议拆分
- Props 重复声明 → 提取 interface
- 条件渲染嵌套 >3 层 → 提取子组件

[MUST NOT DO]:
- 不改变行为语义
- 不在 bugfix 中重构无关代码
```

#### Agent 3: Efficiency（性能效率）

```
[CONTEXT]: 审查 openzen 项目 diff 的性能问题。

[GOAL]: 找出不必要的分配、错过并发机会、热路径膨胀。

[MUST DO - Rust]:
- 循环内 clone() → 移到循环外
- 循环内 to_string() → 避免重复分配
- Vec::new() + push() → Vec::with_capacity()
- collect::<Vec<_>>() 仅需迭代 → 不收集
- 独立 async 调用 → tokio::join!
- read_to_string 大文件 → BufReader
- Mutex vs RwLock 选择不当

[MUST DO - Svelte]:
- {#each} 缺 key → 不必要 DOM 重建（只报告，不自动修）
- $derived 依赖过多 → 不必要重算

[MUST NOT DO]:
- 不做微优化（1% 以内的改善不自动修）
- 如果代码不在热路径 → 降级为建议
```

### Phase 3: 聚合与修复

1. 收集 3 个 agent 发现，去重
2. 排除伪阳性后，对每个有效发现用 Edit 工具应用修复
3. 修复后运行 `lsp_diagnostics` 确认无新错误
4. 如果修复导致错误 → 回滚，标记为 [跳过]

### Phase 4: 输出

```
## Simplify: [范围]

### ♻️ Reuse (N)
- file:line — 做什么（为什么）

### ✨ Quality (N)
- file:line — 做什么（为什么）

### ⚡ Efficiency (N)
- file:line — 做什么（为什么）

### 📊 Summary
N fixes applied. Run git diff to review.
```
