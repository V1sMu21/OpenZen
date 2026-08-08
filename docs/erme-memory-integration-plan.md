# ERME 记忆引擎接入实施计划

> 目标:将熵减记忆引擎 (Entropy-Reduced Memory Engine) 接入 OpenZen,提供语义级记忆。
> 决策记录:[ADR-0010](adr/0010-erme-memory-engine-integration.md)
> 愿景文档:[ERME 伙伴愿景](erme-companion-vision.md)(含调整后的阶段规划)
> 环境:macOS + Rust workspace;ERME 位于 `~/Documents/opencode/Entropy-Reduced Memory Engine`
> 日期:2026-08-08

---

## 一、方案概要

**直接 path 依赖 + 配置开关 + 渐进式接入**,不做 trait 全量抽象,不做 MCP。

```
memory_backend = "file" | "erme"     (mykey.toml, 默认 "file")
```

| 路径 | file(现状,默认) | erme |
|------|----------------|------|
| 读 | `get_global_memory()` 全文注入 | `recall_by_text(query, k=5)` 语义召回注入 |
| 写 | `Crystallizer` → SkillMcpStore | 结晶钩子 + ERME distill(冲突检测自动生效) |

**核心原则**:记忆引擎是增强组件,不是关键路径。任何 ERME 错误只记日志,不阻断 Agent。

---

## 二、现状盘点(已核实)

### 2.1 读路径 — 4 个入口,同一模式

| 文件 | 行号 | 说明 |
|------|------|------|
| `src-tauri/src/runner.rs` | 306-321 | Tauri 桌面端(主入口) |
| `src/main.rs` | 541-542 | TUI/CLI |
| `crates/oz-platform/src/bridge.rs` | 125-138 | 平台桥接(飞书/微信等) |
| `crates/oz-server/src/webui/mod.rs` | 745-746 | WebUI server |

统一模式:
```rust
let memory = MemorySystem::new(working_dir, &lang);
let memory_context = memory.get_global_memory().await.unwrap_or_default();
// 追加到 system_prompt, 形如 "## Persistent Memory Context"
```

### 2.2 写路径 — 一个钩子

`crates/oz-core/src/agent_loop.rs:2090-2112`:
- 条件:`config.enable_crystallization && !tool_sequence.is_empty() && final_reason == "EXITED"`
- 现状:`Crystallizer::crystallize()` → LLM 抽取 skills/SOPs/facts → `SkillMcpStore`

### 2.3 关键事实

- **`LoopConfig` 没有 `memory_backend` 字段**(旧计划假设不存在,当前也不存在)
- **没有 MemoryBackend trait**——`MemorySystem` 是具体结构体,直接使用
- ERME 是**同步 API**,OpenZen 是 async tokio → 需要 `spawn_blocking`
- ERME **不需要 MLX**(全部有纯 Rust 降级:HashEmbedding / TruncatingCompressor / 规则抽取)
- ERME 数据目录默认在 ERME 仓库内 → 接入时必须显式指定存储路径

---

## 三、ERME API 速查(接入所需)

### 3.1 核心构造

```rust
// 1. 三层引擎
let l1 = L1Cache::builder().capacity(10_000).build();
let l2 = L2Engine::new(L2Config { hnsw: HnswConfig { dimension: 384, ..Default::default() }, ..Default::default() });
let l3 = L3Engine::new(L3Config {
    storage_path: memory_erme_dir.join("erme_memory.bin"),  // ← 必须显式指定!
    budget: BudgetConfig { daily_token_limit: 256_000, annual_storage_limit: 50_000_000, ..Default::default() },
    compression_max_chars: 400,
    ..Default::default()
});

// 2. 门面
let store = Arc::new(MemoryStore::new(l1, l2, l3, ConsolidationConfig::default()));

// 3. 可选增强(冲突检测 + 检疫)
let conflict_resolver = Arc::new(ConflictResolver::new(Arc::new(L2Engine::new(/* 同 dimension */))));
let quarantine = Arc::new(QuarantineManager::new(Arc::new(L2Engine::new(/* 同 dimension */)), QuarantineConfig::default()));
let orchestrator = Arc::new(MemoryOrchestrator::new(Arc::clone(&store), conflict_resolver, quarantine));
store.attach_orchestrator(orchestrator);
```

### 3.2 读写

```rust
// 写 (同步, 需 spawn_blocking)
store.store(MemoryInput::new(MemoryContent::Fact(Fact::new(s, p, o))).with_importance(0.7))

// 读 (同步, 需 spawn_blocking)
store.recall_by_text(query, 5)  // → Vec<(Memory, f32, LayerId)>

// 固化 (会话结束, 异步后台)
// ConsolidationEngine::extract_from_interaction(text) → Vec<(s, p, o, confidence)>
// store.consolidate()
```

---

## 四、实施步骤

### Phase M1:依赖接入(零行为变化)

1. `openzen/Cargo.toml` 增加 path dependency:
   ```toml
   entropy_memory_engine = { path = "../../opencode/Entropy-Reduced Memory Engine" }
   ```
   (不需要加入 workspace members,path dependency 即可)
2. `crates/oz-core/Cargo.toml` 和 `src-tauri/Cargo.toml` 添加依赖
3. `cargo check` + 现有测试全绿

**验证**:编译通过,无任何业务逻辑变化。

---

### Phase M2:ERME 实例生命周期(长驻)

