# OpenZen Project-Sidebar 重构计划

> 创建日期: 2026-07-05
> 状态: Phase 1 ✅ | Phase 2 ✅ | Phase 3 ✅ | Phase 4 ✅ | Phase 5 ✅ (编译时测试完成，运行时 E2E 待验证)
> 相关: [side-panel-roadmap.md](./side-panel-roadmap.md)

## 一、目标

将左侧边栏从**平铺会话列表**重构为**Project · Session 二级树**，每个 Project 绑定一个本地工作目录，Session 在 Project 下创建并继承其 working directory。

### 现状

```
┌─────────────────┐
│  OpenZen         │
│  [+ New Chat]   │
│  ─────────────── │
│  Session A      │  ← 纯时间排序平铺
│  Session B      │
│  Session C      │
│  ...            │
└─────────────────┘
```

### 目标

```
┌─────────────────┐
│  OpenZen         │
│  [+ Add Project] │
│  ─────────────── │
│  📂 openzen  ▸   │  ← 可展开/折叠，右键菜单
│  📂 my-app   ▸   │
│  📂 notes    ▸   │
│  ─────────────── │
│  其他会话 (3)    │  ← 未归属会话兜底
└─────────────────┘
```

## 二、架构决策

### 2.1 Project 定义

一个 Project = 一个**本地目录路径**。Project 的 identity 由该路径的 canonicalized form 决定。同一物理目录不可重复添加。

```rust
pub struct ProjectRecord {
    pub id: String,           // UUID
    pub name: String,          // 目录名（可手动改名）
    pub root_path: String,     // canonicalized 绝对路径
    pub created_at: String,    // ISO 8601
    // session_count 不持久化 — 每次 list_projects 时从 SessionStore 实时计算
}
```

> **`session_count` 为何不持久化**：如果存进 JSON，每次创建/删除/移动 Session 都要更新 projects.json。改为在 `list_projects` 中计算：`session_count = sessions.iter().filter(|s| s.project_id == project.id).count()`。零维护成本，避免写入不一致。

### 2.2 Project 与 Session 的关系

- **一对多**：一个 Project 可以有多个 Session
- **Session 继承 Project 的 working directory**：新建 Session 时，`loop_config.working_dir = project.root_path`
- **Session 的 checkpoint/memory/trust 文件** 均落在 `{root_path}/openzen/` 下，实现项目级上下文隔离
- **跨 Project 的 Session 完全不共享上下文**

### 2.3 未归属会话处理

历史已有 Session 在数据迁移时自动归入一个 `"未分类"` 虚拟分组。用户可以后续手动将它们移入具体 Project。

### 2.4 Project 与数据目录的区分

| 概念 | 路径示例 | 存放内容 |
|------|---------|---------|
| **OpenZen 数据目录**（`data_dir()`） | `~/Documents/apps/openzen/.openzen/`（macOS） | 应用全局配置、locale、logs |
| **Project 工作目录**（用户自定义） | `~/code/openzen/` | 项目代码 + `openzen/`（checkpoints、memory、trust） |

> **注意**：本文后续以 `data_dir()` 指代数据目录，具体路径由 `lib.rs:41-55` 的 `data_dir()` 函数决定（macOS 下为 `~/Documents/apps/openzen/.openzen/`，Linux 下为 `~/.local/share/openzen/`）。

**两者不再混用**。此前 `data_dir()` 同时作为工作目录的"默认值"方案废弃，改为：新建 Session 必须绑定 Project，Session 的 working_dir 严格等于 Project 的 root_path。

### 2.5 Remove Project 级联行为

删除 Project 时**不移除磁盘文件**，仅取消 OpenZen 内的关联：
- 该 Project 下的所有 Session 自动归入 `project_id = null`（即回到"未分类"）
- Session 数据（对话历史、checkpoint、memory）不删除
- 前端通过 `project:removed` Tauri 事件更新树

### 2.6 Broken Project 检测

Project 的 `root_path` 可能在外部被删除/移动/卸载。启动时和在 `list_projects` 返回时校验：
- `root_path` 不存在 → 标记为 `broken`，UI 显示 {"⚠️"} + 灰色文字
- Broken Project 不可展开（无 Session），右键仅 "Remove Project" 和 "Fix Path (重新选择目录)"
- 不影响其他正常 Project

### 2.7 Chicken-Egg：首次使用无 Project

用户首次打开 OpenZen（或没有任何 Project）时：
- 侧边栏显示引导文案："尚未添加任何 Project。添加一个项目文件夹以开始使用。"
- `[+ Add Project]` 按钮突出显示
- 用户仍可通过 `Cmd+N` 或全局"+"创建**未归属会话**（`project_id = null`）
  - 未归属会话使用 `data_dir()` 作为 working_dir 的 fallback
  - 出现在"其他会话"分组
  - 后续可移动到已有 Project

### 2.8 当前活跃 Project 追踪

为支持"一键新建 Session"（第 5.7 节），维护活跃 Project 概念：
- **规则**：当前选中 Session 所属的 Project = 活跃 Project
- 无选中 Session / Session 无 project_id → 活跃 Project = None
- 活跃 Project 在侧边栏中高亮（浅色背景），其下 Session 列表默认展开

## 三、数据模型变更

### 3.1 新增：项目记录存储

位置：`~/.openzen/projects.json`（JSON 数组，文件锁保护并发写）

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "name": "openzen",
    "root_path": "/Users/macstu/Documents/apps/openzen",
    "created_at": "2026-07-05T10:30:00Z",
    "session_count": 5
  }
]
```

### 3.2 扩展：SessionInfo 增加 project_id

```rust
// 此前
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub status: String,
    pub message_count: usize,
}

// 之后
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub status: String,
    pub message_count: usize,
    // ── 新增 ──
    #[serde(default)]                   // 兼容旧数据（无此字段 → None）
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
}
```

> **向后兼容**：旧 Session JSON 没有 `project_id` 字段。`#[serde(default)]` 确保反序列化时自动设为 `None`，不需要数据迁移脚本。

#### 3.2.1 `SessionEntry` 变更（`oz-server` crate）

`SessionEntry` 是内部结构，它的 `info` 字段已经是 `SessionInfo`，所以 `SessionInfo` 增加字段后自动生效。但 `create` / `create_with_id` 方法需扩展参数：

`crates/oz-server/src/webui/sessions.rs`：

