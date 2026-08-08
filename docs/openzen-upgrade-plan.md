# OpenZen 升级计划 — 借鉴 Codex Harness

> 目标:吸收 OpenAI Codex CLI harness 的成熟设计,补齐 OpenZen 的 agent harness 短板。
> 依据:本地 Codex v0.147.0 实证(rollout 回放 / SQLite 结构 / 系统提示)+ OpenZen 源码分析。
> 范围:高价值 3 项 + 中价值 3 项;低价值项(子任务派生、动态延迟工具、目标状态机)明确放弃。
> 日期:2026-08-08

---

## ✅ 实现状态(2026-08-08 已完成)

| 项 | 状态 | 验证结果 |
|----|------|---------|
| U1 显式计划阶段 | ✅ 已实现 | `plan_from_todos()` 填充 CheckpointPlan;Planning 哲学进 sys_prompt;125+ oz-core 测试通过 |
| U2 MCP call_tool | ✅ 已实现 | 持久 stdio 连接 + `send_request/read_response` + `McpManager::call_tool` + `McpToolHandler` 桥接;12+2+85 测试通过 |
| U3 记忆调度层 | ✅ 已实现 | `MemoryJobScheduler`(租约/重试/崩溃接管)+ runner 后台 worker + `McpMemoryDistiller`;5 个专项测试 |
| U4 会话回放 | ✅ 已实现 | `rollout.rs`(RolloutRecorder/read_rollout)+ agent_loop turn 边界录制;roundtrip 测试通过 |
| U5 git 快照 | ✅ 已实现 | `git_snapshot()` + LoopCheckpoint 3 字段(serde default 兼容旧 checkpoint) |
| U6 提示词纪律 | ✅ 已实现 | sys_prompt 新增 计划哲学/验证哲学/效率/范围/收尾 5 节 |

**验证管道**:`cargo check --workspace` 0 错误;`cargo test --workspace` 仅 1 个**预存在**失败
(`recover_from_real_checkpoint` 依赖缺失测试数据,已用 git stash 验证与本次改动无关);
变更文件 clippy 全部干净;Oracle 代码审查发现的 memory_job 3 个 bug(崩溃接管/完成清理/耗尽无限重试)
已修复并补 2 个专项测试;verify-simplify 发现的 rollout Drop flush、MCP 注册竞态已修复。

---

## 一、差距总览(来源:Codex harness 分析)

| 优先级 | 差距 | Codex 做法 | OpenZen 现状 |
|--------|------|-----------|--------------|
| 🔴 高 | 无显式计划阶段 | `update_plan` 工具 + Planning 系统提示章节 | `CheckpointPlan` 结构存在但永远 `default()` |
| 🔴 高 | MCP call_tool 未实现 | 完整 MCP 调用链 | 只能发现工具,不能调用 |
| 🔴 高 | 无两阶段记忆管线 | `stage1_outputs` → `selected_for_phase2` 固化 | 文件追加,无后台作业(→ ERME 计划已承接) |
| 🟡 中 | 无会话回放 | rollout JSONL 完整重放 | 检查点恢复状态,无确定性重放 |
| 🟡 中 | 无 git 快照绑定 | threads 记录 git_sha/branch/origin | 无 |
| 🟡 中 | 工具纪律缺哲学引导 | Task execution 章节(验证/计划/效率准则) | AGENTS.md 有工程风格,缺验证哲学 |

> 放弃项:① 子任务派生(thread_spawn_edges)——桌面伙伴场景单线程更自然;② 动态延迟工具(defer_loading)——OpenZen 工具数 ~30 远小于 Codex 生态;③ 目标状态机(usage_limited/budget_limited)——无云端额度概念。

---

## 二、U1:显式计划阶段 🔴

### 目标

让"计划"从死代码变为 agent 循环的一等公民:turn 开始时有计划,执行中跟踪,结束时有完成状态。

### 现状

- `CheckpointPlan { completed, in_progress, pending, accumulated_context, artifacts }` 存在于 `checkpoint.rs:212`,但 agent_loop 总是用 `CheckpointPlan::default()`
- `todowrite/todoupdate` 工具已存在,但无计划哲学引导(何时用/何时不用/质量标准)

