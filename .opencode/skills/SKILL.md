---
name: openzen
description: >
  OpenZen 项目技能总纲。聚合所有 OpenZen 专业子 skill，提供统一的上下文入口。
  当工作在 openzen 项目时，用本 skill 快速判断需要加载哪个子 skill。
---

# OpenZen Project Skill — 项目级技能总纲

> 本项目级 skill 注册在 `.opencode/skills/`，当 OpenCode 在 `openzen` 目录下工作时生效。
> 它不替代各子 skill，而是告诉你**什么时候调用哪个子 skill**。

---

## 一、可用子 skill 清单

以下 skill 已在用户级注册（`~/.config/opencode/skills/`），通过 `skill(name="xxx")` 或 `load_skills=["xxx"]` 加载：

| Skill 名 | 用途 | 触发关键词 |
|----------|------|-----------|
| `openzen-icon` | Tauri macOS 应用图标生成。将源图处理为 1:1 正方形 → `cargo tauri icon` → 全平台图标 + .icns | icon, 图标, Dock, macOS icon, app icon |
| `openzen-tauri-debug` | OpenZen Tauri 桌面端渲染/状态/流式/SSE Bug 调试。包含 4 个时间态、13 个渲染机制、ChatMessage 门控变量详解 | 渲染不对, 流式卡住, isLive 不对, 状态不更新, SSE 事件, Tauri 桌面端 Bug |
| `openzen-debug-loop` | 完全自主的自我迭代 Bug 修复。多角度正交诊断 → 交叉收敛 → 契约验证 → 三层闭环。专治「没有 groundtruth 标准」的 Bug | bug, 错误, 修复, 崩溃, 异常, 自洽验证, 变异测试 |
| `code-review` | 正交代码审查委员会。六位专家（安全/性能/架构/可维护/合规/边界）并行审查 → 首席汇总 → 修复闭环 | review, 审查, code review, CR, QA, 质量, 正交审查 |
| **`tauri-e2e`** | **Tauri 桌面端全自动 E2E 测试**：CGEvent 注入输入 + VL 模型截图验证 + 闭环修复。覆盖 82 个测试用例 | Tauri 测试, E2E, CGEvent, 截图验证, VL 模型, 自动化测试, send message, debug loop |

以下为 `openzen` 项目级 skill（注册在 `.opencode/skills/`），通过 `/skill-name` 调用：

| Skill 名 | 用途 | 触发关键词 |
|----------|------|-----------|
| **`verify-code-review`** | **OpenZen 代码审查**：4 个并行 agent（Rust 正确性 / Svelte5 类型 / 约定合规 / 跨 crate 集成）审查 diff。对应 Claude Code `/code-review` | code review, bug, correctness, 审查代码, 查 bug, 跨 crate |
| **`verify-simplify`** | **OpenZen 代码简化**：3 个并行 agent（复用/质量/效率）审查并自动修复冗余、重复、性能问题。对应 Claude Code `/simplify` | simplify, cleanup, 简化, refactor, 优化, 清理, 去冗余 |
| **`verify-check`** | **OpenZen 端到端验证**：四级管道（编译→测试→Clippy→Tauri E2E+VL 截图）。对应 Claude Code `/verify` | verify, check, 验证, 编译, 测试, build, E2E |
| **`verify-design`** | **Song Celadon UI 合规**：对照 `frontends/DESIGN.md` 逐条验证颜色/字体/组件/布局/深度。对应 Anthropic 内部 `/design` | design, UI, 设计, 样式, 颜色, 字体, 组件, CSS |

---

## 二、技能选择决策树