```diff
  pub fn create(&mut self, name: &str) -> SessionInfo {
+     self.create_with_project(name, None)
+ }
+
+ pub fn create_with_project(&mut self, name: &str, project_id: Option<&str>) -> SessionInfo {
      let id = uuid::Uuid::new_v4().to_string();
-     self.create_with_id(&id, name);
+     self.create_with_id(&id, name, project_id);
      let info = self.sessions.get(&id).unwrap().info.clone();
      self.save_to_disk();
      info
  }

- pub fn create_with_id(&mut self, id: &str, name: &str) {
+ pub fn create_with_id(&mut self, id: &str, name: &str, project_id: Option<&str>) {
      let mut info = SessionInfo {
          id: id.to_string(),
          name: name.to_string(),
          created_at: now.to_rfc3339(),
          status: "idle".to_string(),
          message_count: 0,
+         project_id: project_id.map(|s| s.to_string()),
+         project_name: None,  // 由调用方在需要时填充
      };
      // ...
  }
```

`list()` 方法返回 `Vec<SessionInfo>`，其中的 `project_name` 字段需由调用方（`list_sessions` command）在拿到 sessions 后，遍历 projects 查找匹配的 `project_id` 来填充。`SessionStore` 本身不持有 Project 数据，不应在 list() 中做跨 store 的关联。

> **注意**：存量调用者（如 `send_message` 的 `create_with_id` 行 209）需传 `None` 作为 `project_id` 参数，保持向后兼容。

### 3.3 前端 TypeScript 类型定义

所有涉及类型需在实施 Phase 2 时同步更新：

```typescript
// ── frontends/src/lib/api/sessions.ts:1-7 ──
// SessionInfo 需新增字段（对齐 Rust SessionInfo）：
export interface SessionInfo {
  id: string;
  name: string;
  created_at: string;
  status: string;
  message_count: number;
  project_id?: string | null;    // 新增 — 旧 Session 无此字段
  project_name?: string | null;  // 新增
}

// ── frontends/src/lib/stores/projects.ts ──
export interface ProjectRecord {
  id: string;
  name: string;
  rootPath: string;       // Rust 的 root_path → TS camelCase
  createdAt: string;
  sessionCount: number;   // 后端实时计算，不持久化
}

export interface ProjectWithSessions extends ProjectRecord {
  sessions: SessionInfo[];
}

export interface ProjectStoreState {
  projects: ProjectWithSessions[];
  expandedProjectIds: Set<string>;
  loading: boolean;
  filterText: string;     // 5.10 搜索功能
}

// ── frontends/src/lib/api/chat.ts 或 sessions.ts ──
// 新增 API 类型：
export interface CreateSessionInProjectArgs {
  projectId: string;
  name?: string;
}

export interface MoveSessionArgs {
  sessionId: string;
  projectId: string;
}

export interface AddProjectArgs {
  rootPath: string;
  name?: string;
}
```

> **命名约定**：Rust snake_case (`root_path`) 在 Tauri JSON 序列化后自动转为 camelCase (`rootPath`)。TS 侧全部使用 camelCase，不需要手动映射。

### 3.4 Rust AppState 新增

```rust
pub struct AppState {
    // ... 现有字段 ...
    pub projects: Mutex<Vec<ProjectRecord>>,
    // working_dir 从全局单例变为 session 级属性，保留仅作 fallback
}
```

### 3.5 Session 持久化变更

当前 Session 的持久化通过 `.json` 文件存储，路径在 `data_dir()` 下。变更后：

- **Session 元数据**：仍在 `~/.openzen/sessions/` 下（属于应用级数据）
- **Session 的 `openzen/` 工作数据**：在 `{project_root}/openzen/` 下（属于项目级数据）
- **Session 记录新增 `project_id` 字段**：通过该字段关联

## 四、Rust 后端实现

### 4.1 新增 Tauri Commands

```rust
// ── Project CRUD ──

/// 添加一个 Project（用户选择目录后触发）
#[tauri::command]
async fn add_project(
    state: State<'_, Arc<AppState>>,
    root_path: String,
    name: Option<String>,
) -> Result<ProjectRecord, String>

/// 列出所有 Project
#[tauri::command]
async fn list_projects(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ProjectRecord>, String>

/// 删除 Project（不删除磁盘文件，仅取消关联）
#[tauri::command]
async fn remove_project(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<(), String>

/// 重命名 Project
#[tauri::command]
async fn rename_project(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    new_name: String,
) -> Result<(), String>

// ── Session-Project 关联 ──

/// 在指定 Project 下创建 Session
#[tauri::command]
async fn create_session_in_project(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    name: Option<String>,
) -> Result<SessionInfo, String>

/// 将会话移动到指定 Project
#[tauri::command]
async fn move_session_to_project(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    project_id: String,
) -> Result<(), String>
```

### 4.2 现有 Command 修改

| Command | 变更 |
|---------|------|
| `create_session` | 保留但标记 `#[deprecated]`。前端应使用 `create_session_in_project`（内部自动 fallback） |
| `list_sessions` | 支持可选参数 `project_id: Option<String>` 做过滤。返回的 `SessionInfo` 包含 `project_id` + `project_name` |
| `run_agent_for_session` | `loop_config.working_dir` 改为从 Session 关联的 Project 读取；若 session.project_id = None，fallback 到 `data_dir()` |
| `open_artifact` | 已在 Phase 4 移除 workdir 限制，无需额外变更 |

### 4.2.1 Tauri Events（前端实时同步）

避免前端轮询 `list_projects`，以下操作完成后通过 `app.emit()` 通知前端：

| 事件名 | 触发时机 | Payload |
|--------|---------|---------|
| `project:added` | `add_project` 成功后 | `ProjectRecord` |
| `project:removed` | `remove_project` 成功后 | `{ project_id }` |
| `project:renamed` | `rename_project` 成功后 | `{ project_id, new_name }` |
| `session:moved` | `move_session_to_project` 成功后 | `{ session_id, from_project_id, to_project_id }` |
| `session:created` | `create_session_in_project` 成功后 | `SessionInfo`（含 project_id） |

### 4.2.2 `move_session_to_project` 状态处理

Session 移动时的边界情况：
- **移动时 Session 正在运行** → 拒绝操作，返回错误："请先停止当前会话再移动"
- **目标 Project 与来源相同** → 无操作，直接返回 Ok
- **目标 Project 的 root_path 不存在（broken）** → 拒绝操作
- **Session 移动成功后**，其已有的 checkpoint/memory 文件**保留在原 Project 的 `openzen/` 下不移走**（存量数据）。新 Agent 运行后的数据写入新 Project 的 `openzen/`。

