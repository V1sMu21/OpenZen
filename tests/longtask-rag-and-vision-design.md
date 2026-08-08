# 两个长程复杂任务设计 — 针对 30B 模型

> 面向 OpenZen 项目 | 预计每个任务 8-20 小时连续工作量 | 单任务涉及 15-40 个文件

---

## 任务 1：完整 RAG 系统 — 向量检索 + 多格式文档处理 + 混合搜索 + 重排序 + 全栈集成

### 背景

RAG (Retrieval-Augmented Generation) 是 OpenZen 路线图 Phase 1.3 的核心内容，
已在 `docs/roadmap.md` 和 `docs/comparison-vs-other-agents.md` (P3 方向) 中规划，
ADR-0009 已决策使用 fastembed，但实现被推迟。
接受准则中列为 "Future (nice to have)"。

**当前状态**：`crates/oz-rag/` 尚未创建，`/rag` 命令不存在，WebUI 无 RAG API。

### 目标

构建一个生产级 RAG 系统，让 Agent 能够：
1. 将本地文档（PDF、Word、Markdown、HTML、代码）切块并向量化
2. 对用户查询执行向量检索 + BM25 混合搜索
3. 使用重排序器优化召回结果
4. 将检索结果作为上下文注入 LLM
5. 在 TUI 和 WebUI 中提供 RAG 查询接口

### 影响范围

- **新增 crate**: `crates/oz-rag/` (~2000 行)
- **修改 Cargo.toml**: workspace.members + 依赖
- **修改 oz-tools**: 新增 `rag_query` 工具
- **修改 oz-config**: 新增 `[rag]` 配置 schema
- **修改 oz-tui**: 新增 `/rag` 命令
- **修改 oz-server**: 新增 `POST /api/rag/query` 端点
- **修改 frontends**: 新增 RAG 搜索组件
- **新增测试**: ~100 个测试用例

---

### Phase 1：研究与架构设计 (2-4 小时)

#### 1.1 外部库调研

**必须完成的研究**:
- [ ] fastembed Rust API: 嵌入模型下载、推理、内存占用
  - 研究: `fastembed = "3"` 的 `TextEmbedding` API
  - 研究: 模型下载路径配置 (`FASTEMBED_CACHE_PATH`)
  - 研究: CPU 推理性能 (batch_size, 线程数)
  - 研究: 支持的嵌入模型列表 (bge-small-zh-v1.5, bge-base-en-v1.5 等)
- [ ] SQLite FAISS vs 纯 Rust 向量存储
  - 选项 A: `sqlite-vss` (SQLite 扩展, 需要编译 C 代码)
  - 选项 B: `lancedb` (Rust 原生, Apache Arrow)
  - 选项 C: `memvid` (纯 Rust, in-memory)
  - 选项 D: 自定义 SQLite + 余弦相似度 (零额外依赖)
  - 评估: 编译体积影响、查询性能、持久化能力
- [ ] BM25 实现
  - 选项 A: `tantivy` (Rust 搜索引擎, 自带 BM25)
  - 选项 B: 自定义 BM25 (tokenize + TF-IDF)
  - 评估: 与向量检索的混合策略
- [ ] 重排序 (Reranker)
  - 选项 A: `fastembed` 的 `RerankingModel` API
  - 选项 B: 自定义交叉编码器 (需要 ONNX Runtime)
  - 评估: 延迟 vs 召回率权衡

**参考文件**: `docs/adr/0009-rag-system-selection.md` (如果存在)

#### 1.2 架构设计

**必须产出**:
- [ ] `docs/adr/0010-rag-architecture.md` — RAG 架构决策记录
  - 嵌入模型选择 (默认: bge-small-zh-v1.5, 50MB, 中英文支持)
  - 向量存储选择 (推荐: 自定义 SQLite + 余弦相似度, 零额外依赖)
  - 分块策略 (默认: 512 token, 50 token overlap)
  - 混合搜索权重 (默认: 向量 0.7 + BM25 0.3)
  - 重排序策略 (可选, 默认关闭)
- [ ] 数据流图
  ```
  用户查询 → 分词 → 向量化 → 向量检索(top_k=20) → BM25检索(top_k=20)
  → 混合排序 → 重排序(top_k=5) → 上下文组装 → LLM
  ```
- [ ] 配置 schema 草案

#### 1.3 研究模式

```
crates/oz-rag/
├── src/
│   ├── lib.rs          # 公共 API
│   ├── embedder.rs     # 嵌入模型抽象
│   ├── store.rs        # 向量存储
│   ├── chunker.rs      # 文档分块
│   ├── loader.rs       # 文档加载器
│   ├── retriever.rs    # 检索器 (向量 + BM25)
│   ├── reranker.rs     # 重排序
│   └── context.rs      # 上下文组装
├── tests/
│   ├── chunker_test.rs
│   ├── embedder_test.rs
│   ├── store_test.rs
│   ├── retriever_test.rs
│   └── integration_test.rs
└── Cargo.toml
```

