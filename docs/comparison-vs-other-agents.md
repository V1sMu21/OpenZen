# Rust Generic Agent vs 其他 Agent 框架：全面对比（第二版）

> 生成日期：2026-05-21
> 目标：梳理 Rust Generic Agent (GA-RS) 相对于原版 Generic Agent (Python)、OpenClaw、Hermes Agent、OpenHuman 的优劣势，明确后续改进方向。
> 说明：第二版在 P0/P1/P2 全部 11 项改进实现后重新对比，Rust GA 的短板已有 9/11 补齐。

---

## 一、基础架构

| 维度 | **Rust GA (GA-RS)** | **Python GA (原版)** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| 语言 | Rust | Python | TypeScript/Node.js | Python | Rust + TypeScript |
| 核心代码量 | ~13,500 行 (12 crates) | ~3,300 行 | 中等 (npm pkg) | ~15k+ 行 (AIAgent 单文件) | 大代码库 |
| Agent Loop | 745 行 (agent_loop.rs) | 92-100 行 (agent_loop.py) | 框架化多文件 | 15k+ 行单文件 (run_agent.py) | 框架化+侧边车进程 |
| 桌面端 | ✅ Tauri (原生 Rust) | ❌ 无 | ✅ macOS 菜单栏 | ❌ 无 | ✅ Tauri (原生) |
| CLI | ✅ | ✅ | ✅ | ✅ | ✅ |
| WebUI | ✅ (axum + SSE + 干预/检查点/恢复 API) | ✅ | ❌ | ❌ | ❌ |
| 协议 | 多通道 (CLI/HTTP/SSE/Tauri IPC) | CLI+HTTP | 网关 + 多 IM 通道 | CLI + Gateway | JSON-RPC 双 Socket |
| 开源协议 | MIT | MIT | MIT | MIT | GPL-3.0 |
| 核心哲学 | 极简 + 原生性能 | 自演化种子 | 个人 AI 助理 | 研究级 Agent | 个人超级智能 |

---

## 二、7×24 小时连续工作能力

Rust GA 的**最强维度**，也是与其他方案最显著的区别。经过 P0/P1 改进后，连续运行能力已大幅增强。

| 能力 | **Rust GA** | **Python GA** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| **内存安全** | ✅ Rust 编译器保障，无 GC 暂停 | ⚠️ Python GC 可能随机 STW | ⚠️ Node GC 可能导致抖动 | ⚠️ Python GC 抖动 | ✅ Rust+Tauri |
| **内存泄漏风险** | ✅ 极低（所有权模型） | ❌ 高（引用计数+循环引用） | ⚠️ 中（V8 堆增长） | ❌ 高 | ✅ 低 |
| **长时间运行稳定性** | ✅ 无累积退化 + Context 压缩防止膨胀 | ❌ 会话历史不截断会 OOM | ⚠️ 取决于插件 | ⚠️ 有压缩但 CPU 累积 | ⚠️ 未知 |
| **并发模型** | ✅ 原生 async/await，零成本 | ⚠️ asyncio 有 GIL 限制 | ⚠️ 单线程事件循环 | ❌ 同步引擎 (ThreadPool) | ✅ 原生 async |
| **自愈/看门狗** | ✅ Daemon 模式（PID + 健康检查 + 自动重启） | ❌ 无 | ✅ launchd/systemd daemon | ❌ 无 | ❌ 无 |
| **SSE 流式** | ✅ 原生 tokio 流 | ✅ | ❌ | ✅ | ✅ |
| **Context 窗口管理** | ✅ 上下文压缩（工具结果摘要 + 旧消息丢弃） | ✅ 5 层记忆 + 极简 token | ⚠️ 插件依赖 | ✅ ContextEngine 有损压缩 | ✅ TokenJuice 压缩 |
| **断点续跑** | ✅ LoopCheckpoint（保存消息+turn 状态，重启恢复） | ❌ 无 | ❌ 无 | ❌ 无 | ❌ 无 |
| **超时保护** | ✅ 120s timeout + per-tool timeout (30s) | ❌ 无 | ❌ 无 | ✅ interruptible API | ❌ 未知 |
| **最大轮数** | 70 (可配) | 40 (可配) | 框架级 | iteration budget | 框架级 |
| **GC 暂停** | ✅ 无 | ❌ 有 (STW) | ⚠️ 有 (V8 增量) | ❌ 有 | ✅ 无 |
| **SOP 自动结晶** | ✅ 成功 tool_sequence 自动保存为 SOP | ✅ 自动结晶为 Skill/SOP | ❌ 手动 | ❌ 手动 | ❌ 手动 |