### 改动设计

**1. 填充 CheckpointPlan**(`crates/oz-core/src/agent_loop.rs`)
- turn 1 的 LLM 响应解析后,若包含工具调用:把当前 todo 快照写入 `checkpoint.plan`
- 每 turn 结束:用最新 todo 状态更新 `checkpoint.plan.completed/in_progress/pending`
- 效果:恢复会话时,计划随 checkpoint 一起还原(当前只还原 messages/todos)

**2. Planning 哲学进系统提示**(`assets/sys_prompt.txt` 或 AGENTS.md)
- 何时用计划:非平凡 / 多阶段 / 有歧义 / 用户要求多个任务
- 何时不用:简单单步任务(避免填充式计划)
- 高质量 vs 低质量计划示例(直接采用 Codex 的 3 对示例模式)
- 计划更新纪律:mark completed 再进下一步;中途变更计划要说明理由

**3. 计划事件进 UI**
- `StreamEvent` 增加 `PlanUpdate { items, current, total }`(或复用 `DataTodoUpdate`)
- 前端 TodoProgress 已存在,直接绑定

### 验证

- 复杂任务(≥5 步):计划在 UI 可见、随 checkpoint 恢复
- 简单任务:无计划噪声(不产生填充式计划)
- 回归:现有 todo 工具行为不变

---

## 三、U2:MCP call_tool 补全 🔴

### 现状

`crates/oz-mcp/src/client.rs`:`call_tool()` **未实现**(返回 "requires persistent bidirectional transport" 错误)。工具能通过 `tools/list` 发现,但不能被 agent 调用。

### 改动设计

**1. 实现 call_tool**
- 补齐 MCP JSON-RPC 调用:`tools/call` 请求 + 响应解析
- 处理 `CallToolResult { content, isError }` → 转换为 `ToolOutput`
- 工具输出格式:MCP 内容块(text/image/resource)→ OpenZen 的 `ToolOutput` 文本

**2. 持久化连接**
- 当前 `McpClient` 是 stdio 一次性握手——需要保持子进程存活,维护 JSON-RPC 会话
- 参照 Codex:每个 MCP server 一个常驻子进程,连接池管理

**3. 动态注册到 ToolRegistry**
- `mcp_bridge.rs` 已有 `register_with_name()`——补全 call 后即可全链路工作
- 错误处理:server 崩溃 → 工具标记不可用 → agent 收到清晰错误

### 验证

- 配置一个真实 MCP server(如 exa),agent 能通过工具调用搜索
- server 中途崩溃 → 优雅降级,不卡死 agent

---

## 四、U3:两阶段记忆调度层 🔴

> 已由 [erme-memory-integration-plan.md](erme-memory-integration-plan.md) Phase M5 承接,此处为独立于 ERME 的通用设计。

### 目标

会话结束后的知识蒸馏异步化:后台 job 队列 + 租约 + 重试,不阻塞 agent 主循环。

### 设计(Codex `memories_1.sqlite` 实证)

```
jobs(kind, job_key, status, worker_id, lease_until, retry_at, retry_remaining, last_error)
stage1_outputs(thread_id, raw_memory, rollout_summary, selected_for_phase2)
```

### 落地

- `crates/oz-core/src/memory_job.rs`(新增):`MemoryJob` + `MemoryJobScheduler`
- 入队点:`agent_loop.rs:2092` 结晶钩子 → 只入队,立即返回
- worker:后台 tokio task,轮询队列,带租约防双 worker
- 两阶段:stage1 提取原始记忆+摘要 → 标记 → stage2 固化(ERME store / SkillMcpStore)

### 验证

- 会话结束立即返回,蒸馏在后台完成
- 杀进程重启,未完成 job 恢复

---

## 五、U4:会话回放(rollout)🟡

### 现状

`checkpoints/loop_{session}_{turn}.json` 保存完整状态,但无事件流、无确定性重放。

### 改动设计

**1. JSONL 事件流**
- 新增 `sessions/{date}/{rollout-{ts}-{id}}.jsonl`,每行一个事件
- 事件 = 现有 `StreamEvent` + 关键决策点(LLM 请求/响应摘要、工具调用、压缩触发)
- 与 Codex rollout 同构:`session_meta → world_state → turn_context → task_* → token_count`