---

### Phase 2：后端实现 (6-12 小时)

#### 2.1 创建 `oz-rag` crate

**文件**: `crates/oz-rag/Cargo.toml`

```toml
[package]
name = "oz-rag"
version = "0.1.0"
edition = "2021"

[dependencies]
oz-core-types = { path = "../oz-core-types" }
oz-config = { path = "../oz-config" }
oz-llm = { path = "../oz-llm" }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
# 选择: fastembed 或自定义
# fastembed = "3"
# 或: 零依赖自定义嵌入 (HTTP 调用外部嵌入服务)
```

**必须修改**: `Cargo.toml` workspace.members 添加 `"crates/oz-rag"`

#### 2.2 嵌入模型抽象 (`embedder.rs`)

**必须实现**:
- [ ] `Embedder` trait:
  ```rust
  pub trait Embedder: Send + Sync {
      async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>>;
      async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
      fn dimension(&self) -> usize;
      fn model_name(&self) -> &str;
  }
  ```
- [ ] `FastEmbedder` 实现 (如果选择 fastembed)
  - 模型下载到 `~/.openzen/rag/models/`
  - 线程池管理
  - 内存缓存 (避免重复嵌入相同文本)
- [ ] `HttpEmbedder` 实现 (备用: 调用外部嵌入 API)
  - 配置: `embedder_url = "http://localhost:8000/embed"`
  - 用于无法安装 fastembed 的环境
- [ ] `MockEmbedder` 实现 (用于测试)
  - 随机向量, 固定维度
  - 确定性种子, 可重复测试

**测试**:
- [ ] embedder_test.rs: 嵌入维度正确, 批量嵌入, 缓存命中

#### 2.3 文档分块 (`chunker.rs`)

**必须实现**:
- [ ] `Chunker` trait:
  ```rust
  pub trait Chunker: Send + Sync {
      fn chunk(&self, text: &str) -> Vec<Chunk>;
  }
  ```
- [ ] `Chunk` 结构:
  ```rust
  pub struct Chunk {
      pub id: String,        // 格式: "{file_id}:{chunk_idx}"
      pub file_id: String,
      pub chunk_idx: usize,
      pub text: String,
      pub token_count: usize,
      pub metadata: ChunkMetadata,
  }
  ```
- [ ] `RecursiveChunker` 实现:
  - 按层级分割: 段落 → 句子 → 词组
  - `chunk_size = 512` tokens (可配置)
  - `chunk_overlap = 50` tokens (可配置)
  - 使用 `tiktoken` 或自定义 tokenizer
- [ ] `SemanticChunker` 实现 (高级):
  - 根据语义边界分割
  - 使用嵌入相似度判断段落边界
  - 可选, 默认关闭

**测试**:
- [ ] chunker_test.rs: 长文本正确分割, overlap 正确, token 计数准确

#### 2.4 文档加载器 (`loader.rs`)

**必须实现**:
- [ ] `DocumentLoader` trait:
  ```rust
  pub trait DocumentLoader: Send + Sync {
      async fn load(&self, path: &Path) -> anyhow::Result<Document>;
  }
  ```
- [ ] `Document` 结构:
  ```rust
  pub struct Document {
      pub id: String,
      pub path: PathBuf,
      pub title: String,
      pub content: String,
      pub file_type: FileType,
      pub metadata: DocumentMetadata,
  }
  ```
- [ ] 文件类型探测:
  - `.md` → MarkdownLoader (直接读取)
  - `.html` / `.htm` → HtmlLoader (strip tags)
  - `.txt` → TextLoader (直接读取)
  - `.py` / `.rs` / `.ts` / `.js` → CodeLoader (带语言标记)
  - `.pdf` → PdfLoader (调用 `pdftotext` 或 `pdf-extract`)
  - `.docx` → DocxLoader (调用 `docx2txt` 或 `python-docx`)
  - `.xlsx` → SpreadsheetLoader (调用 `calamine`)
- [ ] 配置: `document_loaders.<ext> = "<command>"` (如 `pdf: 'pdftotext $1 -'`)
  - 参考: `docs/roadmap.md` Phase 1.3 的 `document_loaders.<ext>` 配置

**测试**:
- [ ] loader_test.rs: 各格式文件正确加载, 元数据正确

#### 2.5 向量存储 (`store.rs`)