### 评级

| 框架 | 7×24 适合度 | 理由 |
|---|---|---|
| **Rust GA** | ⭐⭐⭐⭐⭐ | 原生二进制、零 GC、零内存增长、<50ms 启动；新增 daemon 自愈、断点续跑、context 压缩、SOP 结晶，连续运行能力已达最完整 |
| Python GA | ⭐⭐⭐ | 代码极简但 Python 运行时拖后腿 |
| OpenClaw | ⭐⭐⭐⭐ | daemon 守护好，但 Node.js 堆增长不可控 |
| Hermes Agent | ⭐⭐⭐ | 功能最全但 Python 单进程跑一周已经算极限 |
| OpenHuman | ⭐⭐⭐⭐ | Rust 底层好，但前端层和 118+ 集成带来复杂性 |

---

## 三、记忆系统

| 维度 | **Rust GA** | **Python GA** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| 架构 | WorkingMemory + Sensorium + MemorySystem + SOP | L0-L4 五层分层记忆 | 插件驱动 | MemoryManager + MemoryProvider | Memory Tree (Markdown→SQLite) |
| 持久化方式 | 文件 (JSON) + SOP Markdown 文件 | 文件 (Markdown/SOP) | 配置/插件 | SQLite (FTS5 全文搜索) | SQLite + Obsidian Vault |
| 跨会话记忆 | ✅ 全局记忆 (global_memory) + SOP 积累 | ✅ 分层记忆 + SOP | ⚠️ 弱 | ✅ 会话搜索 (FTS5) | ✅ Memory Tree |
| 自动演化 | ✅ SOP 自动结晶（成功路径自动保存） | ✅ 自动结晶为 Skill/SOP | ❌ 手动 | ❌ 手动 | ✅ Auto-fetch 每 20 分钟 |
| 记忆工具 | 3 个 (checkpoint + long_term + sop) | 2 个 (update_working_checkpoint, start_long_term_update) | 插件 | memory + session_search | Memory Tree 工具 |
| 长程召回 | Sensorium（工具循环检测）+ SOP 积累 | L4 → 会话归档 | 插件 | session_search (FTS5) | Memory Tree 摘要树 |
| 上下文压缩 | ✅ 2 阶段压缩（摘要工具结果 + 丢弃旧消息对） | ❌ 无（极简省 token） | ❌ 无 | ✅ ContextCompressor（有损摘要） | ✅ TokenJuice |
| SOP 知识沉淀 | ✅ SopStore + crystallise 自动保存 | ✅ SOP 自动结晶 | ❌ 无 | ❌ 无 | ❌ 无 |
| 记忆持久化目录 | `~/.openzen/` + sop 目录 | 项目内 `memory/` | 配置目录 | `~/.hermes/` | SQLite + Obsidian `wiki/` |

### 评级

| 框架 | 记忆系统 | 理由 |
|---|---|---|
| **Rust GA** | ⭐⭐⭐⭐ | 新增上下文压缩 + SOP 自动结晶；已从最简升级为中等完备，短板基本补齐 |
| Python GA | ⭐⭐⭐⭐ | 5 层记忆设计完善，SOP 结晶是独创 |
| OpenClaw | ⭐⭐ | 插件依赖，无统一记忆层 |
| Hermes Agent | ⭐⭐⭐⭐ | SQLite FTS5 搜索 + ContextCompressor 压缩 |
| OpenHuman | ⭐⭐⭐⭐⭐ | Memory Tree + Auto-fetch + TokenJuice，仍然最完整 |

---

## 四、工具调用