1. `src-tauri/src/runner.rs`(或 lib.rs setup)创建一次:
   - 存储路径:`{working_dir}/memory_erme/erme_memory.bin`(与现有 `memory/` 并列)
   - 构建 `Arc<MemoryStore>` + `Arc<MemoryOrchestrator>`
   - 放入 `AppState`(Tauri `manage`),全局共享
2. **不要**在每次 `run_agent_for_session` 时重建(L2 HNSW 索引和 L3 存储是长生命周期对象)

> 注:TUI(`src/main.rs`)、bridge、webui 各自独立运行,暂不接入;先只做 Tauri 桌面端。

---

### Phase M3:读路径(配置开关分支)

1. 配置:`mykey.toml` 增加 `memory_backend = "file"`(默认)
2. `src-tauri/src/runner.rs` 306 行附近分支:
   ```rust
   let memory_context = match config.memory_backend {
       "erme" => {
           // spawn_blocking: store.recall_by_text(user_query, 5)
           // 格式化: "[Semantic Memory]" + top-k 条目
       }
       _ => memory.get_global_memory().await.unwrap_or_default(),
   };
   ```
3. 语义召回需要**当前 query**——runner 中 `user_message` 此时已解析,直接传入

**验证**:开关默认 "file" 行为不变;"erme" 下对比 token 用量(应显著低于全文注入)。

---

### Phase M4:写路径(结晶钩子旁路)

1. `agent_loop.rs:2092` 结晶钩子旁,增加 ERME distill:
   - `LoopConfig` 增加 `memory_backend: String` 字段(默认 "file")+ `erme_store: Option<Arc<MemoryStore>>`
   - 条件满足时:`spawn_blocking` 调用
     `ConsolidationEngine::extract_from_interaction(transcript)` → `store.store()` 逐条写入
2. 错误处理:`tracing::warn!`,不阻断 Agent
3. `Handler`/`LoopConfig` 传递:runner 构造 loop_config 时注入 `erme_store`

**验证**:`memory_backend = "erme"` 跑一个真实会话,确认 ERME 存储增长(`memory_erme/` 目录)。

---

### Phase M5:开关切换 + 回滚演练

1. `mykey.toml` 切到 `memory_backend = "erme"`,跑日常会话
2. 确认异常时切回 `"file"` 立即恢复原状
3. 观察点:响应质量(语义召回是否命中)、token 用量、`memory_erme/` 增长

---

### Phase M6(可选):L0 灵魂层 + 内循环

> 已提前至伙伴愿景的 **Phase 3-4**(见 [erme-companion-vision.md](erme-companion-vision.md))。
> 本阶段从"可选收尾"提升为伙伴路线的主线,完成顺序为:

1. **Phase 3 — 让它"懂你"**:启用 `PromptInjector`(自我模型前缀)+ Portrait 扩展为**用户偏好轨迹**
2. **Phase 4 — 让它"进化"**:启用 `MemoryOrchestrator::with_idle_cycle(RamblingEngine)` 空闲联想
   + QuarantineManager 验证 + RealityAnchor 锚定 + ReflectionEngine 复盘
3. 注意:这两者耗时/占资源,用 `spawn_blocking` + 频率控制
4. **Phase 5(长期)**:基于偏好轨迹的"选择建议",永远只建议不代替(详见愿景文档)

---

## 五、决策点(实现前确认)

| # | 决策 | 建议 | 理由 |
|---|------|------|------|
| 1 | ERME 数据位置 | `{working_dir}/memory_erme/` | 随项目走,与旧记忆并列,回滚零影响 |
| 2 | 热插拔粒度 | 配置开关(重启生效) | 足够;`ArcSwap` 运行中切换是过度设计 |
| 3 | 读路径模式 | 先双轨(保留文件记忆 + 加语义段)→ 稳定后纯语义 | 渐进,可对比 |
| 4 | 首批范围 | L1-L3 核心(store/recall/conflict) | 按伙伴愿景,Phase 3-4(L0/内循环)紧随其后,不拖到二期 |
| 5 | 接入端 | 先 Tauri 桌面端 | 主入口;TUI/bridge/webui 后续跟进 |

---

## 六、风险与缓解

| 风险 | 缓解 |
|------|------|
| ERME 是同步 API 阻塞 tokio | 全部 `spawn_blocking`;写入失败只 warn |
| path dependency 跨仓库耦合 | ERME 是只读依赖;如需修改先 fork 到 openzen 内 |
| 语义召回质量不如预期 | 降级回 "file";调 k / 换 embedding(MLX 可用时自动启用) |
| 存储增长失控 | ERME 内置 BudgetController + forgetting;监控 `memory_erme/` 大小 |
| 旧计划假设过时 | 本计划基于 8月8日代码现状重写(见 ADR-0010) |

---

## 七、文件变更清单

| 文件 | Phase | 变更 |
|------|-------|------|
| `openzen/Cargo.toml` | M1 | + path dependency |
| `crates/oz-core/Cargo.toml` | M1 | + 依赖 |
| `src-tauri/Cargo.toml` | M1 | + 依赖 |
| `src-tauri/src/runner.rs` | M2/M3 | ERME 初始化 + 读路径分支 |
| `crates/oz-core/src/handler.rs` | M4 | `LoopConfig` + `memory_backend` / `erme_store` 字段 |
| `crates/oz-core/src/agent_loop.rs` | M4 | 结晶钩子旁 ERME distill |
| `~/.openzen/mykey.toml` | M3 | + `memory_backend = "file"`(运行时配置,非代码) |