**必须实现**:
- [ ] `VectorStore` trait:
  ```rust
  pub trait VectorStore: Send + Sync {
      async fn upsert(&self, chunks: Vec<ChunkWithEmbedding>) -> anyhow::Result<()>;
      async fn search(&self, query_embedding: &[f32], top_k: usize) -> anyhow::Result<Vec<ScoredChunk>>;
      async fn delete(&self, file_id: &str) -> anyhow::Result<()>;
      async fn list_files(&self) -> anyhow::Result<Vec<String>>;
  }
  ```
- [ ] `SQLiteVectorStore` 实现:
  - SQLite 数据库: `~/.openzen/rag/vector.db`
  - 表结构:
    ```sql
    CREATE TABLE chunks (
        id TEXT PRIMARY KEY,
        file_id TEXT,
        chunk_idx INTEGER,
        text TEXT,
        embedding BLOB,  -- f32 数组序列化为二进制
        token_count INTEGER,
        metadata TEXT    -- JSON
    );
    CREATE INDEX idx_file_id ON chunks(file_id);
    ```
  - 余弦相似度查询 (SQLite 自定义函数)
  - 批量插入 (事务)
  - 自动清理过期 chunk
- [ ] `BM25Index` 实现 (混合搜索):
  - 倒排索引: term → {doc_id, tf, positions}
  - BM25 分数计算
  - 与向量检索结果合并

**测试**:
- [ ] store_test.rs: upsert/search/delete 正确, BM25 索引正确

#### 2.6 检索器 (`retriever.rs`)

**必须实现**:
- [ ] `Retriever` 结构:
  ```rust
  pub struct Retriever {
      embedder: Arc<dyn Embedder>,
      store: Arc<dyn VectorStore>,
      chunker: Arc<dyn Chunker>,
      config: RagConfig,
  }
  ```
- [ ] `retrieve` 方法:
  ```rust
  pub async fn retrieve(&self, query: &str, top_k: usize) -> anyhow::Result<Vec<RetrievedChunk>> {
      // 1. 查询向量化
      let query_emb = self.embedder.embed(query).await?;
      // 2. 向量检索
      let vector_results = self.store.search(&query_emb, top_k * 2).await?;
      // 3. BM25 检索
      let bm25_results = self.store.bm25_search(query, top_k * 2).await?;
      // 4. 混合排序 (向量 0.7 + BM25 0.3)
      let merged = self.hybrid_merge(vector_results, bm25_results, 0.7, 0.3);
      // 5. 重排序 (可选)
      let reranked = if self.config.rerank {
          self.reranker.rerank(query, merged).await?
      } else {
          merged
      };
      // 6. 返回 top_k
      Ok(reranked.into_iter().take(top_k).collect())
  }
  ```
- [ ] `hybrid_merge` 方法:
  - 将向量分数和 BM25 分数归一化到 [0, 1]
  - 加权合并: `score = 0.7 * vector_score + 0.3 * bm25_score`
  - 去重 (相同 chunk_id 只保留最高分)
- [ ] 上下文组装 (`context.rs`):
  ```rust
  pub fn assemble_context(chunks: &[RetrievedChunk], max_tokens: usize) -> String {
      // 按相关度排序, 截断到 max_tokens
      // 格式: [1] chunk_text_1\n[2] chunk_text_2\n...
  }
  ```

**测试**:
- [ ] retriever_test.rs: 混合检索正确, 去重正确, 上下文组装正确

#### 2.7 RAG 查询工具 (`oz-tools/rag_query.rs`)

**参考模式**: `crates/oz-tools/src/file_ops.rs` (ToolHandler trait)

**必须实现**:
- [ ] `RagQueryTool` struct 实现 `ToolHandler` trait
- [ ] 工具定义:
  ```json
  {
    "name": "rag_query",
    "description": "从本地文档知识库中检索相关信息",
    "parameters": {
      "type": "object",
      "properties": {
        "query": {"type": "string", "description": "检索查询"},
        "top_k": {"type": "integer", "default": 5, "description": "返回结果数量"},
        "max_tokens": {"type": "integer", "default": 2000, "description": "上下文最大token数"}
      },
      "required": ["query"]
    }
  }
  ```
- [ ] 工具处理:
  - 调用 `Retriever::retrieve()`
  - 组装上下文
  - 返回 `StepOutcome` with 检索结果 + 引用标记
- [ ] 注册到 `ToolRegistry::build_default()`

**测试**:
- [ ] rag_query_test.rs: 工具正确调用, 返回格式正确

#### 2.8 配置 Schema (`oz-config`)

**参考**: `crates/oz-config/src/mykey.rs`

