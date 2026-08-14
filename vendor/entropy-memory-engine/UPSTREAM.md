# ERME Fork 基线记录 (OpenZen vendored fork)

本目录是 [熵减记忆引擎 ERME](https://github.com/) 的 vendored 副本,作为 OpenZen 的
path dependency(`entropy_memory_engine = { path = "vendor/entropy-memory-engine" }`)。
仓库外 path 依赖在 GitHub CI 上不可用,故 fork 进本仓库(ADR-0010 决策点 1)。

## 上游基线

- 上游仓库: `~/Documents/opencode/Entropy-Reduced Memory Engine`(本地)
- 基线 commit: `74e31c8 chore: initial commit of ERME codebase`
- 拷贝日期: 2026-08-09 (OpenZen `0351122 feat(erme): add entropy-reduced memory engine and integrate`)
- fork 同步工具: `scripts/sync-erme.sh`(见下)

## 本地改动清单(相对上游,9 个文件 / 331 行补丁 `openzen-delta.patch`)

> `openzen-delta.patch` 由 `diff -ruN <upstream>/src <vendor>/src` 生成;
> 从本目录执行 `patch -p1 < openzen-delta.patch` 可重放(详见 sync-erme.sh)。

| 文件 | 改动 | 动机 |
|------|------|------|
| `memory_store.rs` | ①`MemoryStore::new` 第二参改为 `Arc<L2Engine>`;②新增 `distill_and_store()`(ConsolidationEngine 抽取事实→批量 store) | ①Orchestrator/Rambling 与 store 共享同一 L2;②会话蒸馏入口(OpenZen M4/M5) |
| `router.rs` | `MemoryRouter.l2` 改为 `Arc<L2Engine>` + `l2_arc()` 访问器 | Phase2 RamblingEngine 通过同一 Arc 读 store 记忆,否则其独立 L2 为空、内循环空转 |
| `l2/engine.rs` | `L2Engine.graph` 改为 `Arc<TimeGraph>` | RamblingEngine 与 store 共享同一时间感知图 |
| `orchestrator.rs` | `rambling` 字段与 `with_idle_cycle()` 签名改为 `Arc<RamblingEngine>` | 与 L0 ReflectionEngine 共享同一联想引擎(状态不分裂) |
| `l0/generator.rs` / `l0/reflection.rs` / `metrics.rs` | 测试内 `MemoryStore::new` 适配 Arc 签名 | 签名变更连带 |
| `l1/wal.rs` | `let _ = write()` → `std::mem::drop(write())` | clippy 修复 |
| `l2/time_graph.rs` | 测试内增加 2ms sleep | 时序稳定 |

## 升级/同步流程(`scripts/sync-erme.sh`)

```bash
# 1) 只报告:上游是否有新 commit、与本目录的差异清单
bash scripts/sync-erme.sh

# 2) 升级:用上游最新 src 覆盖本目录 src,然后重放本地改动补丁
bash scripts/sync-erme.sh --apply
```

`--apply` 步骤:
1. `rsync` 上游 `src/` → 本目录 `src/`(覆盖)
2. 从本目录执行 `patch -p1 < openzen-delta.patch`
3. 若 patch 冲突,说明上游已改动同一区域——**手工合并**,并更新本文件与补丁
4. `cargo check -p entropy_memory_engine` + `cargo test -p entropy_memory_engine --lib` 验证

> 警告:上游文件若删除或重命名,`rsync --delete` 会同步移除;本地新增文件(如本目录
> 的 UPSTREAM.md、openzen-delta.patch)放在 `src/` 之外,不受影响。