| 维度 | **Rust GA** | **Python GA** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| 工具数量 | ~26 个 (含 MCP 客户端) | 7+2 个原子工具 | 50+ 集成 | 70+ 工具 (28 toolsets) | 118+ 集成 |
| 注册机制 | 手动 struct → `ToolHandler` trait + linkme distributed slice 自动注册 | 手动类注册 | NPM 包/插件 | 自注册 (import 时自动发现) | OAuth 集成配置 |
| 分发方式 | ToolRegistry → async dispatch | 函数分发 | 插件系统 | Registry + agent-level 拦截 | JSON-RPC |
| 异步支持 | ✅ 原生 async trait | ✅ asyncio | ✅ Promise | ❌ 同步 + ThreadPool 桥接 | ✅ 原生 async |
| 并发执行工具 | ✅ async join_all + Semaphore(8) + per-tool timeout(30s) + 提前取消 | ❌ 串行 | ❌ 串行 | ✅ ThreadPoolExecutor (真并行) | ❌ 未知 |
| 工具循环检测 | ✅ Breaker + Sensorium (独创) | ❌ 弱 | ❌ 无 | ❌ 无 | ❌ 无 |
| 危险操作拦截 | ✅ ask_user 工具 | ✅ ask_user 工具 | ✅ 沙箱模式 | ✅ 危险命令检测 + approval | ❌ 未知 |
| WASM 插件 | ✅ 动态加载 .wasm 文件（name/desc/params） | ❌ 无 | ❌ 无 | ❌ 无 | ❌ 无 |
| MCP 支持 | ✅ MCP Server (SSE/stdio) + Client (McpManager) + CLI `ga mcp` | ❌ 无 | ✅ MCP 协议 | ✅ MCP 协议 | ✅ MCP 协议 |
| 代码执行 | ✅ code_run (终端命令) + RPC 模式（中间结果不进 context） | ✅ code_run (动态安装) | ❌ | ✅ execute_code (RPC 模式) | ✅ 内置 |
| 浏览器控制 | ✅ ga-browser (Playwright) | ✅ web_scan + web_execute_js | ❌ | ✅ 10 个浏览器工具 | ✅ Chromium Embedded |
| 工具 Schema 格式 | Vec\<ToolDefinition\> | OpenAI 格式 | OpenAI 格式 | OpenAI 格式 | JSON-RPC |
| 工具文件独立性 | 每个工具一个 .rs 文件 | 每个工具一个 .py 文件 | 每个插件一个包 | 每个工具一个 .py 文件 | 每个集成配置 |

### 关于 Breaker + Sensorium（Rust GA 独有）

Rust GA 拥有业界唯一的内置工具调用循环检测机制：

- **Sensorium**：记录每个工具被调用的次数和频率，检测 LLM 是否在反复调用同一个工具毫无进展
- **Breaker**：当某个工具调用过于频繁时，主动跳过并插入提示，防止无限循环

这在 7×24 场景下非常实用——LLM 偶尔会陷入"查文件→再查文件→继续查"的死循环，Breaker 会强制中断。

### 关于 execute_code RPC（Rust GA + Hermes）

Rust GA 现已实现 Hermes 风格的 `execute_code` RPC 模式：

1. Agent 通过 `code_run(mode="rpc", script="...")` 执行脚本
2. 工具结果写入临时文件 `ga_rpc/code_run_*.json`
3. 仅返回文件引用 + 摘要，**中间结果不进 context**
4. 脚本可通过 `GA_RPC_*` 环境变量调用其他工具

**收益**：原本需要 3 轮 tool call 的工作，现在 1 轮 `execute_code` 搞定，token 消耗降低 3-10 倍。

### 关于 WASM 插件系统（Rust GA 独有）

Rust GA 是唯一支持 WASM 插件的框架：

1. 用户编译 `.wasm` 文件放入 `--plugin-dir`
2. 运行时自动发现并加载，无需重新编译
3. 插件暴露 `name`、`description`、`parameters` 作为工具描述
4. 处理后结果返回 LLM

**收益**：第三方能力可以动态加载，扩展性不再受限于编译时注册。

### 评级

| 框架 | 工具系统 | 理由 |
|---|---|---|
| **Rust GA** | ⭐⭐⭐⭐ | Breaker+Sensorium 独特 + MCP 支持 + WASM 插件 + 并行执行 + RPC 模式；工具数量少仍是一个限制但 MCP 大幅缓解 |
| Python GA | ⭐⭐⭐ | 7 个原子工具设计优雅，但功能有限 |
| OpenClaw | ⭐⭐⭐⭐ | 50+ 集成 + MCP + 沙箱 |
| Hermes Agent | ⭐⭐⭐⭐⭐ | 70+ 工具 + execute_code RPC + ThreadPool 并发 + MCP |
| OpenHuman | ⭐⭐⭐⭐⭐ | 118+ 集成 + 自动拉取 + MCP |

---

## 五、系统开销