**必须实现**:
- [ ] `RagConfig` 结构:
  ```rust
  pub struct RagConfig {
      pub enabled: bool,
      pub embedder: EmbedderConfig,
      pub chunker: ChunkerConfig,
      pub retriever: RetrieverConfig,
      pub reranker: Option<RerankerConfig>,
      pub document_loaders: HashMap<String, String>,
  }
  
  pub struct EmbedderConfig {
      pub model: String,      // 默认: "bge-small-zh-v1.5"
      pub cache_path: String, // 默认: "~/.openzen/rag/models"
      pub batch_size: usize,  // 默认: 32
  }
  
  pub struct ChunkerConfig {
      pub chunk_size: usize,  // 默认: 512
      pub chunk_overlap: usize, // 默认: 50
  }
  
  pub struct RetrieverConfig {
      pub top_k: usize,       // 默认: 5
      pub vector_weight: f32, // 默认: 0.7
      pub bm25_weight: f32,   // 默认: 0.3
      pub max_context_tokens: usize, // 默认: 2000
  }
  ```
- [ ] TOML schema:
  ```toml
  [rag]
  enabled = true
  
  [rag.embedder]
  model = "bge-small-zh-v1.5"
  cache_path = "~/.openzen/rag/models"
  batch_size = 32
  
  [rag.chunker]
  chunk_size = 512
  chunk_overlap = 50
  
  [rag.retriever]
  top_k = 5
  vector_weight = 0.7
  bm25_weight = 0.3
  max_context_tokens = 2000
  
  [rag.document_loaders]
  pdf = "pdftotext $1 -"
  docx = "docx2txt $1 -"
  ```

---

### Phase 3：前端集成 (2-4 小时)

#### 3.1 TUI `/rag` 命令

**参考**: `crates/oz-tui/src/command.rs` (现有命令模式)

**必须实现**:
- [ ] `/rag <query>` 子命令
  - 调用 `rag_query` 工具逻辑
  - 显示检索结果 (带引用标记 [1], [2])
  - 显示相关度分数
- [ ] `/rag index <path>` 子命令
  - 将指定路径的文档加入知识库
  - 显示处理进度
- [ ] `/rag list` 子命令
  - 列出已索引的文档
- [ ] `/rag remove <file_id>` 子命令
  - 从知识库中移除文档

**测试**:
- [ ] TUI 启动后 `/rag` 命令正确响应

#### 3.2 WebUI API 端点

**参考**: `crates/oz-server/src/webui/mod.rs` (现有 REST API 模式)

**必须实现**:
- [ ] `POST /api/rag/query`
  - 请求: `{"query": "...", "top_k": 5, "max_tokens": 2000}`
  - 响应: `{"results": [{"text": "...", "score": 0.85, "source": "..."}], "context": "..."}`
- [ ] `POST /api/rag/index`
  - 请求: `{"path": "..."}`
  - 响应: `{"status": "indexed", "chunks": 42}`
- [ ] `GET /api/rag/files`
  - 响应: `{"files": [{"id": "...", "path": "...", "chunks": 42}]}`
- [ ] `DELETE /api/rag/files/:id`
  - 响应: `{"status": "removed"}`

**前端组件**:
- [ ] `RagSearch.svelte` — 搜索框 + 结果列表
- [ ] `RagIndex.svelte` — 文件索引面板
- [ ] 在 `App.svelte` 中集成 (可选: `/rag` 聊天指令)

**测试**:
- [ ] WebUI API 端点正确响应
- [ ] 前端组件渲染正确

---

### Phase 4：测试与验证 (2-4 小时)

#### 4.1 单元测试

**必须完成**:
- [ ] chunker_test.rs (~15 个测试)
  - 长文本分割正确性
  - overlap 计算
  - token 计数
  - 边界情况 (空文本, 单字符, 超长文本)
- [ ] embedder_test.rs (~10 个测试)
  - 嵌入维度
  - 批量嵌入
  - 缓存命中
  - 错误处理
- [ ] store_test.rs (~15 个测试)
  - upsert/search/delete
  - BM25 索引
  - 余弦相似度
  - 并发访问
- [ ] retriever_test.rs (~10 个测试)
  - 混合检索
  - 去重
  - 上下文组装
  - 重排序
- [ ] rag_query_test.rs (~5 个测试)
  - 工具调用
  - 返回格式

#### 4.2 集成测试

**必须完成**:
- [ ] `tests/integration_test.rs`
  - 端到端: 文档加载 → 分块 → 向量化 → 检索 → 上下文
  - 使用测试文档: `tests/fixtures/rag_test_docs/`
  - 测试文档: Markdown, Python 代码, 文本文件
- [ ] TUI E2E 测试
  - `scripts/e2e/tauri_rag_e2e.sh`
  - Agent 调用 rag_query, 显示结果

#### 4.3 性能基准

**必须完成**:
- [ ] 嵌入性能: 1000 个文本块嵌入耗时 < 10 秒
- [ ] 检索性能: 10000 个 chunk 检索 top_k=5 < 100ms
- [ ] 内存占用: 嵌入模型 + 10000 个 chunk < 500MB
- [ ] 二进制体积: `cargo build --release` 增加 < 15MB

