---
name: verify-check
description: >
  OpenZen 项目端到端验证（AI 助手用）。四级验证管道：
  cargo check → cargo test → cargo clippy → Tauri E2E+VL。
  不依赖假设，确认代码在真实环境中正确。
  对应 Claude Code /verify——构建、运行、观察。
argument-hint: "[--quick] [--full] [--e2e-only] [--tauri]"
user-invocable: true
disable-model-invocation: false
allowed-tools: [Read, Bash, Grep, Glob, Task]
---

# verify-check — End-to-End Verification for OpenZen

构建 → 测试 → Lint → E2E。每一级独立运行，失败不阻止后续级别。

---

## 验证层级

| Level | 命令 | 时间 | 说明 |
|-------|------|------|------|
| L0 | `cargo check --workspace` | 30s | 编译检查 |
| L1 | `cargo test --workspace` | 60s | 单元测试 |
| L2 | `cargo clippy --workspace -- -D warnings` | 90s | Lint |
| L3 | Tauri E2E + VL 截图验证 | 2min | GUI 行为 |
| L4 | 日志/完整工作流检查 | 15s | 运行时 |

---

## 执行

### 模式选择

```
/verify-check           → L0 + L1 + L2（默认）
/verify-check --quick   → L0 only
/verify-check --full    → L0-L4
/verify-check --e2e-only → L3 only
/verify-check --tauri   → L0-L3（桌面端验证）
```

### Level 0-2: 自动化（全部并行执行）

```bash
# 三个命令并行运行
cargo check --workspace 2>&1
cargo test --workspace --test-threads=1 2>&1
cargo clippy --workspace -- -D warnings 2>&1
```

**判定：**
- Exit 0 → ✅ PASS
- 有错误 → ❌ FAIL，列出错误
- 预存在 warning（非本次 diff） → ⚠️ 标注但不阻止

### Level 3: Tauri E2E

仅在 macOS + Tauri 运行 + 显式要求时执行。

前置检查：
```bash
pgrep -fl openzen-tauri
test -f /tmp/cgclick.py && test -f /tmp/cgtype.py
curl -s http://127.0.0.1:8000/v1/models | grep Qwen
```

E2E 测试（加载 tauri-e2e skill 获取详细过程）：
1. `tauri_send_message("What is 2+2?")` — 注入消息
2. 等待 30s — agent 处理
3. `screencapture` — 截图
4. VL 模型验证 — "Is there a response visible? YES/NO"

截图保存到 `docs/test-screenshots/tauri/verify-*.png`

### Level 4: 日志

```bash
tail -50 ~/.openzen/logs/openzen.log
grep -i "panic\|ERROR\|fatal" ~/.openzen/logs/openzen.log | tail -20
```

---

## 输出

```
## Verification Report: [timestamp]

### L0: Compile
✅ cargo check passed (N crates)

### L1: Tests
✅ N passed, 0 failed

### L2: Clippy
✅ no warnings / ⚠️ N pre-existing warnings

### L3: E2E
⊘ Skipped / ✅ passed / ❌ FAIL: [reason]

### L4: Logs
✅ clean / ⚠️ found [details]

### Verdict: ✅ ALL PASSED / ❌ NEEDS FIX
```

---

## 约束

- ✅ 每级失败继续跑下一级
- ✅ 区分预存问题 vs 新引入问题
- ❌ 不修改代码（verify，不是 fix）
- ❌ L3 无 GUI → 跳过并提示
- L3 截图保存到 `docs/test-screenshots/tauri/`