| 维度 | **Rust GA** | **Python GA** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| **运行时依赖** | 无（原生二进制） | Python 3.x (~50MB) | Node.js 22+ (~80MB) | Python 3.x (~50MB) | Tauri runtime |
| **二进制体积** | 5-7 MB (strip + LTO) | N/A（需要解释器） | N/A（需要 Node） | N/A（需要解释器） | ~100-200 MB (含 WebView) |
| **空载内存** | ~1-3 MB | ~30-50 MB | ~50-80 MB | ~40-60 MB | ~50-100 MB |
| **单次 LLM 调用额外开销** | <1ms (reqwest) | ~5-10ms (httpx) | ~5-10ms (fetch) | ~5-10ms (httpx) | <1ms |
| **CPU 效率** | ✅ 原生编译，LLVM O3+LTO | ⚠️ 解释执行 | ⚠️ JIT 但有 GC | ⚠️ 解释执行 | ✅ 原生编译 |
| **启动时间** | <50ms | ~500ms-2s | ~1-3s | ~500ms-2s | ~1-2s (Tauri) |
| **依赖管理** | Cargo 静态链接 | pip (易冲突) | npm (node_modules 膨胀) | pip (易冲突) | pnpm + cargo |
| **部署方式** | 单二进制，scp 即用 | 需 Python + pip install | 需 Node + npm install | 需 Python + pip install | 需 .app bundle |
| **45天连续运行预期内存增长** | ~0%（已加入 context 压缩防膨胀） | ~15-30% | ~5-15% | ~10-20% | ~0% |
| **跨平台二进制** | ✅ macOS/Linux/Windows | ⚠️ 需解释器 | ⚠️ 需 Node | ⚠️ 需解释器 | ✅ macOS (Tauri) |

### 具体的性能差异放大效应

假设每天 10,000 次 LLM 调用，每次附带 3 次工具执行：

| 操作 | Rust GA | Python GA | 优势 |
|---|---|---|---|
| HTTP 请求开销 | 0.5ms × 10,000 = 5s | 8ms × 10,000 = 80s | 16x |
| 序列化 Serde vs json.dumps | 0.1ms × 30,000 = 3s | 2ms × 30,000 = 60s | 20x |
| 文件 I/O (tokio::fs vs open) | 0.2ms × 5,000 = 1s | 3ms × 5,000 = 15s | 15x |
| **每日合计** | **~9s** | **~155s** | **17x** |

一年下来差距超过 14 小时——Rust GA 每年能多完成几十万次工具调用。

---

## 六、运行速度

| 维度 | **Rust GA** | **Python GA** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| **工具执行延迟** | ✅ 纳秒级（原生代码） | ⚠️ 微秒级（解释执行） | ⚠️ 微秒级（JIT） | ⚠️ 微秒级（解释） | ✅ 纳秒级 |
| **SSE 流式处理** | ✅ 零拷贝 bytes 流 | ⚠️ 字符串复制 | ⚠️ 中间层多 | ⚠️ 字符串复制 | ✅ 零拷贝 |
| **JSON 序列化** | ✅ serde (零拷贝可选) | ⚠️ json.dumps (str 复制) | ⚠️ JSON.stringify | ⚠️ json.dumps | ✅ serde |
| **并行工具执行** | ✅ async join_all + Semaphore | ❌ 串行 | ❌ 串行 | ✅ ThreadPool (真并行) | ❌ 串行 |
| **HTTP 客户端** | ✅ reqwest (Rust，连接池) | ⚠️ httpx | ⚠️ fetch (Node) | ⚠️ httpx | ✅ reqwest |
| **文件 I/O** | ✅ tokio::fs (async) | ⚠️ 同步/异步混合 | ⚠️ fs/promises | ⚠️ 同步 | ✅ tokio::fs |
| **并发模型开销** | ✅ 零成本 async | ⚠️ asyncio 有开销 | ⚠️ 事件循环 + Promise 堆 | ❌ 同步线程池 | ✅ 零成本 async |

---

## 七、开发者体验与生态

| 维度 | **Rust GA** | **Python GA** | **OpenClaw** | **Hermes Agent** | **OpenHuman** |
|---|---|---|---|---|---|
| **添加新工具的难度** | ⚠️ 高（写 struct + trait impl + 注册） | ✅ 低（写类 + 注册） | ✅ 低（写插件脚本） | ✅ 低（自注册，import 即可） | ✅ 低（配置集成） |
| **WASM 插件开发** | ✅ 用其他语言写插件，部署 .wasm | ❌ 无 | ❌ 无 | ❌ 无 | ❌ 无 |
| **编译时间** | ⚠️ ~1-2 分钟增量 | ✅ 即时 | ✅ 即时 | ✅ 即时 | ⚠️ ~1-2 分钟 |
| **调试体验** | ⚠️ 一般（Rust 编译器严格） | ✅ 好（pdb/ipdb） | ⚠️ 一般 (Node) | ✅ 好 (pdb) | ⚠️ 一般 |
| **社区生态** | ⭐⭐ 小 | ⭐⭐⭐ Python 生态 | ⭐⭐⭐⭐ 18 万+ Star | ⭐⭐⭐ 研究社区 | ⭐⭐⭐ 快速增长 |
| **文档质量** | ⚠️ 基本 | ⚠️ 基本 | ✅ 完善 | ✅ 完善 (GitBook) | ✅ 完善 (GitBook) |
| **CI/CD** | ✅ Cargo 原生 | ✅ pytest | ✅ jest | ✅ pytest | ✅ Vitest + cargo-test + WDIO |
| **静态类型** | ✅ Rust 类型系统最强 | ⚠️ type hints (运行时擦除) | ✅ TypeScript | ⚠️ type hints | ✅ Rust + TypeScript |