---

### Phase 5：文档与清理 (1-2 小时)

#### 5.1 文档

- [ ] 更新 `docs/roadmap.md`: Phase 1.3 标记完成
- [ ] 更新 `docs/acceptance-criteria.md`: RAG 移至完成列表
- [ ] 更新 `docs/comparison-vs-other-agents.md`: P3 RAG 标记完成
- [ ] `docs/adr/0010-rag-architecture.md`: 架构决策记录

#### 5.2 代码质量

- [ ] `cargo clippy -- -D warnings` 零警告
- [ ] `cargo test --workspace` 全部通过
- [ ] 代码审查 (参考 `/code-review` skill)

---

## 任务 2：完整多模态文件附件系统 — 图片/文档上传 + Vision 支持 + 安全 + 前端集成

### 背景

多模态文件附件是 OpenZen 路线图 Phase 2.2 的内容，
已在 `docs/roadmap.md` 中规划但被跳过 (跳过原因: 依赖链较长)。
接受准则中列为 "Future (nice to have)"。

**当前状态**: 无文件上传端点, 无多模态 LLM 支持, 前端无拖拽组件。

### 目标

构建一个完整的文件附件系统，让用户能够:
1. 拖拽/选择文件上传到 Agent 对话
2. 系统自动检测文件类型 (图片, PDF, 代码, 文档)
3. 将图片发送给支持 Vision 的 LLM (Claude, OpenAI)
4. 在对话中显示附件预览
5. 支持文件大小限制和类型白名单
6. 安全防护 (路径遍历, 内容扫描)

### 影响范围

- **修改 oz-server**: 新增上传端点, blob 存储
- **修改 oz-llm**: 扩展消息构建器支持 image_url
- **修改 oz-tools**: 新增 `attach_file` 工具
- **修改 oz-config**: 新增 `[server.attachments]` 配置
- **修改 frontends**: 新增 ChatInput 拖拽, AttachmentPreview 组件
- **修改 src-tauri**: 新增文件选择对话框
- **新增测试**: ~50 个测试用例

---

### Phase 1：研究与架构设计 (2-3 小时)

#### 1.1 外部 API 调研

**必须完成的研究**:
- [ ] OpenAI 多模态 API
  - 参考: `crates/oz-llm/src/openai.rs` (现有消息格式)
  - 研究: `content: [{type: "text", text: "..."}, {type: "image_url", image_url: {url: "data:image/jpeg;base64,..."}}]`
  - 研究: base64 编码大小限制 (20MB)
  - 研究: 支持的图片格式 (JPEG, PNG, GIF, WebP)
  - 研究: 图片尺寸限制 (默认 1024px 缩放)
- [ ] Claude 多模态 API
  - 参考: `crates/oz-llm/src/native_claude.rs`
  - 研究: `content: [{type: "text", text: "..."}, {type: "image", source: {type: "base64", media_type: "image/jpeg", data: "..."}}]`
  - 研究: 图片大小限制 (3.125MB for Claude 3 Haiku, 15MB for Opus)
  - 研究: 最大图片尺寸 (4000x4000)
- [ ] MiniMax / Kimi 多模态
  - 研究: 是否支持 image_url content
  - 参考: `crates/oz-llm/src/stream.rs` (MiniMax 解析)

#### 1.2 架构设计

