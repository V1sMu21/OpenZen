# ERME Fork 基线记录 (OpenZen vendored fork)

本目录是 [熵减记忆引擎 ERME](https://github.com/) 的 vendored 副本,作为 OpenZen 的
path dependency(`entropy_memory_engine = { path = "vendor/entropy-memory-engine" }`)。
仓库外 path 依赖在 GitHub CI 上不可用,故 fork 进本仓库(ADR-0010 决策点 1)。

## 上游基线

- 上游仓库: `~/Documents/opencode/Entropy-Reduced Memory Engine`(本地)
- 基线 commit: `74e31c8 chore: initial commit of ERME codebase`
- 拷贝日期: 2026-08-09 (OpenZen `0351122 feat(erme): add entropy-reduced memory engine and integrate`)
- fork 同步工具: `scripts/sync-erme.sh`(见下)

## 本地改动清单(相对上游,10 个文件 / 888 行补丁 `openzen-delta.patch`)

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
| `l2/embedding.rs` | ①`MLXEmbedding::embed()` 失败重试路径:守卫先 drop 再重建子进程;新增 `disabled` 标志(重启仍失败则永久降级 HashEmbedding);②句向量模型改走 `mlx_embeddings`(`mlx_lm` 只认 causal LM,对 BERT 直接报 `Model type bert not supported`),带 4 行 huggingface_hub 私有模块兼容垫片,mean-pool + L2 归一化;③新增本地模型路径解析(`resolve_local_model`/`find_local_model`,覆盖 HF 缓存 `models--owner--name/snapshots/*`、未打包的 `<root>/owner/name`(oMLX 布局)、绝对路径),模型路径改由 argv 传给子进程;④子进程 stdout 由专用 `mlx-embed-reader` 线程持续排空,回复经 channel 配对,每次请求带回复期限(`OPENZEN_EMBED_TIMEOUT_MS` 覆盖,默认 45s),超时即丢弃子进程并降级;⑤回归测试 `test_failed_child_falls_back_without_deadlocking`、`test_wedged_child_times_out_instead_of_hanging` + 3 条解析测试 | **①修自锁死(2026-09-21)**:原实现持 `process` 守卫调用 `embed_with_child()`,后者再取同一把非递归 std 互斥锁 → 同线程重入自锁;`erme-l2-backfill` 线程先撞上,此后所有 ERME recall(agent 回复的必经路径)永久阻塞,表现为"发消息完全没有回复"。**②③启用真实语义召回**:上游用 `mlx_lm` 加载 `mlx-community/all-MiniLM-L6-v2-4bit` 必然失败,一直静默降级为 hash 嵌入。**④修持久子进程的管道僵死**:单条回复约 8 KB、管道容量 64 KB,严格「写一条读一条」会让子进程写满 stdout 后停在写里、不再读 stdin,父进程随即写满 stdin 阻塞——父子互堵且握着嵌入锁,recall 再次永久卡死(2026-09-21 在真机复现)。⚠️ 运行前提:需 `pip install mlx-embeddings`(py3.9 上只能装 0.0.1;0.0.2+ 依赖 mlx-vlm→新版 scipy/gradio,装不上),缺失时依次回退 `mlx_lm` → `HashEmbedding`。⚠️ 本 crate 不在根 `Cargo.toml` 的 workspace members 内,根 `cargo test` 不会执行它——需在本目录跑 `cargo test --lib l2::embedding` |

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
