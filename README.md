# OpenZen

> **一只会记住你的猫**
> 为 Apple Silicon 统一内存而生的完全本地自主 Agent Harness——本地推理（oMLX）+ 本地记忆（ERME）+ 器物语法桌面端。

![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange)
![Tauri](https://img.shields.io/badge/Tauri-2.x-blue)
![Platform](https://img.shields.io/badge/macOS-Apple%20Silicon-93c3d6)
![Version](https://img.shields.io/badge/version-v0.1.0-93c3d6)
![License](https://img.shields.io/badge/License-MIT-lightgrey)

**中文** · [English](README.en.md)

---

## 目录

- [为什么会有 OpenZen](#为什么会有-openzen)
- [主要特点](#主要特点)
- [运行效果](#运行效果)
- [设计哲学](#设计哲学)
- [快速开始](#快速开始)
- [与现有 Harness 的对比](#与现有-harness-的对比)
- [与现有记忆引擎的对比](#与现有记忆引擎的对比)
- [Bench：三任务三方实测](#bench三任务三方实测)
- [设计借鉴与致谢](#设计借鉴与致谢)
- [架构一瞥](#架构一瞥)
- [路线与状态](#路线与状态)
- [测试与验证](#测试与验证)
- [License](#license)

---

## 为什么会有 OpenZen

一台 Mac Studio M3 Ultra 拥有 **256GB 统一内存**——显存与内存之间的墙消失了。此前「本地跑大模型」意味着要在 8–48GB 显存里精打细算；统一内存让**模型权重、向量索引、浏览器、工具链常驻同一块内存**成为现实。

OpenZen 就是为这台机器设计的：一个**完全本地**的自主 Agent Harness——

- **本地推理**：oMLX 推理服务器（MLX 框架，OpenAI 兼容 `/v1` 接口，`127.0.0.1:8000`）会在统一内存里常驻多个大上下文模型（如 256K ctx 的 MXFP4 量化模型）；
- **本地记忆**：自研 ERME 熵减记忆引擎全程在 `~/.openzen/` 下运转，数据永不出本机；
- **本地缓存红利**：本地推理的 prompt cache 命中率在典型会话中约 **96%**，长程任务（>2 小时）稳定到 **98%**（bench task4 实测 6.3 小时、38.1M tokens，命中 98.05%）。对本地部署的模型，缓存命中率衡量的不是 token 成本——本地缓存是免费的——而是 **prefill 速度**：命中率高意味着每次请求真正需要重新 prefill 的 token 极少，从而极大加速 prefill、进而提高响应速度，本地长会话因此更加趁手。

它还是一只 **7×24 常驻的伙伴**：会话中断可以断点续跑，每次交付的成败会被记住并转化为下一次的经验——这是纯会话型 Harness 做不到的复利。

> 目前形态：macOS Apple Silicon 桌面端（Tauri）。TUI 与 WebUI 已从产品形态中移除，专注桌面体验。

---

## 主要特点

### 1. 完全本地：推理与记忆零云依赖

oMLX 推理 + ERME 记忆 + 全部数据（`workspace/`、`memory_erme/`、`harness/`、`logs/`）都在本机。可配置 25 个模型槽位（本地 oMLX 为主，可接云端 API），离线可用。

### 2. ERME 熵减记忆引擎（自研）

一套为「个人长期记忆」设计的分层引擎，哲学是**熵减**——缓存层负责热数据，向量层负责语义，持久层负责压缩沉淀：

| 层 | 检索延迟 | 关键实现 |
|---|---|---|
| **L0 灵魂层** | 纳秒级（常驻内存） | Portrait、LifeNarrative、ReflectionEngine、RamblingEngine |
| **L1 工作缓存** | 纳秒级（常驻内存） | 并发哈希 + Moka + AttentionLRU + WAL 崩溃恢复 |
| **L2 向量索引** | 微秒级（常驻内存） | HNSW（384 维）+ MLX 嵌入（无 MLX 时纯 Rust 降级） |
| **L3 持久层** | 毫秒级 | 预算控制（每日 token 限额 + 按重要性逐条驱逐） |

引擎为 Mac Studio M3 Ultra 的 256GB 统一内存**深度优化**：L0 / L1 / L2 全部内存驻留，检索分别在纳秒 / 纳秒 / 微秒级；在日均新增 256K tokens 记忆的情况下，一年的记忆量在本地存储约 **100MB**，预算控制器保证它不会无限上涨。另有冲突检测（补全 / 升华 / 推翻）、检疫区（错误猜想不污染记忆）、现实锚点。自带 221 项测试。

### 3. 记忆复利：会话型 Harness 做不到的事

- **断点续跑**：MmapWal + LoopCheckpoint，进程重启后从消息级中断处继续；checkpoint 附 git 快照（sha / branch / origin）；
- **启示账本（harness refine）**（机制借鉴自 Prime Agent）：模型把可复用教训**主动写入审计账本**（`harness_state.json`），写入需附可验证证据（evidence 强校验），支持快照回滚；每轮启动按 Jaccard 相关性自动注入 `## Persistent Harness Lessons`；
- **反思闭环**：失败与成功都沉淀进 `reflections.jsonl`，后续会话可读取。

### 4. 交付质量门（QA–QE）

把「交付质量高」从意图变成可测量、可回归的工程体系：

- **验收断言**：任务先立 `task_spec.md`，含真实可执行的 `[verify]` 命令，退出前必须通过（无 spec 时自动合成最小断言）；
- **独立评审**：干净上下文（spec + 交付物 + 回复）评审，尽量换模型 / temperature=0，去自我评审盲区；
- **Diff 自检门**：退出前对照 spec 逐文件自查 diff；
- **交付契约**：强制三段式汇报——做了什么 / 怎么验证的 / 遗留什么；
- **写后验证链**：按项目类型自动跑 `cargo check` / `tsc` / `py_compile` / `go vet` 等轻量检查；
- **Harness 教训注入**：质量失败转化为账本条目，下次自动规避。

### 5. 器物语法 UI：一件宋韵天青釉器

桌面端不是「软件面板」，而是一件**汝窑瓷器**——「雨过天青云破处」的天青美学被翻译成界面语言：

- **三色克制**：全界面只有釉白（底）、天青（唯一功能色）、墨（文字），例外仅朱砂（印章与错误）；
- **釉面即界面**：消息是釉面上的刻痕，工具调用是釉下暗纹，无卡片、无玻璃拟态；
- **文化锚点**：宋体铭文 + 楷体手迹（思考块）、印章「禅」「言」、干支纪年「丙午 制」、落款「修砚 识」；
- **复杂度契约**：一切「好看」都是 O(1) 恒定成本——纹理平铺 GPU 解码一次、时间线折叠不挂 DOM、动画只走合成层（10h soak 目标 RSS ≤ 350MB）。

### 6. 效率纪律

- 系统提示词 ~4.4KB（约 1.1k tokens），对比 Claude Code 的 20k–43k tokens 体量；
- 本地 prompt cache 典型 ~96%，长程 98%（见 Bench）；
- 会话蒸馏**全异步**（任务队列 + 租约 + 崩溃接管），不阻塞主循环；
- 并行工具执行；linkme 分布式切片实现工具零拷贝自动注册（20+ 内置工具 + MCP 桥接）；
- 前端流式渲染 O(1) 恒定成本（时间线折叠 + 虚拟滚动 + 合成层动画，长会话下 DOM 恒定）——这正是峰值 RSS 低（180–240MB）、响应快、适合本地部署模型的原因。

### 7. 轻量

| 指标 | 数值 |
|---|---|
| 单二进制 | 29MB |
| 安装包（dmg，arm64） | 18.1MB |
| 空载内存 | ~180MB（桌面端主进程实测） |
| Bench 峰值内存 | 180–240MB RSS |
| 测试规模 | workspace 600+ + ERME 221 |

### 8. 桌面端体验

Tauri（Rust + Svelte 5）：SSE 流式输出、`ask_user` 弹窗确认、侧边栏 + 右侧栏、设置面板（模型 / Skills / MCP / **灵魂状态** / token 统计）、自动更新。长任务没有进度条焦虑——时间线自动折叠成「卷」。

### 9. 阿青：灵魂的可见表达

OpenZen 的 Agent 默认名是**阿青**（用户可以随时自定义）。桌面端还有一只同名小猫（`idle / working / thinking / waiting / error / done` 六态动画原型），它的「心情」来自 ERME 灵魂层（`get_memory_status`），任务完成会有小鱼干——记忆与行为一致的「知行合一」，首先表现出形态。

> 当前阶段：六态动画原型与灵魂可见化实验，完整桌面跑动交互仍在打磨中。

### 10. 消息平台接入：微信 / 飞书 / Telegram

同一 Agent 内核可接入**微信、飞书与 Telegram** 等消息平台（`oz-platform-*` 系列 crate 桥接）——桌面上的伙伴，也守在你每天聊天的地方，7×24 常驻不止于桌面。

---

## 运行效果

一段真实的运行时画面（本地模型 **DeepSeek-V4-Flash-0731 · 本地部署**，任务：编写并运行打印前 20 个斐波那契数的脚本，全程约 2.5 分钟，压缩为 7 秒循环）：

<img src="docs/screenshots/runtime-demo.gif" width="100%">

画面里可以看到：楷体手迹的**思考块**、釉下暗纹式的**工具调用**（一次落笔失败后自动改道成功）、token **流式入釉**、待办清单 2/2 收束、📋 **交付说明**契约，以及右上角「阿青」的状态变化——任务完成后它会回到「完成」形态。

---

## 设计哲学

### 功能设计

Agent Loop 是 Rust 里长出来的状态机：`exit_reason` 显式结账（stopped / paused / llm_error / EXITED）、LLM 连续错误指数退避重试（本地推理卡死防护）、`ask_user` 等待槽、**Breaker 循环检测**（识别工具调用死循环）、checkpoint 断点续跑。「先计划再动手」：任务先立 `task_spec.md` + 清单，验收断言真实执行——**环境即真相，不靠模型自述**。

### 视觉设计

器物语法（Artifact Grammar）是设计规范，不是皮肤：三色 token、釉面三层平铺纹理、入釉动效（`cubic-bezier(.22,1,.36,1)` 350–600ms）、竖排天头、卷轴式叙事流。禁止清单同样明确：无卡片、无玻璃拟态、无整页 Canvas、无动画 layout 属性。

### 效率与轻量

token 经济学 + 恒定成本渲染。系统提示词保持 ~4.4KB；Skill/SOP 渐进披露；上下文压缩阈值与摘要等待策略；前端流式渲染 O(1)（时间线折叠 + 虚拟滚动 + 合成层动画，长会话下 DOM 恒定）——这是峰值 RSS 低、响应快、适合本地部署模型的关键；内存占用由预算控制器（记忆）与复杂度契约（UI）双重约束。

### 交付质量

质量是回路的，不是检查点：失败 → 反思日志 → 账本教训 → 下次规避。**交付质量是随使用增长的数字曲线，而非单次任务的成败**——这是 7×24 常驻伙伴独有的资产。

### 灵魂

**知行合一**（借自王阳明心学）：记忆与行为一致，反思驱动进化。L0 灵魂层持续演化（画像 → 偏好轨迹 → 空闲联想 → 检疫 → 进化），目标是「越来越懂你」。

**诚实边界**（写进设计的三盆冷水）：

1. 所谓「灵魂层」本质是状态机 + 文本生成，不是意识；
2. 「神奇化学反应」是记忆密度 × 交互次数 × 模型能力的**涌现品**，不是交付物——坚持使用三个月以上，是愿景成立的前提；
3. 价值模型永远**只建议不代替**，非常谨慎地渐进。

---

## 快速开始

> 当前发布渠道：GitHub Releases 的 dmg 一键安装，无需编译。

**系统要求**

- macOS（Apple Silicon，arm64）
- 建议内存 64GB+；256GB 统一内存（M3 Ultra）为理想设计目标
- 本地推理需要 [oMLX](https://github.com/) 服务器（见步骤 3）

**安装**

1. 从 **GitHub Releases** 下载 `OpenZen-vX.Y.Z-aarch64.dmg`（CI 为每个版本 tag 自动构建）；
2. 拖入 Applications，启动 OpenZen；
3. 安装并启动 oMLX 本地推理服务器，加载模型（推荐 256K 上下文的 MXFP4 量化模型，如 Qwen3.8-Flash-Next 或 DeepSeek-V4-Flash-0731 的 MLX 版本）；
4. 在 OpenZen **设置面板 → 模型**中选择本地 oMLX（默认 `http://127.0.0.1:8000/v1`，无需修改）；
5. 新建会话，开始对话。

**数据与隐私**：一切数据留在本机 `~/.openzen/`（`workspace/` 工作目录、`memory_erme/` 记忆库、`harness/` 教训账本、`logs/` 日志）。无需账号、无遥测。

---

## 与现有 Harness 的对比

> 聚焦 harness 工程（脚手架）维度，不评模型能力。数据采集自各家公开文档与社区实践（2026-08）。

| 维度 | **OpenZen** | ZCode | Hermes (Nous) | Claude Code |
|---|---|---|---|---|
| 定位 | 7×24 本地常驻伙伴 | Goal 模式 IDE | 极简自我改进 CLI | 行业参照系 |
| 运行模式 | **默认全本地**（oMLX + ERME），数据不出本机 | 本地 / 云均可 | 本地 / 云均可 | 云 API |
| 系统提示词 | ~4.4KB（~1.1k tokens） | — | 字节稳定（缓存神圣） | 20k–43k tokens |
| 记忆 | ERME 三层 + L0 灵魂层 | 有 | SessionDB（SQLite FTS5） | CLAUDE.md ×4 + MEMORY.md |
| 记忆复利 | 启示账本注入 + 反思 + 异步蒸馏 | 无 | 任务后自动结晶 Skill | 无（手工维护） |
| 交付质量 | 验收断言 / 独立评审 / diff 自检 / 交付契约 | submit_plan + 清单 | 无系统化 | 无显式机制 |
| 断点续跑 | MmapWal + git 快照 + 崩溃恢复 | 无 | 无 | 会话内 compact |
| 循环防护 | Breaker 循环检测 | 无 | 无 | 无 |
| 桌面端 | Tauri 器物语法 + 阿青 | IDE | — | — |
| 轻量 | 29MB 二进制 / 空载 ~180MB | IDE 级 | Python 系 | 大体积 |

一句话：ZCode 与 Hermes 同样支持本地模型、同样拥有记忆——**OpenZen 的差异在于把记忆做成会进化的复利闭环（启示账本 + 反思 + 异步蒸馏），并把交付质量变成可回归的工程体系**——这是「伙伴」与「工具」的分野。

---

## 与现有记忆引擎的对比

**OpenZen 内置 ERME（熵减记忆引擎，自研）**——三层 + 灵魂，为 M3 Ultra 统一内存深度优化：L1 工作缓存（纳秒级，WAL 崩溃恢复）→ L2 向量索引（微秒级，HNSW 语义召回，MLX 嵌入加速 / 纯 Rust 降级）→ L3 持久层（毫秒级；预算控制：每日 token 限额 + 按重要性逐条遗忘）；L0 灵魂层（画像 / 生平叙事 / 反思 / 空闲漫游 / 检疫进化）同样内存驻留、纳秒级。日均 256K tokens 增量下，年存储仅约 100MB。冲突检测三态（补全 / 升华 / 推翻），检疫区保证错误猜想不污染记忆。221 项测试，纯 Rust 无外部服务依赖。

| 其他体系 | 机制 | 差异点 |
|---|---|---|
| **Claude Code** | 四层手工 CLAUDE.md + 自动 MEMORY.md 写回 | 无语义检索；靠章节注入 |
| **Hermes SessionDB** | SQLite FTS5（含中文 trigram）+ LLM 摘要 | 全文检索无向量；无预算/遗忘策略 |
| **Gemini CLI** | `~/.gemini/GEMINI.md` save_memory 追加 | 极简但零检索 |
| **Mem0 / Zep** | 向量 + 图，Python / Go | 10–50ms；多为托管服务，数据出境 |
| **Letta (MemGPT)** | 递归摘要 + 自编辑记忆 | LongMemEval ~85%；Python 生态 |
| **Hindsight** | 4 路并行检索 + 重排序器 | LongMemEval 91.4% 最高；依赖外部图谱与重排模型 |
| **OpenClaw** | Markdown 三层文件 + LLM 心跳提炼 | 零语义检索 |
| **OpenHuman** | 本地内存存储 + TokenJuice 压缩 | 细节不公开 |
| **GenericAgent**（起源） | JSON 文件 + 技能结晶 | 无检索、无遗忘、无限增长 |

它们各有所长（中文全文检索、递归摘要、4 路检索），但**没有一套把记忆做成「灵魂」**——L0 层 + 空闲进化 + 桌面可见的猫，是 OpenZen 独有的组合。

---

## Bench：三任务三方实测

> 同一任务文案、同一本地模型后端（oMLX 中本地部署的 DeepSeek-V4-Flash-0731，MXFP4 量化版）下运行，监测脚本实时采集 tokens / 内存 / 耗时 / 交付物。表格数据均为各 agent 最后一次优化轮次的实测结果。Codex CLI 曾参与部分任务，因监测数据异常未纳入。
> 交付物截图见每格下方。

### TASK 1 · 网页小游戏《星海拾遗》

单文件 HTML/CSS/JS 游戏，需要 ComfyUI 生成 ≥6 张美术素材，裁判试玩评分。

| 指标 | **OpenZen** | ZCode | Hermes |
|---|---|---|---|
| prompt tokens | **2.67M** | 12.12M | 8.18M |
| 峰值 RSS | **192MB** | 680MB | 613MB |
| 耗时 | **43 分钟** | 83 分钟 | 81 分钟 |
| 交付物 | 12 素材 | 10 | 10 |
| 交付物截图 | <img src="docs/bench/screenshots/task1-openzen.gif" width="300"> | <img src="docs/bench/screenshots/task1-zcode.gif" width="300"> | <img src="docs/bench/screenshots/task1-hermes.gif" width="300"> |

### TASK 2 · 品牌营销网站「青岚茶事」

单页品牌站，需要 ComfyUI 生成 ≥8 张素材，裁判截图评分。

| 指标 | **OpenZen** | ZCode | Hermes |
|---|---|---|---|
| prompt tokens | **731K** | 1.07M | 721K |
| 峰值 RSS | **195MB** | 480MB | 562MB |
| 耗时 | **23 分钟** | 26 分钟 | 25 分钟 |
| 交付物 | 9 | 10 | 9 |
| 交付物截图 | <img src="docs/bench/screenshots/task2-openzen.gif" width="300"> | <img src="docs/bench/screenshots/task2-zcode.gif" width="300"> | <img src="docs/bench/screenshots/task2-hermes.gif" width="300"> |

### TASK 3 · 独立游戏行业 2026 调研报告

图文报告，需要 ComfyUI 生成 ≥4 张配图，报告体积与完整度由裁判评估。

| 指标 | **OpenZen** | ZCode | Hermes |
|---|---|---|---|
| prompt tokens | **1.33M** | 12.04M | 3.24M |
| 峰值 RSS | **212MB** | 631MB | 827MB |
| 耗时 | **22 分钟** | 124 分钟 | 52 分钟 |
| 交付物 | 7 图全齐 | 7 | 6 |
| 报告体积 | **706KB** | — | 7.16MB |
| 交付物截图 | <img src="docs/bench/screenshots/task3-openzen.gif" width="300"> | <img src="docs/bench/screenshots/task3-zcode.gif" width="300"> | <img src="docs/bench/screenshots/task3-hermes.gif" width="300"> |

**读法**：OpenZen 在三个任务里用**约 1/4–1/9 的 token 消耗、约 1/3 的内存、约为一半的耗时**交付了同等或更完整的成品。不必纠结单轮缓存命中率（90–98% 随会话结构波动，长任务更高）——**每单位工作消耗的绝对 token 数**才是效率的证据。

### 长程验证（仅 OpenZen 数据，task4 / task5）

| 任务 | 规模 | 结果 |
|---|---|---|
| TASK 4 · ICLR 论文复现（纯代码） | 6.3 小时 · 38.1M tokens · 434 轮 | 缓存命中 **98.05%** · 峰值 240MB · **零 stalled** |
| TASK 5 · 高校博士后面试 PPT | 48 分钟 · 3.06M tokens | 缓存命中 95.2% · 峰值 204MB |

---

## 设计借鉴与致谢

OpenZen 借鉴前人的优秀经验一路走来，借鉴清单（逐项明确来源）：

- **GenericAgent（起源）** — OpenZen 最初是 Python 版 GenericAgent 框架的 Rust 重写（单一静态二进制，体积与内存降低一到两个数量级）；经多轮重构（删除启发式 narration.rs 375 行、content.rs 559 行、协议适配层，新增 checkpoint / ERME / 质量门）后已与原框架判若两物，只继承了「极简自主 Agent + 技能结晶」的精神内核；
- **Claude Code** — `<system-reminder>` 动态注入、MEMORY.md 自动记忆、Agent Skills 渐进披露、四层 CLAUDE.md 层级；
- **Codex CLI** — 两阶段记忆管线、收工前 diff 自检纪律、per-env profiles 概念；
- **ZCode** — submit_plan + 待办清单双轨、verify-check 四级管道（cargo check → test → clippy → E2E）；
- **Hermes（Nous）** — prompt caching 神圣原则（系统提示词字节稳定）、任务后学习循环、中文全文检索思路；
- **Prime Agent** — harness_refine（启示账本 / 自我精化）机制；
- **Pi** — 最小 harness 哲学（只做 loop / tools / context / sessions 四件事）；
- **Gemini CLI / opencode / MiMo** — save_memory 极简记忆、tool registry 与声明式权限、规格驱动工作流。

> 取前人之土，用 Rust 作窑火，烧成一件会记住你的中国瓷器。

---

## 架构一瞥

```
┌────────────────────────────────────────────────────────────┐
│ OpenZen.app（Tauri 桌面端 · Rust + Svelte 5 · 器物语法）      │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────┐ │
│  │ oz-core      │  │ oz-tools     │  │ oz-memory + ERME  │ │
│  │ agent_loop   │  │ 20+ 内置工具  │  │ L0 灵魂层         │ │
│  │ checkpoint   │  │ oz-mcp 桥接   │  │ L1/L2/L3 记忆     │ │
│  │ 质量门 QA    │  │ oz-skill-mcp  │  │ harness 账本      │ │
│  └──────────────┘  └──────────────┘  └───────────────────┘ │
└──────────────────────────┬─────────────────────────────────┘
                           │ http://127.0.0.1:8000/v1 (OpenAI 兼容)
┌──────────────────────────▼─────────────────────────────────┐
│ oMLX 本地推理服务器（MLX · MXFP4 量化 · 256K 上下文）          │
└────────────────────────────────────────────────────────────┘
```

`oz-core` 是内核（agent loop / 检查点 / 压缩 / 质量门 / 反思）；`oz-tools` 用 linkme 分布式切片自动注册工具；`oz-memory` 接入 vendored 自研 ERME 引擎；`oz-mcp` / `oz-skill-mcp` 桥接外部工具与技能库；`oz-platform-feishu / oz-platform-telegram / oz-platform-wechat` 提供消息平台接入；`src-tauri + frontends` 是器物语法壳。全部 21 个 oz-* crate 组成一个 Rust workspace（[Cargo.toml](Cargo.toml)）。

---

## 路线与状态

**v0.1.0 已于 2026-08 发布**，此后迭代包括：ERME 全量接入（M1–M4 + P0–P4）、质量门 QA–QE 五组十二项、器物语法 UI 迁移、设置面板、harness 账本闭环。

- ✅ 已完成：流协议、桌面端、Codex 风格 harness 升级（U1–U6）、ERME 接入、质量门、器物语法迁移
- 🚧 进行中：器物语法复杂度审计（10h soak RSS ≤ 350MB）、长程 bench 验证、阿青动画完善

---

## 测试与验证

- **四级验证管道**：`cargo check` → `cargo test`（workspace 600+ 测试）→ `cargo clippy` → Tauri E2E（CGEvent 驱动真实桌面交互 + 截图验证）；
- **ERME 自带 221 项测试**；
- **发布流程**：版本号由 git-cliff 从 Conventional Commits 派生；release 脚本先过测试门禁再打 tag；GitHub Actions 自动构建 dmg 并挂到 Release。

---

## License

[MIT](LICENSE) © 2026 OpenZen contributors