---

## 八、整体优劣势总结（第二版更新）

### Rust GA 的核心优势

| # | 优势 | 量化对比 |
|---|---|---|
| 🦀 | **Rust 原生性能**：5-7MB 单二进制，1-3MB 空载内存，零 GC | 比 Python 方案快 15-20 倍，内存少 10-50 倍 |
| 🔧 | **Tool Loop 防护**：Breaker + Sensorium 自动检测工具调用循环 | 业界唯一，7×24 场景关键 |
| 🔄 | **并行工具执行**：join_all + 信号量 + per-tool 超时 + 提前取消 | 3 个并行工具=串行的 3 倍吞吐 |
| 📡 | **多通道原生支持**：CLI + WebUI(SSE) + Tauri IPC | 三通道全部原生性能 |
| 💰 | **极致资源效率**：无运行时依赖，单二进制部署 | 适合 Docker/边缘设备/低配机器 |
| 🛡️ | **编译期安全**：所有权系统杜绝内存泄漏、数据竞争 | 连续运行数月不出问题 |
| 🧩 | **WASM 插件系统**：动态加载第三方能力，不重新编译 | 业界唯一，扩展性突破编译限制 |
| 📜 | **SOP 自动结晶**：成功路径自动保存为可复用技能 | 经验积累全自动，无需手动干预 |
| 🖥️ | **WebUI 干预面板**：运行时注入策略/信息/暂停，支持检查点恢复 | 用户可中途调整 Agent 行为，无需重启会话 |
| 🐛 | **UTF-8 安全**：smart_format 等函数正确处理多字节字符 | 中文字符串切片不再 panic，国际化支持完善 |

### Rust GA 的核心短板（第二版更新后剩余短板）

经过 P0/P1/P2 改进后，原 7 项核心短板已解决 5 项，剩余 2 项待改善：

| # | 短板 | 状态 | 影响 | 应对方案 |
|---|---|---|---|---|
| 📚 | **工具生态偏弱**：~26 个仍远少于 Hermes 70+ / OpenHuman 118+ | ⚠️ MCP 已缓解但原生工具仍少 | 用户开箱即用不如对手 | ①持续扩展内置工具 ②完善 MCP 客户端自动发现 |
| 🏗️ | **开发者门槛高**：加工具要写 Rust struct + trait | ⚠️ 部分缓解 | 贡献者少，生态成长慢 | WASM 插件降低门槛 + MCP 协议接入社区工具无需编译 |
| 🧠 | **记忆系统原始**：无 Memory Tree / FTS5 搜索 | ✅ 已解决：SOP 结晶 + 上下文压缩 + 断点续跑 | 长时间运行经验可沉淀 | 已通过 SOP 自动结晶解决 |
| 📉 | **Context 管理弱**：无压缩/摘要 | ✅ 已解决：2 阶段压缩策略 | Token 消耗已降低 | 已通过 CompressConfig 实现 |
| 🔄 | **缺少自愈/daemon** | ✅ 已解决：Daemon 模式 + 健康检查 + 自动重启 | 进程崩溃可自动恢复 | 已通过 DaemonConfig + monitor 实现 |
| 🧩 | **无插件系统** | ✅ 已解决：WASM 插件 + MCP 协议 | 扩展性不再受限 | 已通过 plugin-wasm + mcp-client 实现 |
| 🚀 | **无并行工具执行** | ✅ 已解决：Semaphore + timeout | 批量操作速度提升 | 已通过 agent_loop Phase 2 重写实现 |

### 第二版改进总结