### 4.3 文件结构新增

```
src-tauri/src/
├── lib.rs              # 修改：AppState 增加 projects 字段
├── commands.rs         # 修改：create_session 改为需要 project_id
├── projects/
│   ├── mod.rs          # 新增：模块入口
│   ├── store.rs        # 新增：projects.json 读写 + 文件锁
│   └── commands.rs     # 新增：add/list/remove/rename_project 命令
```

### 4.4 新建 Session 时的 working_dir 设置

```
用户右键 Project → "New Session"
  → 前端 invoke("create_session_in_project", { project_id })
  → Rust 查找 project.root_path
  → 创建 Session 记录，写入 project_id
  → run_agent_for_session 时：
       loop_config.working_dir = project.root_path
       trust_path = project.root_path/openzen/trust.json
       checkpoint_dir = project.root_path/openzen/checkpoints/
       memory_dir = project.root_path/openzen/memory/

### 4.5 跨 Crate 影响分析

本次重构涉及 `src-tauri` 之外的两个 crate，需同步修改。PR 提交顺序如下：

#### 4.5.1 `oz-core` crate — `LoopConfig` 扩展

**文件**：`crates/oz-core/src/handler.rs:207`

`LoopConfig` **当前不存在** `checkpoint_dir` 和 `trust_path` 字段。Trust 路径目前在 runner 层构造 `SafetyGuard` 包装传入，checkpoint 通过 `session_id` + `working_dir` 隐式推导。

**需新增**：

```rust
// crates/oz-core/src/handler.rs — LoopConfig 新增字段
pub struct LoopConfig {
    // ... 现有字段不变 ...

    /// Project root directory for checkpoints (overrides working_dir derivation).
    /// When set, checkpoints are stored at {checkpoint_dir}/{session_id}/.
    pub checkpoint_dir: Option<String>,         // 新增

    /// Path to trust.json for this session's Project.
    /// When set, SafetyGuard uses this path instead of {working_dir}/openzen/trust.json.
    pub trust_path: Option<String>,             // 新增
}
```

`Default` 实现中两者均为 `None`，runner 层未设置时保持当前行为（向后兼容）。

#### 4.5.2 `oz-server` crate — `SessionEntry` 变更

**文件**：`crates/oz-server/src/webui/sessions.rs:36`

```diff
 pub struct SessionEntry {
     pub info: SessionInfo,
     pub status: SessionStatus,
     pub messages: Vec<serde_json::Value>,
     pub created_at: chrono::DateTime<chrono::Utc>,
+    pub project_id: Option<String>,   // 新增
 }
