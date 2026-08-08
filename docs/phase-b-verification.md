# Phase B 验证计划 & 脚本

> 配套：`desktop-ui-keyboard-design.md` §八 Phase B
> 依赖：Phase A 完成（TransientsBar 已挂载，`data_compressing_context` 前端管道就绪）

---

## B-1：自动压缩通知验证

### 触发条件

Agent loop 中 `compress_messages()` 返回值 > 0 时 emit `StreamEvent::DataCompressingContext { before: usize, after: usize, saved: usize }`.

### 后端验证（Rust 单元测试）

```rust
// crates/ga-core/src/compress.rs 已有 8 个测试，追加 1 个：

#[test]
fn test_compress_messages_detects_overflow() {
    let long = "x".repeat(10_000);
    let mut msgs = vec![
        Message::user(&long),
        Message::assistant("short response"),
    ];
    let saved = compress_messages(&mut msgs, 1, &CompressionConfig::default());
    assert!(saved > 0, "should compress when content exceeds tiny budget");
}
```

运行：
```bash
cargo test -p ga-core compress  # 应全部通过
```

### 前端验证（浏览器）

前提：发送一条超长消息触发自动压缩（或在 compress.rs 中降低 budget 值便于触发）。

验证点：
- [ ] TransientsBar 顶部出现通知条："Compressing context: 124K → 18K tokens"
- [ ] 4 秒后自动消失
- [ ] 通知条不阻挡其他 UI

### agent_loop.rs emit 验证

在 `agent_loop.rs` 的压缩段（line 367-372）中确认：
- [ ] 仅当 `saved > 0` 时 emit
- [ ] emit 通过 `config.event_tx` 通道发送
- [ ] `DataCompressingContext` 携带 before/after/saved 字段

---

## B-2：`/compact` 命令验证

### 命令格式

在 ChatInput 输入 `/compact`，触发当前 session 的手动上下文压缩。

### 后端验证（API 端点）

#### HTTP endpoint（非 Tauri 模式）

```bash
# 1. 获取 session 列表
curl http://localhost:18567/api/sessions

# 2. 对目标 session 执行压缩
curl -X POST http://localhost:18567/api/sessions/<SESSION_ID>/compress

# 期望响应
{
  "session_id": "<SESSION_ID>",
  "before_chars": 85320,
  "after_chars": 21140,
  "saved_chars": 64180,
  "saved_pct": 75.2,
  "messages_removed": 3,
  "strategy": "summarized tool results + dropped 3 oldest message pairs"
}
```

#### Tauri command

在 Tauri 桌面端：前端调用 `tauriInvoke("compress_session", { id })` → 后端 `compress_session` command 执行压缩 → 返回相同结构。

### 前端验证

#### ChatInput 命令

- [ ] 在 ChatInput 输入 `/compact` 后回车 → ChatInput 清空
- [ ] 调用 `compressSession(currentId)` 
- [ ] 在聊天中显示一条 assistant 消息报告结果：
  ```
  ⚡ 上下文压缩完成
  压缩前: 85,320 chars · 压缩后: 21,140 chars
  释放: 64,180 chars (75.2%)
  ```
- [ ] 在非 Tauri 模式下的 HTTP fallback 正常工作

#### 命令自动补全

- [ ] 输入 `/co` → CommandPalette 显示选项，包含 "Compact"

---

## B-3：回归检查清单

- [ ] 发送普通消息不受影响
- [ ] Tauri 的 `send_message` 正常工作（未引入编译错误）
- [ ] `cargo build` 编译通过
- [ ] `cargo check` 零错误
- [ ] `cargo test -p ga-core` 全部通过（含新增压缩测试）
- [ ] 现有 sessions API（list/create/get/delete/rename/stop）不受影响

---

## 总结

| 模块 | 验证方式 | 关键风险 |
|------|---------|---------|
| `DataCompressingContext` emit | `cargo check` + 手动 | 不影响现有 event 序列化 |
| `compress_session` Tauri cmd | `cargo build` | 类型签名需匹配 |
| `/compress` HTTP endpoint | `curl` 调用 | 需要 axum router 注册 |
| `/compact` ChatInput | 浏览器手动 | 异步调用 + 错误处理 |