**必须产出**:
- [ ] `docs/adr/0011-multimodal-attachments.md`
  - 文件存储策略: `~/openzen/blobs/<uuid>.<ext>`
  - 大小限制: 默认 10MB (可配置)
  - 类型白名单: image/*, application/pdf, text/*, application/json
  - 图片预处理: 缩放到最大 1024px (节省 token)
  - 安全: 路径规范化, content-type 验证, 文件头魔数检查
- [ ] 数据流图:
  ```
  用户拖拽文件 → 前端验证 → POST /api/upload → blob 存储
  → 消息附加 attachments → LLM 消息构建 → 多模态推理
  ```

#### 1.3 文件结构

```
crates/oz-server/src/webui/
├── mod.rs          # 新增上传端点
├── blob.rs         # blob 存储管理 (NEW)
└── attachments.rs  # 附件处理 (NEW)

crates/oz-llm/src/
├── message_format.rs  # 扩展: image_url content
├── openai.rs          # 扩展: 多模态消息构建
├── native_claude.rs   # 扩展: 多模态消息构建
└── mixin.rs           # 扩展: 多模态路由

frontends/src/lib/
├── components/
│   ├── ChatInput.svelte         # 修改: 拖拽支持
│   ├── AttachmentPreview.svelte # NEW
│   └── AttachmentList.svelte    # NEW
├── lib/
│   └── api/
│       └── attachments.ts       # NEW: API 封装
└── stores/
    └── chat.ts                  # 修改: attachments 字段
```

---

### Phase 2：后端实现 (4-8 小时)

#### 2.1 Blob 存储 (`oz-server/src/webui/blob.rs`)

**必须实现**:
- [ ] `BlobStore` 结构:
  ```rust
  pub struct BlobStore {
      root: PathBuf,           // ~/.openzen/blobs/
      max_size: u64,           // 默认 10MB
      allowed_types: Vec<String>, // 类型白名单
  }
  ```
- [ ] `store_file` 方法:
  - 生成 UUID 文件名: `{uuid}.{ext}`
  - 验证文件大小
  - 验证 content-type (魔数检查)
  - 保存到磁盘
  - 返回 blob_id
- [ ] `get_file` 方法:
  - 根据 blob_id 返回文件路径
  - 验证文件存在
- [ ] `delete_file` 方法:
  - 删除 blob 文件
  - 清理元数据
- [ ] `list_files` 方法:
  - 列出所有 blob
  - 按创建时间排序

**安全要求**:
- [ ] 路径遍历防护: `canonicalize` + 验证在 `root` 目录内
- [ ] 魔数检查: 文件头验证 (JPEG: FF D8 FF, PNG: 89 50 4E 47, PDF: 25 50 44 46)
- [ ] 大小限制: 上传前检查 Content-Length

#### 2.2 上传端点 (`oz-server/src/webui/mod.rs`)

**参考**: 现有 REST API 模式 (auth, sessions)

**必须实现**:
- [ ] `POST /api/upload` (multipart)
  - 请求: `multipart/form-data` with `file` field
  - 验证: auth token, 文件大小, content-type
  - 调用 `BlobStore::store_file`
  - 响应: `{"blob_id": "...", "filename": "...", "mime_type": "...", "size": 12345}`
- [ ] `GET /api/blobs/:blob_id`
  - 响应: 文件内容 (stream)
  - 用于前端预览图片
- [ ] `DELETE /api/blobs/:blob_id`
  - 响应: `{"status": "deleted"}`
- [ ] `GET /api/blobs`
  - 响应: `{"blobs": [{"blob_id": "...", "filename": "...", "size": 12345, "created_at": "..."}]}`

#### 2.3 LLM 消息构建器扩展 (`oz-llm`)

**参考**: `crates/oz-llm/src/message_format.rs`, `openai.rs`, `native_claude.rs`

**必须实现**:
- [ ] `MessageContent` 枚举:
  ```rust
  pub enum MessageContent {
      Text(String),
      MultiModal(Vec<ContentPart>),
  }
  
  pub struct ContentPart {
      pub content_type: ContentType,
      pub text: Option<String>,
      pub image_url: Option<String>,     // data:image/jpeg;base64,...
      pub file: Option<FileRef>,
  }
  ```
- [ ] OpenAI 消息构建:
  ```rust
  // 在 openai.rs 中扩展
  fn build_multimodal_content(content: &MessageContent) -> serde_json::Value {
      match content {
          MessageContent::Text(t) => json!(t),
          MessageContent::MultiModal(parts) => {
              json!(parts.iter().map(|p| match p.content_type {
                  ContentType::Text => json!({"type": "text", "text": p.text}),
                  ContentType::Image => json!({"type": "image_url", "image_url": {"url": p.image_url}}),
                  ContentType::File => json!({"type": "text", "text": format!("[File: {}]", p.file.name)}),
              }).collect::<Vec<_>>())
          }
      }
  }
  ```
- [ ] Claude 消息构建:
  ```rust
  // 在 native_claude.rs 中扩展
  fn build_multimodal_content(content: &MessageContent) -> serde_json::Value {
      match content {
          MessageContent::Text(t) => json!(t),
          MessageContent::MultiModal(parts) => {
              json!(parts.iter().map(|p| match p.content_type {
                  ContentType::Text => json!({"type": "text", "text": p.text}),
                  ContentType::Image => json!({"type": "image", "source": {"type": "base64", "media_type": p.mime_type, "data": p.base64_data}}),
                  ContentType::File => json!({"type": "text", "text": format!("[File: {}]", p.file.name)}),
              }).collect::<Vec<_>>())
          }
      }
  }
  ```
- [ ] 图片预处理:
  - 缩放到最大 1024px (节省 token)
  - 转换为 JPEG (减少 base64 大小)
  - 使用 `image` crate: `image = "0.25"`

#### 2.4 附件处理 (`oz-server/src/webui/attachments.rs`)

**必须实现**:
- [ ] `AttachmentProcessor` 结构:
  ```rust
  pub struct AttachmentProcessor {
      blob_store: Arc<BlobStore>,
      config: AttachmentConfig,
  }
  ```
- [ ] `process_attachment` 方法:
  - 根据 blob_id 获取文件
  - 检测文件类型
  - 图片: 缩放 + base64 编码
  - 文档: 读取文本内容 (pdftotext 等)
  - 代码: 直接读取
  - 返回 `ContentPart`
- [ ] `prepare_message_content` 方法:
  - 将文本 + 附件合并为 `MessageContent::MultiModal`
  - 验证总 token 数不超过限制

#### 2.5 Agent Loop 集成

**参考**: `crates/oz-core/src/agent_loop.rs` (消息处理)

**必须实现**:
- [ ] 在 `agent_loop.rs` 中:
  - 检查用户消息是否有 `attachments` 字段
  - 调用 `AttachmentProcessor::prepare_message_content`
  - 将多模态内容传递给 LLM session
- [ ] 在消息存储中:
  - `Message` 结构添加 `attachments: Vec<AttachmentRef>` 字段
  - `AttachmentRef { blob_id, filename, mime_type, size }`

#### 2.6 配置 (`oz-config`)

**必须实现**:
- [ ] `AttachmentConfig`:
  ```rust
  pub struct AttachmentConfig {
      pub max_file_size: u64,        // 默认 10MB
      pub max_total_size: u64,       // 默认 50MB
      pub allowed_types: Vec<String>, // 默认: ["image/*", "application/pdf", "text/*"]
      pub image_max_dimension: u32,  // 默认 1024
      pub image_quality: u8,         // 默认 85
  }
  ```
- [ ] TOML schema:
  ```toml
  [server.attachments]
  max_file_size = 10485760  # 10MB
  max_total_size = 52428800  # 50MB
  allowed_types = ["image/*", "application/pdf", "text/*", "application/json"]
  image_max_dimension = 1024
  image_quality = 85
  ```

---

### Phase 3：前端实现 (3-6 小时)

#### 3.1 ChatInput 拖拽支持

**参考**: `frontends/src/lib/components/ChatInput.svelte`

**必须实现**:
- [ ] 拖拽悬停状态:
  - `dragover` 事件 → 显示拖拽遮罩
  - `dragleave` 事件 → 隐藏遮罩
  - 显示: "释放以上传文件" + 文件图标
- [ ] 文件选择按钮:
  - `<input type="file" multiple accept="image/*,.pdf,.md,.txt,.json">`
  - 点击后打开文件选择对话框
- [ ] 文件验证 (前端):
  - 大小检查 (< 10MB)
  - 类型检查 (白名单)
  - 显示错误提示
- [ ] 上传队列:
  - 显示待上传文件列表
  - 显示上传进度
  - 支持取消上传

#### 3.2 AttachmentPreview 组件

**必须实现**:
- [ ] `AttachmentPreview.svelte`:
  - 图片: `<img src="/api/blobs/{blob_id}" />`
  - PDF: 显示图标 + 文件名, 点击下载
  - 代码: 显示文件名 + 语法高亮 (prismjs)
  - 文档: 显示图标 + 文件名
  - 每个附件显示: 预览 + 文件名 + 大小 + 删除按钮
- [ ] 响应式布局:
  - 水平滚动的缩略图栏
  - 最大显示 5 个附件
  - 超过显示 "+N 更多"

#### 3.3 AttachmentList 组件

**必须实现**:
- [ ] `AttachmentList.svelte`:
  - 在聊天消息中显示已发送的附件
  - 与 `ChatMessage.svelte` 集成
  - 显示: 缩略图/图标 + 文件名 + 大小
  - 支持点击预览

#### 3.4 API 封装 (`frontends/src/lib/api/attachments.ts`)

**参考**: `frontends/src/lib/api/chat.ts`

**必须实现**:
- [ ] `uploadFile(file: File): Promise<AttachmentRef>`
  - multipart 上传
  - 返回 blob_id 等信息
- [ ] `getBlobUrl(blobId: string): string`
  - 返回 `/api/blobs/{blob_id}`
- [ ] `deleteBlob(blobId: string): Promise<void>`
- [ ] `listBlobs(): Promise<AttachmentRef[]>`

#### 3.5 Chat Store 修改

**参考**: `frontends/src/lib/stores/chat.ts`

**必须实现**:
- [ ] `AttachmentRef` 类型:
  ```typescript
  export interface AttachmentRef {
    blob_id: string;
    filename: string;
    mime_type: string;
    size: number;
  }
  ```
- [ ] `Message` 类型添加 `attachments?: AttachmentRef[]` 字段
- [ ] `sendMessage` 方法:
  - 检查是否有附件
  - 上传附件 (如果未上传)
  - 在消息中附加 attachments 字段
- [ ] `submitAskUserResponse` 方法:
  - 也支持附件 (用于 ask_user 对话)

---

### Phase 4：测试与验证 (2-4 小时)

#### 4.1 后端测试

**必须完成**:
- [ ] blob_test.rs (~10 个测试)
  - store_file 正确
  - 大小限制
  - 类型白名单
  - 路径遍历防护
  - 魔数检查
  - delete/get/list
- [ ] upload_test.rs (~5 个测试)
  - multipart 上传
  - auth 验证
  - 错误处理
- [ ] message_format_test.rs (~10 个测试)
  - OpenAI 多模态消息构建
  - Claude 多模态消息构建
  - 图片预处理
  - base64 编码
- [ ] attachment_processor_test.rs (~5 个测试)
  - 文件类型检测
  - 图片缩放
  - 文档读取

#### 4.2 前端测试

**必须完成**:
- [ ] AttachmentPreview 组件测试
  - 图片预览渲染
  - 文件图标显示
  - 删除按钮点击
- [ ] ChatInput 拖拽测试
  - 拖拽悬停状态
  - 文件选择
  - 验证错误
- [ ] API 封装测试
  - uploadFile 正确调用
  - getBlobUrl 正确
  - deleteBlob 正确

#### 4.3 安全测试

**必须完成**:
- [ ] 路径遍历攻击测试
  - `../../../etc/passwd` 被拒绝
  - `..%2f..%2f` 被拒绝
- [ ] 文件类型欺骗测试
  - `.exe` 文件被拒绝
  - `.jpg` 但内容为脚本被拒绝 (魔数检查)
- [ ] 大小限制测试
  - 超过 10MB 被拒绝
  - 超过总限制被拒绝

#### 4.4 E2E 测试

**必须完成**:
- [ ] `scripts/e2e/tauri_attachment_e2e.sh`
  - 拖拽图片到聊天框
  - 图片显示在附件预览中
  - 发送消息后, LLM 收到多模态消息
  - 回复正确

---

### Phase 5：文档与清理 (1-2 小时)

#### 5.1 文档

- [ ] 更新 `docs/roadmap.md`: Phase 2.2 标记完成
- [ ] 更新 `docs/acceptance-criteria.md`: 多模态附件移至完成列表
- [ ] `docs/adr/0011-multimodal-attachments.md`: 架构决策记录

#### 5.2 代码质量

- [ ] `cargo clippy -- -D warnings` 零警告
- [ ] `cargo test --workspace` 全部通过
- [ ] `npm run build` 零错误
- [ ] 代码审查 (参考 `/code-review` skill)

---

## 任务难度评估

| 维度 | 任务 1 (RAG) | 任务 2 (多模态) |
|------|-------------|----------------|
| **文件数** | ~30-40 个 | ~25-35 个 |
| **新增代码** | ~3000-4000 行 | ~2000-3000 行 |
| **外部库** | fastembed / tantivy / 自定义 | image crate / 魔数检查 |
| **跨 crate** | 5 个 (oz-rag, oz-tools, oz-config, oz-tui, oz-server) | 4 个 (oz-server, oz-llm, oz-tools, oz-config) |
| **前端复杂度** | 中 (TUI + WebUI API) | 高 (拖拽 + 预览 + 状态管理) |
| **安全要求** | 中 (文件路径) | 高 (上传安全, 内容扫描) |
| **测试难度** | 高 (向量检索正确性) | 中 (上传/预览流程) |
| **估计耗时** | 12-20 小时 | 10-16 小时 |
| **30B 模型建议** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |

## 建议执行顺序

1. **先做任务 1 (RAG)**: 
   - 独立 crate, 风险较低
   - 不涉及安全敏感的上传
   - fastembed 下载可能需要额外时间
   - 为任务 2 的文档处理奠定基础

2. **再做任务 2 (多模态)**:
   - 依赖 RAG 的文档加载器 (PDF, Word 等)
   - 前端拖拽与 RAG 搜索可以共享 UI 模式
   - 多模态 LLM 支持是当前 AI 应用的趋势

## 关键风险点

### 任务 1 风险
- **fastembed 编译体积**: 可能增加 50-100MB 二进制体积
  - 缓解: 使用 `HttpEmbedder` 备用方案 (调用外部嵌入服务)
- **向量存储性能**: 10000 个 chunk 检索可能慢
  - 缓解: SQLite 索引 + 余弦相似度自定义函数
- **模型下载**: 首次运行需要下载嵌入模型 (50-200MB)
  - 缓解: 支持离线模式 (使用 HTTP 嵌入服务)

### 任务 2 风险
- **base64 膨胀**: 图片 base64 编码会 33% 增大
  - 缓解: 图片预处理 (缩放 + 压缩)
- **LLM API 限制**: Claude 图片大小限制 (3-15MB)
  - 缓解: 自动缩放到限制范围内
- **前端拖拽兼容性**: Safari/Tauri WebView 拖拽行为差异
  - 缓解: 提供点击选择备用方案