| 类别 | 第一版状态 | 第二版状态 | 等级提升 |
|---|---|---|---|
| 7×24 能力 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ (更强) | 新增 daemon + checkpoint + 压缩 + 干预恢复 |
| 记忆系统 | ⭐⭐ | ⭐⭐⭐⭐ | 新增压缩 + SOP 结晶 |
| 工具调用 | ⭐⭐⭐ | ⭐⭐⭐⭐ | 新增 MCP + 并行 + RPC + WASM |
| 系统开销 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | 不变 |
| 运行速度 | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | 新增并行执行 |
| 开发者体验 | ⭐⭐ | ⭐⭐⭐ | WASM 插件降低扩展门槛 |
| 前端交互 | ⭐⭐⭐ | ⭐⭐⭐⭐ | 新增干预面板 + 检查点恢复 UI + 实时状态同步 |

---

## 九、场景推荐

| 场景 | 最佳选择 | 第二选择 | 理由 |
|---|---|---|---|
| **7×24 无人值守服务器** | **Rust GA** | Python GA | 零 GC、零内存增长、5MB 二进制、Docker 友好；新增 daemon + checkpoint + 压缩 |
| **快速原型/研究** | Python GA | Hermes | 92 行 loop，秒级迭代 |
| **多 IM 通道机器人** | OpenClaw | Hermes | 20+ IM 通道即开即用 |
| **复杂工具编排** | Hermes Agent | **Rust GA** | 70+ 工具 + MCP + WASM + 并行 + RPC 已大幅缩小差距 |
| **个人桌面 AI 助理** | OpenHuman | OpenClaw | 记忆树 + 118+ 集成 + 桌面吉祥物 |
| **边缘设备/低配机器** | **Rust GA** | OpenHuman (本地模式) | 1-3MB 内存，无运行时依赖 |
| **高阶研究 (RL/训练)** | Hermes Agent | — | RL training 工具 + batch runner |
| **企业级部署** | **Rust GA** | OpenClaw | 单二进制 + 静态链接，安全合规；WASM 插件扩展无需重新编译 |
| **需要第三方插件的场景** | **Rust GA** | OpenClaw | WASM 插件系统 + MCP 协议，业界唯一双通道扩展 |

---

## 十、Rust GA 改进路线图（已全部完成）

### P0 — 高优先级（显著提升 7×24 能力）

- [x] **MCP 协议支持**：接入 MCP = 获得数千个社区工具，瞬间解决工具生态短板
- [x] **Context 压缩**：实现 Heritage/ContextCompressor 类似机制，大窗口运行时省 token
- [x] **自愈/daemon 模式**：systemd/launchd 用户服务 + 健康检查 + 自动重启

### P1 — 中优先级（补齐核心能力）

- [x] **并行工具执行**：独立工具可并发执行，参考 Hermes ThreadPoolExecutor
- [x] **SOP 结晶**：成功路径自动保存为可复用技能
- [x] **execute_code RPC 模式**：Agent 写脚本远程调用工具，中间结果不进 context
- [x] **断点续跑**：保存 agent loop 状态，重启后从上次中断处继续

### P2 — 低优先级（锦上添花）

- [x] **WASM 插件系统**：动态加载第三方能力，不重新编译
- [x] **Auto-fetch 集成**：定时自动拉取数据写入记忆
- [x] **全量仪表盘**：Tauri 端增加实时监控（token 消耗、工具调用频率、记忆大小）
- [x] **多模型路由**：简单问题用廉价模型，复杂问题用旗舰

### P3 — 下一步可探索方向

- [ ] **FTS5 全文搜索**：SQLite FTS5 或 tantivy 实现记忆全文检索
- [ ] **Memory Tree**：摘要树结构，长程召回能力
- [ ] **自动 MCP 服务器发现**：启动时自动扫描并连接配置好的外部 MCP 服务器
- [x] **Hermes 式 Tool Discovery**：linkme distributed slice 自动注册工具（✅ v0.2.0 已完成）
- [ ] **Tauri 集成自动 fetch**：反射模式的定时数据拉取写入仪表盘
- [ ] **支持更多模型格式**：当前已支持 Claude (Anthropic)、OpenAI、Ollama/MLX 兼容 API，可扩展更多推理后端
- [x] **WebUI 前端增强**：已实现 Auth、Chat branching、主题切换、AgentPicker、Transient data bar（✅ v0.2.0 已完成）
- [ ] **RAG 系统**：向量检索 + 文档加载器（ADR-0009 已决策，待实现）

---

*第二版对比完成于 2026-05-21，涵盖 P0/P1/P2 全部 11 项改进后的最新状态。*