```
当前任务是什么？
    │
    ├─ "Tauri 桌面端 E2E 测试 / 自动化测试 / CGEvent 注入 / 截图验证"
    │   └─ tauri-e2e — 全自动 CGEvent + VL 模型测试闭环
    │
    ├─ "生成/修复 app icon / Dock 图标" → openzen-icon
    │
    ├─ "Tauri 桌面端 Bug"（渲染/状态/流式/SSE/前端） → openzen-tauri-debug
    │   └─ 阅读其中的 4 时间态、13 机制、门控变量后再动手
    │
    ├─ "代码审查 / 想全面检查代码质量" → code-review
    │   ├─ 六位专家独立并行审查（安全/性能/架构/可维护/合规/边界）
    │   ├─ 首席汇总 → 修复闭环 → 逐一验证
    │   └─ 建议每个功能实现后至少跑一轮
    │
    ├─ "快速审查当前 diff 找 bug / 查正确性" → verify-code-review
    │   ├─ 4 个并行 agent：Rust 正确性 / Svelte5 类型 / 约定合规 / 跨 crate
    │   ├─ 置信度评分 0-100，只保留 ≥50
    │   └─ 与 code-review 互补：verify-code-review 快准狠，code-review 全面深入
    │
    ├─ "提交前清理代码 / 去冗余 / 优化性能" → verify-simplify
    │   ├─ 3 个并行 agent：Reuse / Quality / Efficiency
    │   ├─ 自动修复 + 去伪阳性 + LSP 验证
    │   └─ 不改行为，只改结构
    │
    ├─ "验证改动 / 编译+测试+lint / 确认行为" → verify-check
    │   ├─ L0: cargo check → L1: cargo test → L2: cargo clippy
    │   ├─ L3: Tauri E2E + VL 截图验证（可选）
    │   └─ 支持 --quick / --full / --e2e-only / --tauri
    │
    ├─ "前端 UI 变更 / 检查设计规范" → verify-design
    │   ├─ 十项逐条检查：颜色/字体/按钮/卡片/侧边栏/布局/深度/响应式/动画
    │   ├─ 对照 frontends/DESIGN.md（Song Celadon 宋韵天青）
    │   └─ Critical/Major/Minor/Nit 分级，--fix 自动修
    │
    ├─ "任意 Bug，尤其是没有明确正确标准的" → openzen-debug-loop
    │   ├─ 特征提取 → 正交侦查 → 收敛 → 契约验证 → 三层闭环
    │   ├─ 修复后必须跑 L1/L2/L3
    │   └─ 如果涉及 Tauri 桌面端，可组合 openzen-tauri-debug 的上下文知识
    │
    └─ "通用开发任务"（写功能、重构、添加特性）→ 不需要加载 OpenZen 子 skill，直接用默认能力
```

---

## 三、组合使用规则

某些复杂场景需要组合多个 skill。规则如下：

### 3.0 验证闭环链（⭐ 推荐开发工作流）

> 对应 Anthropic Claude Code 团队日用的验证链。
> 实现功能后按序运行四个 verify skill：

```
写完代码
    │
    ├─ 1. /verify-code-review  → 4 agent 找 bug、类型错误、约定违反
    │      PASS → 继续          FAIL → 修复 → 重跑
    │
    ├─ 2. /verify-simplify     → 3 agent 清冗余、提质量、优性能
    │      自动修复（review diff 后确认）
    │
    ├─ 3. /verify-check        → 编译 → 测试 → Clippy → (E2E)
    │      PASS → 继续          FAIL → 定位 → 修复 → 回到 1
    │
    └─ 4. /verify-design       → 检查 UI 合规（仅前端变更时）
           PASS → ✅ 可提交    FAIL → 修复 → 重跑
```

**快捷模式（日常提交前）：**
```
/verify-check --quick && /verify-simplify
```

**完整模式（PR 前）：**
```
/verify-code-review && /verify-simplify && /verify-check --full
```

**有前端变更时追加：**
```
... && /verify-design
```

### 3.1 Tauri 桌面端 Bug + 需要系统性 Debug

> 先加载 `openzen-tauri-debug` 理解上下文（4 时间态、数据流、门控变量），
> 再加载 `openzen-debug-loop` 执行正交诊断流程。

```
workflow:
  1. skill(name="openzen-tauri-debug")  → 理解 Tauri 端渲染/SSE 机制
  2. skill(name="openzen-debug-loop")   → Phase 0~6 执行
  3. 修复后，用 openzen-tauri-debug 验证修复是否正确
```

