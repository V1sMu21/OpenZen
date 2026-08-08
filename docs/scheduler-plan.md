# OpenZen 心跳 & 定时任务方案

> 状态：设计完成，待实施
>
> 关联文档：[安全方案](./security-plan.md)

---

## 1. 当前状态

| 机制 | 存在？ | 说明 |
|------|--------|------|
| 通用调度器 | ❌ | Cargo.toml 无任何调度相关依赖 |
| 后台定时任务 | ❌ | agent_loop 是请求驱动的 |
| 组件间心跳 | ❌ | 无 WebSocket ping/pong, 无 keepalive |
| 任务队列 | ❌ | 无延迟任务、重试队列、定时触发 |

**唯一存在**：`src/daemon.rs` 中 daemon 模式每 10 秒 health check 子进程。

---

## 2. 设计原则

- **零外部依赖**：使用 `tokio::time::interval`，不引入 cron/schedule crate
- **轻量**：适合个人助手场景，不做复杂任务队列
- **可扩展**：基于 trait 设计，方便增加新任务

---

## 3. 架构

### 新 crate：`crates/ga-scheduler/`

```
crates/ga-scheduler/
├── Cargo.toml
└── src/
    ├── lib.rs       # Scheduler + ScheduledTask trait
    ├── interval.rs  # tokio::time::interval 驱动
    └── tasks/
        ├── mod.rs
        ├── session_cleanup.rs  # 清理过期 session
        ├── knowledge_scan.rs   # 定时知识过期扫描
        ├── trust_decay.rs      # 渐进信任条目过期检查
        └── config_watch.rs     # 配置变更检测（可选）
```

### 核心 trait

```rust
pub trait ScheduledTask: Send + Sync {
    fn name(&self) -> &str;
    fn interval(&self) -> Duration;
    async fn execute(&self, ctx: &TaskContext) -> Result<(), TaskError>;
}
```

### 调度器实现

```rust
pub struct Scheduler {
    tasks: Vec<Box<dyn ScheduledTask>>,
}

impl Scheduler {
    pub fn run(self, shutdown: Arc<AtomicBool>) {
        tokio::spawn(async move {
            let mut handles = Vec::new();
            for task in self.tasks {
                let interval = task.interval();
                let ctx = TaskContext::default();
                handles.push(tokio::spawn(async move {
                    let mut timer = tokio::time::interval(interval);
                    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        timer.tick().await;
                        if shutdown.load(Ordering::Relaxed) { break; }
                        match task.execute(&ctx).await {
                            Ok(()) => { /* silent */ }
                            Err(e) => tracing::warn!("[scheduler] {} failed: {}", task.name(), e),
                        }
                    }
                }));
            }
            // Wait for shutdown
        });
    }
}
```

---

## 4. 内置任务

| 任务 | 文件 | 间隔 | 作用 |
|------|------|------|------|
| Session 清理 | `session_cleanup.rs` | 1 小时 | 删除超过 7 天未使用的 idle session，归档到 `.knowledge/sessions/` |
| Knowledge staleness scan | `knowledge_scan.rs` | 6 小时 | 扫描 skills/sops 质量分数，标记过时条目 |
| Trust entry decay | `trust_decay.rs` | 1 小时 | 检查渐进信任条目是否需要降级（30 天未触发） |
| Config hot-reload | `config_watch.rs` | 5 分钟（可选） | 检测 `mykey.toml` 修改并自动重载 |

---

## 5. 接入点

```rust
// ── Daemon 模式 ──
crates/ga-core/src/daemon.rs
fn run_daemon() {
    let scheduler = Scheduler::new()
        .with_task(SessionCleanup::new(&working_dir))
        .with_task(KnowledgeScan::new(&knowledge_dir))
        .with_task(TrustDecay::new(&trust_path));
    scheduler.run(shutdown_signal.clone());
    // 继续监控子进程...
}

// ── Tauri 模式 ──
src-tauri/src/lib.rs
fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let scheduler = Scheduler::new()
                .with_task(SessionCleanup::new(&app_data_dir));
            scheduler.run(Arc::new(AtomicBool::new(false)));
            Ok(())
        })
}

// ── CLI 模式 ──
// 不需要，单次交互无意义
```

---

## 6. 实施计划

| 阶段 | 内容 | 预估 |
|------|------|------|
| Phase 1 | `ga-scheduler` crate + trait + interval 驱动 | 1 天 |
| Phase 2 | SessionCleanup + KnowledgeScan 任务 | 1 天 |
| Phase 3 | TrustDecay 任务（依赖 ga-safety 就绪） | 0.5 天 |
| Phase 4 | Daemon + Tauri 接入 | 0.5 天 |