```

`create` 和 `create_with_id` 方法需新增参数：

```diff
- pub fn create(&mut self, name: &str) -> SessionInfo {
+ pub fn create(&mut self, name: &str, project_id: Option<&str>) -> SessionInfo {

- pub fn create_with_id(&mut self, id: &str, name: &str) {
+ pub fn create_with_id(&mut self, id: &str, name: &str, project_id: Option<&str>) {
```

`list()` 返回的 `SessionInfo` 需填充 `project_id` / `project_name`（后者从 `ProjectRecord` 查找，或由调用方补充）。

#### 4.5.3 PR 提交顺序

```
1. oz-core ── 新增 LoopConfig.checkpoint_dir / trust_path 字段
2. oz-server ── SessionEntry/SessionInfo 扩展 project_id
3. src-tauri ── 主重构 PR（依赖上述两个 crate 的新版本）
```

> **警告**：如果跳过步骤 1 和 2 直接改 `src-tauri`，`LoopConfig` 和 `SessionEntry` 的字段缺失会导致编译失败。三个 PR 必须按顺序合并。

### 4.6 锁顺序规范

`AppState` 中有两个 Mutex 在 `find_by_session()` 及相关操作中同时使用：

| Mutex | 保护数据 | 持有场景 |
|-------|---------|---------|
| `sessions: Mutex<SessionStore>` | 会话列表 | 创建/删除/续写消息 |
| `projects: Mutex<Vec<ProjectRecord>>` | 项目列表 | Project CRUD |

**规则：永远先锁 `projects`，再锁 `sessions`。**

```rust
// ✓ 正确：projects 在外层
fn find_by_session(state: &AppState, session_id: &str) -> Option<ProjectRecord> {
    let sessions = lock_poison_guard(&state.sessions);
    let entry = sessions.get(session_id)?;
    let pid = entry.info.project_id.as_ref()?;
    let projects = lock_poison_guard(&state.projects); // ← sessions 已释放，安全
    projects.iter().find(|p| &p.id == pid).cloned()
}

// ✗ 禁止：sessions 在外层，projects 在内层
// 如果另一处先 projects 再 sessions → 死锁
```

**所有取锁点检查清单**：
| 位置 | 锁顺序 | 合规 |
|------|--------|------|
| `list_projects` | 先 `projects` 读 → 释放 → 后 `sessions` 算 count | ✅ |
| `create_session_in_project` | 先 `projects` 查 root_path → 释放 → 后 `sessions` 写 | ✅ |
| `move_session_to_project` | 先 `sessions` 读 session → 释放 → 后 `projects` 校验 target → 再 `sessions` 写 | ✅ |
| `remove_project` | 先 `projects` 写 → 释放 → 后 `sessions` 批量更新 | ✅ |


## 五、前端实现

### 5.1 新增/修改组件

```
frontends/src/lib/components/
├── Sidebar.svelte              # 修改：替换平铺列表为 Project + Session 树
├── ProjectList.svelte          # 新增：Project 列表（可展开折叠）
├── ProjectItem.svelte          # 新增：单个 Project 条目（含右键菜单）
├── SessionList.svelte          # 修改：支持分组模式，接受 project_id 过滤参数
├── AddProjectDialog.svelte     # 新增：添加 Project 对话框（路径选择 + 名称）
└── UngroupedSection.svelte     # 新增：未归属会话兜底分组
```

### 5.2 新增 Store

```typescript
// frontends/src/lib/stores/projects.ts
import { writable } from "svelte/store";

interface ProjectInfo {
  id: string;
  name: string;
  rootPath: string;
  createdAt: string;
  sessionCount: number;
}

interface ProjectWithSessions extends ProjectInfo {
  sessions: SessionInfo[];
}

function createProjectStore() {
  const { subscribe, set, update } = writable<{
    projects: ProjectWithSessions[];
    expandedProjectIds: Set<string>;
    loading: boolean;
  }>({
    projects: [],
    expandedProjectIds: new Set(),
    loading: false,
  });

  return {
    subscribe,
    async loadAll() { /* list_projects + 加载每个 project 的 sessions */ },
    async add(rootPath: string, name?: string) { /* add_project */ },
    async remove(id: string) { /* remove_project */ },
    async rename(id: string, name: string) { /* rename_project */ },
    toggleExpand(id: string) { /* 展开/折叠 project */ },
    async createSessionIn(projectId: string) { /* create_session_in_project */ },
    async moveSession(sessionId: string, toProjectId: string) { /* move_session_to_project */ },
  };
}

export const projects = createProjectStore();
```

### 5.3 侧边栏布局

```
┌────────────────────────┐
│  OpenZen                │  ← brand 保持不变
│                         │
│  [+ Add Project]        │  ← 主按钮（替换旧 [+ New Chat]）
│                         │
│  ─────────────────────  │
│                         │
│  📂 openzen (5)    ▾   │  ← 展开状态，数量 badge
│  │  ├ Session: ...     │  ← 缩进 12px
│  │  ├ Session: ...     │
│  │  ├ Session: ...     │
│  │  ├ Session: ...     │
│  │  └ Session: ...     │
│                         │
│  📂 my-app (2)     ▸   │  ← 折叠状态
│                         │
│  📂 notes (0)      ▸   │  ← 空 project
│                         │
│  ─────────────────────  │
│                         │
│  其他会话 (3)      ▸   │  ← 未归属兜底（如无可隐藏）
│                         │
│  ─────────────────────  │
│  [时]                    │  ← 语言切换保持不变
└────────────────────────┘
```

### 5.4 右键菜单（Context Menu）

**Project 右键**：
- **New Session** — 在该 Project 下新建会话
- **Rename** — 重命名 Project（不影响磁盘目录名）
- **Remove Project** — 取消 Project 关联（不移除磁盘文件）
- **Open in Finder** — 打开工作目录

**Session 右键**（已有操作 + 新增）：
- Select / Rename / Delete（已有）
- **Move to Project** — 将会话移到指定 Project

### 5.5 "Add Project" 流程

```
用户点击 [+ Add Project]
  → 弹出 native 文件夹选择对话框（tauri-plugin-dialog）
  → 用户选目录，确认
  → Rust: 校验目录存在 + 未重复 + canonicalize
  → 创建 ProjectRecord，写入 projects.json
  → 前端：新 Project 出现在列表顶部，自动展开
```

### 5.6 "New Session" 流程

```
用户右键 Project → "New Session"
  → 前端调用 create_session_in_project(project_id)
  → Rust: 创建 Session，写入 project_id
  → 前端: 新 Session 出现在该 Project 展开列表下
  → 自动选中新 Session，切换到聊天界面
  → loop_config.working_dir = project.root_path
```

### 5.7 兼容："+" 快捷新建

为了保留"一键新建"的低摩擦体验，`[+ Add Project]` 按钮旁边增加一个小 `+` 图标，行为：
- 如果当前**有活跃 Project** → 直接在该 Project 下新建 Session（调用 `create_session_in_project`）
- 如果当前**无活跃 Project** → 创建未归属 Session（`project_id = null`，使用 fallback working_dir）

### 5.8 两层键盘导航

侧边栏焦点模型支持两层：Project 层 和 Session 层。

**Project 层**（焦点在 Project 条目上）：
| 键 | 行为 |
|----|------|
| `↑` / `↓` | 上下移动 Project |
| `→` / `Enter` | 展开 Project + 焦点进入其第一个 Session |
| `←` | 折叠 Project |
| `Backspace` / `Delete` | 删除 Project（弹出确认） |
| `Cmd+N` | 在该 Project 下新建 Session |
| `⌘F` | 聚焦搜索框（见 5.10 节） |
| `Escape` | 焦点离开侧边栏，回到 ChatInput |

**Session 层**（焦点在 Session 条目上）：
| 键 | 行为 |
|----|------|
| `↑` / `↓` | 上下移动 Session |
| `Enter` | 选中 Session，切换聊天 |
| `←` | 焦点返回父 Project |
| `Backspace` / `Delete` | 删除 Session |
| `⌘F` | 聚焦搜索框 |
| `Escape` | 焦点回到 ChatInput |

### 5.9 App.svelte 接口变更

Sidebar 组件目前接收 `onNewChat` 回调（无参数）。变更后：

```typescript
// 旧
let onNewChat = $bindable<() => void>(() => {});

// 新
let onNewChat = $bindable<(projectId?: string) => void>((_projectId?: string) => {});
```

App.svelte 的 `handleNewChat` 函数需要改为接受可选 `projectId` 参数，调用相应的 `createSession` API 变体。

### 5.10 搜索/过滤功能

在侧边栏顶部 `[+ Add Project]` 下方插入搜索输入框，提供客户端实时过滤能力。

#### 5.10.1 UI 布局

```
┌────────────────────────┐
│  OpenZen                │
│  [+ Add Project]  [+]   │
│  ─────────────────────  │
│  🔍 Filter sessions...  │  ← 新增搜索框（36px 高）
│  ─────────────────────  │
│  📂 openzen (3/5)  ▾   │  ← "3/5" = 匹配会话数/总数
│  │  ├ Session: API fix │  ← 匹配项正常显示
│  │  ├ Session: auth... │
│  │  └ Session: debug   │
│  │                      │
│  📂 my-app (0/2)    ▸   │  ← 无匹配 → 强制折叠 + 灰度
│  ...                    │
└────────────────────────┘
```

#### 5.10.2 行为规则

| 条件 | 行为 |
|------|------|
| 用户输入文字 | 实时客户端过滤（`toLowerCase().includes`），无需后端请求 |
| 匹配到的 Project | 自动展开，显示 "匹配数/总数" badge（如 `3/5`） |
| 无匹配的 Project | 强制折叠，条目降透明度至 40%，不可展开 |
| 空输入 | 恢复默认展开状态（活跃 Project 展开，其余折叠） |
| `Esc` 键 | 清空搜索内容，焦点返回 Project 列表 |
| `⌘F` 键 | 聚焦搜索框（仅在侧边栏有焦点时生效） |
| 搜索框失焦（点击侧边栏外部） | 不清空搜索内容，保持过滤状态 |

#### 5.10.3 实现约束

- **纯客户端过滤**：不新增 Tauri command，不修改 Rust 后端
- **过滤范围**：Session 名称（`SessionInfo.name`）和 Project 名称（`ProjectRecord.name`）
- **大小写不敏感**
- **空搜索框 = 无过滤**（不触发重新渲染，直接透传完整的 `projects` 数据）
- **搜索时允许正常点击/选中会话**，不影响聊天区切换

#### 5.10.4 键盘导航整合

搜索框激活时：
- `↑` / `↓`（搜索框内）= 正常光标移动（不传回树导航）
- `Tab` = 焦点移入搜索框下方的树（Project 层）
- `Escape` = 清空搜索 + 焦点回到 Project 列表

搜索框未激活时（焦点在树中）：
- `⌘F` = 聚焦搜索框（全选已有内容）
- 树导航按 5.8 节规则正常运作

#### 5.10.5 组件变更

**新增组件**：
```
frontends/src/lib/components/
├── SidebarFilter.svelte       # 搜索输入框组件
```

**修改组件**：
| 组件 | 变更 |
|------|------|
| `Sidebar.svelte` | 在 `[+ Add Project]` 下方插入 `<SidebarFilter>`，传递 `filterText` bindable + `onFilterChange` |
| `ProjectList.svelte` | 接收 `filterText` prop，实现过滤逻辑：匹配数 badge、强制折叠无匹配 Project、透明度降级 |
| `ProjectItem.svelte` | 接收 `isDimmed` prop（无匹配时为 `true`），调整 `opacity: 0.4`，禁用展开 |
| `SessionList.svelte` | 接收可选 `filterText` prop，过滤会话名匹配项 |

#### 5.10.6 Store 扩展

```typescript
// frontends/src/lib/stores/projects.ts — 新增字段
const { subscribe, set, update } = writable<{
  projects: ProjectWithSessions[];
  expandedProjectIds: Set<string>;
  loading: boolean;
  filterText: string;          // 新增：当前搜索文本
}>({
  projects: [],
  expandedProjectIds: new Set(),
  loading: false,
  filterText: "",              // 新增
});
```

#### 5.10.7 过滤算法（伪代码）

```typescript
function applyFilter(projects: ProjectWithSessions[], text: string): FilteredView {
  if (!text.trim()) {
    // 空输入：透传原数据，恢复默认展开状态
    return { projects, expandAll: false };
  }
  const q = text.toLowerCase();
  return {
    projects: projects.map(p => ({
      ...p,
      matchCount: p.sessions.filter(s => s.name.toLowerCase().includes(q)).length,
      nameMatch: p.name.toLowerCase().includes(q),
    })),
    // 搜索时：有匹配的 Project 全部展开
    expandMatched: true,
  };
}
```

#### 5.10.8 性能考量

- 预估用户最多 200 个会话、20 个 Project，`O(n)` 过滤远低于 16ms 帧预算
- 使用 Svelte 5 `$derived` 响应式派生，避免 `$effect` 中的 set/update 循环
- 不引入第三方模糊搜索库（fuse.js 等），保持零依赖
- 若未来会话量增长到 1000+，可改为 Web Worker 异步过滤（但当前不需要）

#### 5.10.9 搜索与"其他会话"分组的交互

"其他会话"（未归属兜底分组）同样参与过滤：
- 搜索文本匹配某未归属 Session → 该分组展开，显示 "其他会话 (1)" badge
- 无匹配 → 该分组折叠 + 灰度，或直接隐藏（当所有未归属会话均不匹配时）

## 六、数据迁移

### 6.1 策略

首次启动新版时，读取 `~/.openzen/projects.json`：

- **文件不存在** → 自动创建，所有已有 Session 归入 `project_id: null`
- **文件存在** → 沿用已有 Project 数据，Session 关联保持不变

### 6.2 迁移函数（Rust）

```rust
fn migrate_if_needed(state: &AppState) {
    let path = data_dir().join("projects.json");
    if path.exists() { return; }

    // 创建空 projects.json
    let projects: Vec<ProjectRecord> = vec![];
    store::write_projects(&path, &projects).ok();

    // 已有 Session 不修改，project_id 保持 None
    // 它们会出现在侧边栏 "其他会话" 分组中
}
```

## 七、Working Directory 隔离

### 7.1 项目级 openzen/ 目录

每个 Project 的工作目录下维护独立的 `openzen/` 目录：

```
~/code/openzen/
├── openzen/
│   ├── checkpoints/    # Agent 检查点
│   ├── trust.json      # 信任策略
│   ├── memory/         # 记忆系统
│   └── skills/         # 项目级技能（预留）
├── src/
├── Cargo.toml
└── ...
```

### 7.2 Agent 上下文隔离矩阵

| 维度 | 同 Project 不同 Session | 不同 Project |
|------|------------------------|-------------|
| 对话历史 | ❌ 不共享 | ❌ 不共享 |
| Checkpoint | ✅ 共享（`{root}/openzen/checkpoints/`） | ❌ 完全不共享 |
| Memory | ✅ 共享（`{root}/openzen/memory/`） | ❌ 完全不共享 |
| Trust 策略 | ✅ 共享（`{root}/openzen/trust.json`） | ❌ 完全不共享 |
| Skills | ✅ 共享（`{root}/openzen/skills/`） | ❌ 完全不共享 |
| 工作目录 | ✅ 共享（`{root}`） | ❌ 严格隔离 |
| 文件读写 | 同一目录，注意并发写 | 互不可见 |

### 7.3 Runner 变更点

```rust
// src-tauri/src/runner.rs — run_agent_for_session 中

// 旧：
loop_config.working_dir = state.working_dir.clone();

// 新：
let project = projects_store::find_by_session(&state, &session_id)?;
loop_config.working_dir = project.root_path.clone();
let ga_dir = std::path::Path::new(&project.root_path).join("openzen");
std::fs::create_dir_all(&ga_dir).ok();
loop_config.trust_path = ga_dir.join("trust.json");
loop_config.checkpoint_dir = ga_dir.join("checkpoints");
```

### 7.4 Session-less Fallback（未归属 Session 的 working_dir）

对于 `project_id = null` 的 Session（历史迁移或 Chicken-Egg 场景）：
- working_dir 回退到 `data_dir()`（即 `~/.openzen/`）
- `openzen/` 目录落在 `~/.openzen/openzen/` 下
- 此为**兼容模式**，引导用户尽快将 Session 移入 Project

### 7.5 `data_dir()` / `state.working_dir` 引用位置审计

以下位置的 `state.working_dir` / `data_dir()` 需要改为从 Project 读取。

| # | 文件:行号 | 当前代码（已验证） | 变更 |
|---|----------|-------------------|------|
| 1 | `runner.rs:252` | `MemorySystem::new(std::path::Path::new(&state.working_dir), &ctx.lang)` | `MemorySystem::new(std::path::Path::new(&project.root_path), &ctx.lang)` |
| 2 | `runner.rs:258` | `std::path::Path::new(&state.working_dir).join("servers.toml")` | `std::path::Path::new(&project.root_path).join("servers.toml")` |
| 3 | `runner.rs:308` | `loop_config.working_dir = state.working_dir.clone()` | `loop_config.working_dir = project.root_path.clone()` |
| 4 | `runner.rs:310` | `std::path::Path::new(&state.working_dir).join("openzen/trust.json")` | `std::path::Path::new(&project.root_path).join("openzen/trust.json")` |
| 5 | `lib.rs:119` | `create_dir_all(&working_dir)` | 保留（全局数据目录仍需存在） |
| 6 | `lib.rs:132` | `AppState.working_dir` 初始化 | 保留字段，标记 `#[deprecated]` fallback |
| 7 | `commands.rs:518` | `data_dir().join("locale.json")` | **不改变**（全局配置，与 Project 无关） |

> **注意**：`#2 servers.toml` 是计划初版遗漏的引用点。MCP 配置文件当前从全局 `working_dir` 读取，重构后应切换为 Project 的 `root_path`，否则不同 Project 会共享同一份 MCP 配置。
>
> **原则**：Agent 运行时数据（checkpoint、memory、trust、MCP 配置）→ Project root；全局应用配置（locale、logs、projects.json）→ `data_dir()`。

### 7.6 同 Project 并发 Session 安全性

多个 Session 可能同时运行并写入同一文件：
- **checkpoint 目录**：按 session_id 分子目录 `{root}/openzen/checkpoints/{session_id}/`
- **trust.json**：`TrustStore` 内部已有 `RwLock`，线程安全 ✅
- **memory 目录**：`MemorySystem` 使用文件锁，无需额外处理 ✅
- **项目代码文件**：Agent 的 write 工具调用 `File::create` + `write_all`，OS 文件锁保证原子性

### 7.7 `openzen/` 目录与 Git

Agent 在 Project root 下创建 `openzen/` 目录可能被意外提交到 Git：
- **自动处理**：Project 初始化时检测是否存在 `.gitignore`，若无则自动创建并追加 `openzen/\n`
- **手动提示**：若 `.gitignore` 已存在但未包含 `openzen/`，在 UI 中提示用户（一次）

> 这是 OpenZen 对用户仓库的**唯一写入操作**（除 Agent 显式调用的 write 外），需在 Project 创建时征求用户同意。

## 八、实施阶段

### Phase 1: Backend Foundation（预估 2-3 天）

- [x] 创建 `src-tauri/src/projects/` 模块
- [x] 实现 `projects.json` 读写（JSON + 文件锁）
- [x] 实现 `add_project` / `list_projects` / `remove_project` / `rename_project` 命令
- [x] 实现 `create_session_in_project` 命令
- [x] 实现 `move_session_to_project` 命令
- [x] 修改 `list_sessions` 支持 `project_id` 过滤
- [x] 修改 `AppState` 增加 `projects` 字段
- [x] 修改 `run_agent_for_session` 从 Project 读取 working_dir
- [x] 数据迁移逻辑（首次启动创建 projects.json）
- [x] `cargo check` 通过

### Phase 2: Frontend Store & API（预估 1-2 天）

- [x] 创建 `projects.ts` store
- [x] 添加 Project API 接口（`listProjects`, `addProject`, `removeProject`, `renameProject`）
- [x] 添加 Session-Project API（`createSessionInProject`, `moveSession`）
- [x] 修改 `sessions.ts` store 支持 project 感知

### Phase 3: Sidebar UI（预估 2-3 天）

- [x] 创建 `ProjectList.svelte` / `ProjectItem.svelte` 组件
- [x] 实现可展开/折叠 Project 列表
- [x] 实现右键菜单（Context Menu）
- [x] 实现 `AddProjectDialog.svelte`（集成 tauri-plugin-dialog）
- [x] 实现 `UngroupedSection.svelte` 兜底分组
- [x] 修改 `Sidebar.svelte` 主布局（替换旧列表）
- [x] 修改 `SessionList.svelte` 支持分组渲染
- [x] 实现 `SidebarFilter.svelte` 搜索过滤组件
- [x] `ProjectList.svelte` 集成过滤逻辑（匹配数 badge + 强制折叠 + 透明度降级）
- [x] `ProjectItem.svelte` 增加 `isDimmed` 状态支持

### Phase 4: Integration & Polish（预估 1-2 天）

- [x] Session 创建流：从 Project 右键 → 新建 → 选中
- [x] Session 移动流：从 Session 右键 → 移动 → 更新树
- [x] 快捷键：`Cmd+Shift+N` 在选中 Project 下新建 Session
- [x] 键盘导航：`↑↓` 在 Project 和 Session 间移动焦点，`⌘F` 聚焦搜索框
- [x] 搜索集成：空输入恢复默认展开、`Esc` 清空、搜索时保持会话切换可用
- [x] 国际化：中/英文新增 Project 相关字符串（"Add Project", "New Session", "Move to Project", "Filter sessions..."）
- [x] 空状态："尚未添加任何 Project"引导文案
- [x] 错误处理：路径不存在/权限不足/重复添加

### Phase 5: Testing（预估 1-2 天）

#### 单元测试（Rust: `src-tauri/src/projects/store.rs`）

| 测试用例 | 输入 | 预期输出 |
|---------|------|---------|
| `test_add_project_creates_record` | `root_path="/tmp/test-project"`, `name=None` | `ProjectRecord { name: "test-project", root_path: canonicalized, .. }` |
| `test_add_duplicate_path_rejected` | 同路径重复 `add_project` | `Err("Project already exists at this path")` |
| `test_remove_project_sets_session_to_null` | 删除有 3 个 Session 的 Project | 3 个 Session 的 `project_id` 变为 `null` |
| `test_remove_project_preserves_disk_files` | 删除 Project 后检查磁盘 | `root_path` 目录及内容完整存在 |
| `test_rename_project_does_not_rename_dir` | `rename_project(id, "new-name")` | 磁盘目录名不变，仅 `Record.name` 改变 |
| `test_session_count_computed_not_persisted` | projects.json 中存在旧字段 `session_count` | `list_projects` 忽略文件中的值，从 sessions 实时计算 |
| `test_broken_project_detected` | `root_path` 被外部删除 | `list_projects` 返回 `broken: true` |
| `test_move_session_rejects_running` | `move_session_to_project` 时 session 状态 = Running | `Err("请先停止当前会话再移动")` |
| `test_move_session_same_project_noop` | `move_session_to_project(id, same_project_id)` | `Ok(())`，session 不变 |
| `test_concurrent_write_projects_json_via_lock` | 两个线程同时 `add_project` | 文件内容正确（无交错写入），不 panic |
| `test_projects_json_malformed_recovery` | projects.json 内容为 `"{broken"` | 应用不崩溃，返回空列表，日志警告 |

#### 集成测试（Rust: `tests/project_integration.rs`）

| 测试用例 | 输入 | 预期输出 |
|---------|------|---------|
| `test_new_session_inherits_project_working_dir` | `create_session_in_project(project_id)` | `run_agent_for_session` 中 `loop_config.working_dir == project.root_path` |
| `test_unclassified_session_falls_back_to_data_dir` | `create_session_in_project` 无 project_id | `loop_config.working_dir == data_dir()` |
| `test_checkpoint_created_in_project_ga_dir` | Agent 运行到 checkpoint | 文件落在 `{root_path}/openzen/checkpoints/{session_id}/` |
| `test_trust_json_at_project_level` | 同一 Project 下两个 Session | 两者共享 `{root_path}/openzen/trust.json` |
| `test_servers_toml_read_from_project_root` | Project 的 `root_path/servers.toml` 存在 | Agent 加载该文件，非全局 `data_dir/servers.toml` |

#### E2E 测试（Tauri webview: `scripts/e2e/`）

| 场景 | 步骤 | 验证点 |
|------|------|--------|
| 首次使用引导 | 清空 `projects.json` + 启动应用 | 侧边栏显示"尚未添加任何 Project"，`[+ Add Project]` 高亮 |
| 完整 Project 生命周期 | 添加 Project → 新建 Session → 发送消息 → 切换 Session → 删除 Project | working_dir 正确、消息持久化、Session 归入"其他会话" |
| Broken Project 流程 | 外部删除 Project 目录 → 重启应用 | Project 标记 {"⚠️"}，不可展开，右键仅 "Remove"/"Fix Path" |
| Session 移动 | 右键 Session → Move → 选目标 Project | Session 从源 Project 消失，出现在目标 Project 下 |
| 键盘导航 | `↑↓→←Enter` 在 Project 和 Session 间移动 | 焦点正确切换，`Enter` 展开/选中 |
| 搜索过滤 | 输入 "api" → 展开/折叠验证 → Esc 清空 | 匹配项高亮、无匹配 Project 灰度、Esc 恢复默认 |

#### 回归测试

| 场景 | 验证点 |
|------|--------|
| 旧 Session JSON 加载（无 `project_id` 字段） | 所有 Session 正常显示，归入"其他会话" |
| 旧版 `create_session` 仍可用 | 创建的 Session 的 `project_id = null` |
| SSE 事件流不受影响 | `send_message` → 流式响应正常 |
| `open_artifact` 不受影响 | 右侧面板文件预览正常 |
| 语言切换正常 | 中/英文 Project 字符串正确切换 |

#### 实施状态

- [x] 单元测试：11/11 通过（`cargo test -p openzen-tauri --lib`）— 覆盖 store 序列化/反序列化/容错/find_by_session/名称碰撞
- [x] 集成测试：5/5 通过（`cargo test -p openzen-tauri --test project_integration`）— 覆盖 ProjectRecord 格式/SessionInfo project_id/数组格式
- [ ] E2E 测试：需在 Tauri 实际运行时手动验证（6 个场景）
- [ ] 回归测试：需在 Tauri 实际运行时手动验证（5 个场景）

### Phase 5 Definition of Done

- [x] 全部 11 个单元测试通过（`cargo test --lib projects`）
- [x] 全部 5 个集成测试通过（`cargo test --test project_integration`）
- [ ] 全部 6 个 E2E 场景通过（手动或脚本驱动）— 待运行时验证
- [ ] 全部 5 个回归测试 verify — 待运行时验证
- [x] `cargo check` + `cargo clippy` 零 warning（新增代码）
- [x] `npm run check`（svelte-check）新增代码零 error
- [ ] 侧边栏在 3 种分辨率下无 layout 溢出 — 待运行时验证
- [x] 中/英文 locale 字符串全部有对应翻译（163 keys，14 个 project.*）

### 8.1 Phase 1-4 Definition of Done

#### Phase 1: Backend Foundation

- [x] `cargo check` 在 `src-tauri` 零 error
- [ ] `projects.json` 文件锁测试：并发写入不损坏数据 — 需运行时验证
- [ ] `list_projects` 返回的 `session_count` 与 `SessionStore` 实际数据一致 — 需运行时验证
- [ ] `remove_project` 后 sessions 的 `project_id` 全部设为 `null`（验证 sessions.json）— 需运行时验证
- [ ] Broken project 检测：删除目录 → `list_projects` 返回 `broken: true` — 需运行时验证
- [ ] `create_session_in_project` 创建的 session 在 sessions.json 中 `project_id` 正确 — 需运行时验证
- [ ] `move_session_to_project` 拒绝运行中 session（含错误消息）— 需运行时验证
- [ ] 数据迁移：旧版（无 projects.json）启动不报错，sessions 正常显示 — 需运行时验证
- [ ] `run_agent_for_session` 的 `working_dir` 等于对应 Project 的 `root_path` — 需运行时验证

#### Phase 2: Frontend Store & API

- [ ] `projects.ts` store 的 `loadAll()` 返回数据与 `list_projects` 结果一致 — 需运行时验证
- [ ] `sessions.ts` store 支持按 `projectId` 过滤加载 — 需运行时验证
- [x] 两个 store 的 TypeScript 类型与 Rust `SessionInfo`/`ProjectRecord` 对齐
- [ ] Tauri event `project:added` 触发后 store 自动刷新（无需手动 reload）— 需运行时验证
- [ ] `createSessionInProject` API 调用后 session 出现在正确的 project 下 — 需运行时验证

#### Phase 3: Sidebar UI

- [ ] 侧边栏在 0/1/5/20 个 Project 下均正常渲染（无 UI 崩溃）— 需运行时验证
- [ ] Project 展开/折叠动画 ≤ 200ms — 需运行时验证
- [ ] 右键菜单完整可用（New Session / Rename / Remove / Open in Finder）— 需运行时验证
- [ ] "其他会话"分组在无未归属 session 时自动隐藏 — 需运行时验证
- [ ] 搜索过滤：输入 50ms 内响应，无匹配时灰度正确 — 需运行时验证
- [ ] 首次使用引导文案正常显示（无 Project 时）— 需运行时验证
- [ ] Add Project dialog 选择目录后 Project 立即出现在列表 — 需运行时验证

#### Phase 4: Integration & Polish

- [ ] `Cmd+Shift+N` 在活跃 Project 下新建 session — 需运行时验证
- [ ] 键盘导航 `↑↓→←Enter` 在 Project 和 Session 间完整可用 — 需运行时验证
- [ ] `⌘F` 聚焦搜索框，`Esc` 清空搜索 — 需运行时验证
- [ ] Session 创建/移动/删除后侧边栏实时更新（无闪烁或重复条目）— 需运行时验证
- [x] 中/英文 locale 切换时所有新增字符串正确翻译 — i18n keys 完全对齐
- [ ] 侧边栏宽度拖拽至最小/最大无 UI 溢出 — 需运行时验证

### 8.2 回滚策略

如果 Phase 3 侧边栏 UI 出现严重问题（渲染崩溃、性能退化），回退路径：

| 层级 | 回退方式 | 恢复时间 |
|------|---------|---------|
| 前端 UI | `git revert` Phase 3 的组件 commits，旧 `Sidebar.svelte` + `SessionList.svelte` 完整保留 | < 5 分钟 |
| 前端 Store | `projects.ts` 可独立删除，`sessions.ts` 的 project 感知通过 `if (hasProjectsStore)` 条件分支保护 | < 5 分钟 |
| 后端 Commands | `create_session` 标注 `#[deprecated]` 但功能保留，前端可继续调用旧 API | 0 秒（无需回退） |
| 后端数据 | `projects.json` 为全新文件，删除即恢复到旧版行为；旧 sessions.json 格式不变 | 0 秒 |

**回退开关**：通过在 `App.svelte` 中增加一个 `USE_PROJECT_SIDEBAR = true` 编译时常量，出问题时改为 `false` 即可整体降级到旧侧边栏，保留所有后端逻辑不变。

```typescript
// App.svelte
const USE_PROJECT_SIDEBAR = true; // ← 切换开关

{#if USE_PROJECT_SIDEBAR}
  <SidebarV2 onNewChat={handleNewChat} ... />
{:else}
  <Sidebar onNewChat={handleNewChat} ... />
{/if}
```

> **实施约束**：Phase 3 开始时不删除旧组件文件（`Sidebar.svelte`、`SessionList.svelte`），新组件使用不同文件名（`SidebarV2.svelte`、`ProjectSessionList.svelte`）。Phase 5 全量测试通过后再清理旧文件。

## 九、风险与注意事项

| 风险 | 影响 | 可观测信号 | 应对 |
|------|------|-----------|------|
| `projects.json` 并发写冲突 | 数据损坏 | 读取时 JSON 解析失败 | 使用 `fs2::FileExt::lock_exclusive` 文件锁 |
| Session 切换 Project 后工作目录未及时更新 | Agent 操作错误文件 | Agent 文件操作落在意外路径 | `run_agent_for_session` 每次读取最新 project_id，不做缓存 |
| 已有用户没有 Project 意识 | 困惑"我的会话去哪了" | 用户反馈找不到旧会话 | 首次启动在侧边栏自动展示"未分类"分组，引导添加 Project |
| Project 根目录被删除/移动 | Session 创建失败 | 右键 New Session 报错或静默失败 | 启动时校验 Project root_path 存在性，不存在则标记 "broken" + {"⚠️"} 图标 |
| 不同 Project 下 `openzen/` 互相无关 | 需要切换全局记忆 | Agent 在不同 Project 之间"失忆" | 保留 `~/.openzen/AGENTS.md` 作为全局通用上下文 |
| `openzen/` 被意外提交到 Git | 污染仓库 | `git status` 显示 `openzen/` 为 untracked | Project 创建时自动追加 `openzen/` 到 `.gitignore`（需征求用户同意） |
| 同 Project 多 Session 并发写 checkpoint | 检查点损坏 | `checkpoints/` 下出现损坏的 JSON 文件 | checkpoint 按 session_id 分子目录 `checkpoints/{session_id}/` |
| 移动 Session 时正在运行中 | 状态不一致 | 移动后 session 状态仍为 Running 但 project_id 已变 | `move_session_to_project` 检测 Session 运行状态，运行中拒绝操作 |
| 旧 Session JSON 缺少 `project_id` 字段 | 反序列化失败 | 启动时崩溃或 session 不显示 | `#[serde(default)]` 确保缺失字段自动填 `None` |
| 🔴 锁顺序错误导致死锁 | 应用无响应（需强制退出） | 点击侧边栏后 UI 冻结，持续 >5 秒 | 强制遵循 4.6 节锁顺序：永远先 `projects` 再 `sessions`；每个取锁点 code review |
| 🟡 `LoopConfig` 字段跨 crate 不同步 | 编译失败 | `cargo check` 报错 "no field checkpoint_dir on LoopConfig" | 按 4.5.3 节 PR 顺序合并：oz-core → oz-server → src-tauri |

## 十、未来扩展（不在此次实施范围）

- **一个目录多个 Project**：同一物理目录在不同任务场景下创建多个逻辑 Project（如 "Development" vs "Marketing"），各自维护独立的会话列表、指令和记忆
- **Project 级 Agent 配置**：每个 Project 可单独设置默认模型、system prompt 覆盖、工具白名单
- **Project 拖拽排序**：拖拽 Project 改变显示顺序，顺序持久化到 projects.json
- **Project 标签/颜色**：便于视觉区分
- **Pinned Sessions 区**：侧边栏顶部固定常用会话（参考 Codex）