### 3.2 生成图标后需要验证

> 加载 `openzen-icon` 生成图标，然后用 `cargo tauri build --debug` 构建验证。

### 3.3 Bug 涉及多个模块（前端 + Rust 后端）

> 先加载 `openzen-debug-loop` 做正交诊断（维度可以跨语言），
> 各维度独立调查，收敛后修改。

### 3.4 代码修改后需要验证 Tauri 桌面端行为

> 先加载 `tauri-e2e` 获取 CGEvent 注入 + VL 模型验证能力，
> 然后执行编译 → 测试 → 注入消息 → 截图验证 → 判定 PASS/FAIL 的完整闭环。
> 如果验证 FAIL，再加载 `openzen-tauri-debug` 或 `openzen-debug-loop` 定位根因。

```
workflow:
  1. cargo check → cargo test
  2. skill(name="tauri-e2e") → tauri_send_message("test input")
  3. VL 模型验证截图 → PASS 继续 / FAIL 进入 debug
  4. FAIL 时: skill(name="openzen-tauri-debug") → 定位根因 → 修复
  5. 返回步骤 1 重新验证
```

---

## 四、项目关键路径速查

### 4.1 目录结构

```
openzen/
├── frontends/          # Svelte 5 前端 (Vite)
│   └── src/lib/
│       ├── stores/     # chat.ts (ChatState), sse.ts (SSE监听)
│       ├── components/ # ChatMessage.svelte, ToolCallCard.svelte 等
│       └── utils/      # ticker.svelte.ts (全局计时器)
├── src-tauri/          # Tauri v2 (Rust 后端)
│   ├── src/lib.rs      # Tauri 命令: send_message, ask_user_response 等
│   └── icons/          # 应用图标
├── crates/             # Rust crate 工作空间
│   └── ga-core/        # agent loop, SSE 事件流
└── scripts/e2e/        # E2E 自动化脚本
```

### 4.2 日志位置

| 日志 | 路径 |
|------|------|
| Tauri IPC log | `~/.openzen/logs/openzen-tauri.log` |
| Vite dev log | `/tmp/vite-dev.log` |
| Backend openzen serve | `/tmp/openzen-server.log` |
| 前端构建 | `frontends/` 下 `npm run dev` 输出 |

### 4.3 重启命令

```bash
kill $(pgrep openzen-tauri)
cd /Users/macstu/Documents/apps/openzen/frontends && npm run dev &
cd /Users/macstu/Documents/apps/openzen && cargo tauri dev
```

### 4.4 E2E 坐标参考（1920×1080，Tauri 窗口右半侧）

| 元素 | 坐标 | 点击方式 |
|------|------|---------|
| Sidebar "+ New Chat" | (1370, 204) | `cliclick c:1370 204` |
| Chat Send（纸飞机） | (1872, 887) | **必须** `/tmp/cgclick.py 1872 887 100` |
| AskUserDialog "Send response" | (1830, 620) | **必须** `/tmp/cgclick.py 1830 620 100` |
| Tauri "Stop" | (1835, 910) | — |

---

## 五、通用约束

所有 OpenZen skill 共享以下约束。无论加载哪个子 skill，**都必须遵守**：

- ❌ 禁止在 `$props()` 中对不需要 `bind:` 的 prop 使用 `$bindable`
- ❌ 禁止对纯内部 UI 状态（如 `collapsed`）使用 `$bindable` 替代 `$state`
- ❌ 禁止使用 `@ts-ignore` / `as any` 抑制类型错误
- ❌ 禁止在没理解全套数据链路前修改 `chat.ts` 或 `ChatMessage.svelte`
- ❌ 禁止修改 `isLive` 的条件 4 而不考虑跨轮内容泄漏
- ❌ 禁止重构/优化与 Bug 无关的代码
- ✅ 修改前先读 AGENTS.md（本目录）和对应 skill
- ✅ 修改后跑 `lsp_diagnostics` 确保类型无误