**2. 重放能力**
- 回放器:按事件流重建会话(测试/调试用)
- 测试价值:E2E 测试可用录制回放替代真实 LLM 调用(确定性)

**3. world_state 快照**
- 每会话记录:git_sha/branch、cwd、sandbox/approval 配置、模型、时间

### 验证

- 录制一次会话 → 重放与原始一致
- 测试套件用回放替代 mock LLM(与现有 MockLlm 互补)

---

## 六、U5:git 快照绑定 🟡

### 现状

checkpoint 无 git 信息——恢复会话时不知道代码在什么状态。

### 改动设计

- `LoopCheckpoint` 增加 `git_sha: Option<String>`, `git_branch: Option<String>`, `git_origin_url: Option<String>`
- 保存时机:会话开始 + 每次 checkpoint
- 恢复时显示:"此会话基于 commit abc123(branch main)执行"
- 调试价值:回放/恢复时能检出对应 commit 复现

### 验证

- checkpoint 文件含 git 字段
- 恢复会话 UI 显示 git 快照信息

---

## 七、U6:工具纪律哲学进系统提示 🟡

### 现状

`assets/sys_prompt.txt`(2.7KB)+ `AGENTS.md` 已有工程风格,但缺 Codex 的验证/效率哲学。

### 改动设计

在 `sys_prompt.txt` 补充(借鉴 Codex Task execution 章节):

1. **验证哲学**:从最具体的测试开始(你改的代码)→ 逐步到宽泛(构建/集成);先改后验,不空读文件
2. **效率纪律**:不要重读刚写过的文件(工具调用失败会明确报错);搜索用 rg;不用脚本输出大文件
3. **范围纪律**:修复根因而非表面补丁;不修无关 bug;不提交 git 除非要求;不加无关注释
4. **收尾纪律**:问题完全解决才结束 turn;不猜测答案

> 与 AGENTS.md 的关系:sys_prompt 是通用纪律(所有会话),AGENTS.md 是项目特定(工程风格)。两者互补。

### 验证

- 观察 token 用量下降(减少无谓重读)
- 观察验证行为改善(agent 主动跑针对性测试)

---

## 八、实施顺序与依赖

```
U2 (MCP) ──────┐
U1 (计划) ──────┤  相互独立,可并行
U6 (提示词) ────┘
U4 (回放) ── 依赖 U1 的事件流扩展(可先做基础设施)
U5 (git) ── 独立,小改动
U3 (记忆调度) ── 独立,与 ERME 计划 M5 同步推进
```

| 顺序 | 项 | 理由 | 状态 |
|------|-----|------|------|
| 1 | U2 MCP call_tool | 明确 bug 级缺口,修复收益最大 | ✅ 完成 |
| 2 | U1 显式计划 | 最大架构差距,提升 agent 可理解性 | ✅ 完成 |
| 3 | U6 提示词纪律 | 零代码改动,纯文本,立竿见影 | ✅ 完成 |
| 4 | U5 git 快照 | 小改动,为 U4 打基础 | ✅ 完成 |
| 5 | U4 会话回放 | 基础设施先行(事件流),回放器后置 | ✅ 完成 |
| 6 | U3 记忆调度 | 与 ERME 接入计划 Phase M5 同步 | ✅ 完成 |

---

## 九、风险与回滚

| 项 | 风险 | 缓解 |
|----|------|------|
| U1 | 计划噪声(过度计划) | 系统提示明确"简单任务不用计划";默认关闭? 不——靠提示词约束 |
| U2 | MCP 子进程生命周期 | 连接池 + 崩溃重建;失败工具标记不可用 |
| U3 | 后台 job 竞争 | 租约(lease_until)+ 幂等(job_key) |
| U4 | 回放文件膨胀 | 只记决策点摘要,不记完整工具输出(参照 Codex 的 rollout 精简度) |
| U6 | 提示词过长 | 增量加入,控制在 1KB 内 |

全部为增量改动,可独立回滚;不改变现有工作流兼容性。
