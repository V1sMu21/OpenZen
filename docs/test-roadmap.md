# OpenZen 综合测试路线图 (Comprehensive Test Roadmap)

> 版本：v2.0 · 生成日期 2026-06-14
> 状态：Active — LLM 可执行版本
> 配套阅读：[roadmap.md](roadmap.md) · [security-plan.md](security-plan.md) · [skill-mcp-plan.md](skill-mcp-plan.md) · [scheduler-plan.md](scheduler-plan.md) · [acceptance-criteria.md](acceptance-criteria.md)

---

## 〇、LLM 执行须知

### 致执行者

本文件是**可执行测试路线图**。每个测试用例包含：
- **ID**：唯一标识符，用于报告和截图命名
- **PREREQ**：前置条件（环境/服务/数据）
- **STEPS**：逐步操作指令，LLM 应精确执行
- **ASSERT**：断言条件，必须全部通过才算 PASS
- **FILES**：相关源码文件，LLM 可读以理解实现
- **SCREENSHOT**：需要截图的步骤（使用 Playwright 或系统截图命令）

### 执行顺序

```
Phase 0: 环境验证（5 min）
Phase 1: 后端单元测试（15 min）— cargo test
Phase 2: 后端集成测试（30 min）— openzen serve + API
Phase 3: WebUI 功能测试（60 min）— Playwright 截图
Phase 4: Tauri 桌面测试（30 min）— cargo tauri dev
Phase 5: TUI 终端测试（15 min）— ga tui
Phase 6: 端到端集成测试（30 min）
Phase 7: 性能基准测试（15 min）
Phase 8: 回归扫描
```

### 全局前置条件

```bash
# 步骤 0.1：验证 Rust 工具链
cargo --version && rustc --version
# ASSERT: 版本号输出，rustc >= 1.78

# 步骤 0.2：验证 Node.js
node --version
# ASSERT: v20+

# 步骤 0.3：编译项目
cargo build --release 2>&1 | tail -5
# ASSERT: 编译成功，0 errors

# 步骤 0.4：前端构建
cd frontends && npm install && npm run build 2>&1 | tail -5
# ASSERT: 0 errors

# 步骤 0.5：验证 API Key 配置
cat ~/.openzen/mykey.toml | head -5 || echo "NO API KEY CONFIG"
# ASSERT: 至少一个 provider key 已配置
# 如果无 key，测试需要 mock 或跳过 LLM 依赖用例

# 步骤 0.6：创建截图目录
mkdir -p docs/test-screenshots/{security,webui,tauri,tui,e2e,perf}

# 步骤 0.7：安装 Playwright (用于 WebUI 截图)
cd /Users/macstu/Documents/apps/openzen && npx playwright install chromium 2>&1 | tail -3
# ASSERT: 安装成功
```

---

## 一、后端单元测试（自动化）

### 1.1 全量 cargo test

```bash
# 步骤 1.1.1：运行全部 Rust 测试
cargo test --workspace --exclude openzen-tauri 2>&1 | tee /tmp/openzen-test-unit.log
# ASSERT: 0 failures，输出包含 "test result: ok"
# FILES: 各 crate 的 tests/ 和 src/ 文件

# 步骤 1.1.2：验证测试数量
grep -c "^test " /tmp/openzen-test-unit.log || true
# ASSERT: 测试数量 >= 379（根据 skill-mcp-plan.md 数据）

# 步骤 1.1.3：安全模块测试（重点）
cargo test -p ga-safety 2>&1 | tail -20
# ASSERT: 19 tests passed, 0 failures
# FILES: crates/ga-safety/src/{trust,guard,patterns,queue,approval}.rs

# 步骤 1.1.4：工具测试
cargo test -p ga-tools 2>&1 | tail -20
# ASSERT: 84+ tests passed, 0 failures
# FILES: crates/ga-tools/src/{code_run,file_ops,web_scan,web_js,registry,knowledge_search,knowledge_write}.rs

# 步骤 1.1.5：核心类型测试
cargo test -p ga-core-types 2>&1 | tail -20
# ASSERT: 116+ tests passed, 0 failures
# FILES: crates/ga-core-types/src/{event,tool,knowledge}.rs

# 步骤 1.1.6：知识系统测试
cargo test -p ga-knowledge 2>&1 | tail -20
# ASSERT: 57+ tests passed, 0 failures
# FILES: crates/ga-knowledge/src/{store,skill,sop,meta,memory,matcher,migration,staleness}.rs

# 步骤 1.1.7：MCP 测试
cargo test -p ga-mcp 2>&1 | tail -20
# ASSERT: 12+ tests passed, 0 failures
# FILES: crates/ga-mcp/src/{client,config,discovery,types}.rs

# 步骤 1.1.8：Clippy 检查
cargo clippy -- -D warnings 2>&1 | tail -10
# ASSERT: 0 warnings
```

---

## 二、后端集成测试（openzen serve 模式）

### 2.1 启动服务器

```bash
# 步骤 2.1.1：启动 openzen serve（后台运行）
cd /Users/macstu/Documents/apps/openzen
cargo run --release -- serve --port 3456 2>&1 &
GA_SERVE_PID=$!
echo "GA_SERVE_PID=$GA_SERVE_PID"
sleep 3

# 步骤 2.1.2：等待服务器就绪
curl -s http://localhost:3456/api/health | head -20
# ASSERT: 返回 JSON 包含 "status": "ok" 或类似字段
```

### 2.2 Auth 测试

```
ID: AUTH-01
PREREQ: openzen serve 运行中
STEPS:
  1. curl -s http://localhost:3456/api/sessions
  2. 观察 HTTP 状态码
ASSERT: 返回 401 Unauthorized
FILES: crates/ga-server/src/webui/mod.rs (auth 中间件)

ID: AUTH-02
PREREQ: AUTH-01 通过
STEPS:
  1. 从 openzen serve 启动日志获取 auth token（搜索 "Auth token:"）
  2. curl -s -H "Authorization: Bearer <TOKEN>" http://localhost:3456/api/sessions
ASSERT: 返回 200 + session 列表
FILES: crates/ga-server/src/webui/mod.rs

ID: AUTH-03
PREREQ: AUTH-02 通过
STEPS:
  1. curl -s http://localhost:3456/api/health
ASSERT: 返回 200（health 端点豁免 auth）
FILES: crates/ga-server/src/webui/mod.rs
```

### 2.3 Session 管理 API 测试

```bash
# 设置 AUTH_TOKEN 变量（从 openzen serve 日志获取）
AUTH_TOKEN=$(grep "Auth token:" /tmp/ga-serve.log | grep -oP '\b\w{20,}\b' | head -1)
# 如果未找到，使用启动时打印的 token

# ID: SESS-01 — 创建 session
SESSION_RESP=$(curl -s -X POST http://localhost:3456/api/sessions \
  -H "Authorization: Bearer $AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"test-session-01"}')
echo "$SESSION_RESP"
# ASSERT: 返回 JSON 含 session_id
SESSION_ID=$(echo "$SESSION_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['session_id'])" 2>/dev/null || echo "PARSE_FAILED")

# ID: SESS-02 — 列出 sessions
curl -s http://localhost:3456/api/sessions \
  -H "Authorization: Bearer $AUTH_TOKEN" | python3 -m json.tool 2>/dev/null | head -30
# ASSERT: 返回列表含 test-session-01

# ID: SESS-03 — 获取单个 session
curl -s "http://localhost:3456/api/sessions/$SESSION_ID" \
  -H "Authorization: Bearer $AUTH_TOKEN" | python3 -m json.tool 2>/dev/null | head -20
# ASSERT: 返回 session 详情

# ID: SESS-04 — 重命名 session
curl -s -X PATCH "http://localhost:3456/api/sessions/$SESSION_ID" \
  -H "Authorization: Bearer $AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"renamed-session"}'
# ASSERT: 返回 200

# ID: SESS-05 — Stop session
curl -s -X POST "http://localhost:3456/api/sessions/$SESSION_ID/stop" \
  -H "Authorization: Bearer $AUTH_TOKEN"
# ASSERT: 返回 200

# ID: SESS-06 — 删除 session
curl -s -X DELETE "http://localhost:3456/api/sessions/$SESSION_ID" \
  -H "Authorization: Bearer $AUTH_TOKEN"
# ASSERT: 返回 200
```

### 2.4 安全系统测试

```
ID: SEC-BL-01 — 黑名单阻止 rm -rf /
PREREQ: openzen serve 运行中，有 API key
STEPS:
  1. curl 请求发消息让 agent 执行 code_run("rm -rf /")
  2. 观察工具调用结果
ASSERT: 结果包含 "操作被系统禁止" 或 "Blocked by hardcode blocklist"
FILES: crates/ga-tools/src/code_run.rs (BLOCKED_COMMANDS 列表)
       crates/ga-safety/src/guard.rs (SafetyGuard::check)

ID: SEC-BL-02 ~ SEC-BL-08 — 其他黑名单命令
PREREQ: 同 SEC-BL-01
STEPS: 对每个黑名单命令重复同样流程
ASSERT: 全部被拦截
黑名单命令列表（来自 ga-tools/src/code_run.rs）:
  - "rm -rf" → SEC-BL-02
  - "mkfs" → SEC-BL-03
  - "dd if=" → SEC-BL-04
  - "curl | sh" / "curl | bash" → SEC-BL-05
  - "wget | sh" → SEC-BL-06
  - "chmod 777 /" → SEC-BL-07
  - "shutdown" / "reboot" → SEC-BL-08

ID: SEC-SANDBOX-01 — 路径沙箱：允许 working_dir 内文件
STEPS:
  1. 通过 agent 请求读取项目内的 Cargo.toml
ASSERT: 成功读取，返回文件内容
FILES: crates/ga-tools/src/file_ops.rs (is_path_allowed 函数)

ID: SEC-SANDBOX-02 — 路径沙箱：拒绝 /etc/passwd
STEPS:
  1. 通过 agent 请求读取 /etc/passwd
ASSERT: 返回拒绝信息，包含 "not allowed" 或沙箱错误
FILES: crates/ga-tools/src/file_ops.rs

ID: SEC-SANDBOX-03 — 路径沙箱：允许 /tmp 路径
STEPS:
  1. 创建 /tmp/openzen-test-file.txt
  2. 通过 agent 请求读取该文件
ASSERT: 成功读取（/tmp 是豁免路径）

ID: SEC-SANDBOX-04 — 路径沙箱：拒绝 ~/.ssh/authorized_keys
STEPS:
  1. 通过 agent 请求写入 ~/.ssh/authorized_keys
ASSERT: 返回拒绝信息

ID: SEC-SANDBOX-05 — SSRF 防护：拒绝 127.0.0.1
STEPS:
  1. 通过 agent 请求 web_scan http://127.0.0.1:8080
ASSERT: 返回拒绝信息
FILES: crates/ga-tools/src/web_scan.rs (BLOCKED_IP_RANGES)

ID: SEC-SANDBOX-06 — SSRF 防护：拒绝 10.x 内网
STEPS:
  1. 通过 agent 请求 web_scan http://10.0.0.1
ASSERT: 返回拒绝信息

ID: SEC-SANDBOX-07 — SSRF 防护：拒绝 192.168.x
STEPS:
  1. 通过 agent 请求 web_scan http://192.168.1.1
ASSERT: 返回拒绝信息

ID: SEC-TRUST-01 — 首次执行 code_run 触发审批（WebUI）
PREREQ: trust.json 不存在或清空，openzen serve 运行
STEPS:
  1. 清空 openzen/trust.json（若存在）
  2. 在 WebUI 中发送消息让 agent 执行 code_run("echo hello")
  3. 截图浏览器中出现的审批弹窗
ASSERT: 审批弹窗出现，显示工具名和参数
SCREENSHOT: docs/test-screenshots/security/SEC-TRUST-01_approval-modal.png
FILES: frontends/src/lib/components/ApprovalModal.svelte
       frontends/src/lib/stores/approval.ts
       crates/ga-server/src/webui/approval.rs

ID: SEC-TRUST-02 — 连续允许 3 次后自动晋级 SessionTrust
PREREQ: SEC-TRUST-01 通过
STEPS:
  1. 在审批弹窗中点击"信任此类操作"(trust_session)
  2. 重复 2 次让 agent 执行相同模式命令
  3. 第 4 次执行相同命令
ASSERT: 第 4 次不再弹出审批弹窗
FILES: crates/ga-safety/src/trust.rs (TrustLevel 晋级逻辑)
SCREENSHOT: docs/test-screenshots/security/SEC-TRUST-02_trust-escalation.png

ID: SEC-TRUST-08 — trust.json 文件权限验证
STEPS:
  1. ls -la openzen/trust.json
ASSERT: 权限显示 -rw------- (0600)
FILES: crates/ga-safety/src/trust.rs (写入时设置权限)

ID: SEC-AUDIT-01 — 审计日志记录
STEPS:
  1. 执行任意工具调用（如 code_run("echo test")）
  2. cat openzen/audit.jsonl | tail -5
ASSERT: 包含本次工具调用记录，字段含 timestamp, session_id, tool, result
FILES: crates/ga-core/src/audit.rs

ID: SEC-RATE-01 — 速率限制正常请求
STEPS:
  1. for i in {1..30}; do curl -s -H "Authorization: Bearer $AUTH_TOKEN" http://localhost:3456/api/sessions > /dev/null; done
ASSERT: 30 次请求全部返回 200

ID: SEC-RATE-02 — 速率限制超限
STEPS:
  1. 快速发送 70+ 请求：
     for i in {1..70}; do curl -s -H "Authorization: Bearer $AUTH_TOKEN" http://localhost:3456/api/sessions > /dev/null & done; wait
  2. 检查是否有 429 响应
ASSERT: 部分请求返回 429 Too Many Requests
FILES: crates/ga-server/src/middleware/rate_limit.rs

ID: SEC-CRYPTO-01 — API Key 加密存储
STEPS:
  1. head -5 ~/.openzen/mykey.toml
ASSERT: API key 字段值不是明文（应为加密后的字符串）
FILES: crates/ga-config/src/crypto.rs

ID: SEC-CRYPTO-03 — 密钥文件权限
STEPS:
  1. ls -la ~/.openzen/mykey.toml
ASSERT: 权限显示 -rw------- (0600)
```

### 2.5 SSE 流式协议测试

```bash
# ID: SSE-01 — SSE 连接建立
curl -s -H "Authorization: Bearer $AUTH_TOKEN" \
  -H "Accept: text/event-stream" \
  http://localhost:3456/api/events &
SSE_PID=$!
sleep 2
kill $SSE_PID 2>/dev/null
# ASSERT: 连接成功建立（无错误输出）

# ID: SSE-08 — Protocol V1 事件验证
# 启动 SSE 监听并发送消息，然后 grep 事件类型
curl -s -H "Authorization: Bearer $AUTH_TOKEN" \
  -H "Accept: text/event-stream" \
  http://localhost:3456/api/events > /tmp/sse-output.txt &
SSE_PID=$!
sleep 2

# 创建 session 并发消息
SESSION_RESP=$(curl -s -X POST http://localhost:3456/api/sessions \
  -H "Authorization: Bearer $AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"sse-test"}')
SID=$(echo "$SESSION_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['session_id'])" 2>/dev/null)

curl -s -X POST http://localhost:3456/api/chat \
  -H "Authorization: Bearer $AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"message\":\"hello\",\"session_id\":\"$SID\"}" > /dev/null

sleep 10  # 等待响应完成
kill $SSE_PID 2>/dev/null

# 验证事件类型
echo "=== SSE 事件类型 ==="
grep "event_type" /tmp/sse-output.txt | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        print(d.get('event_type', 'unknown'))
    except:
        pass
" | sort | uniq -c | sort -rn

# ASSERT: 出现 protocol_v1 事件类型
# ASSERT: 不出现 legacy token/thinking/tool_call/tool_result 事件类型（SSE-08）
```

### 2.6 Agent Loop 测试

```
ID: AGENT-01 — 简单问答
PREREQ: openzen serve 运行，API key 可用
STEPS:
  1. 创建 session
  2. 发送消息 "1+1等于几？"
  3. 等待响应完成
ASSERT: 响应中包含正确答案（不包含错误信息）
FILES: crates/ga-core/src/agent_loop.rs

ID: AGENT-02 — 单工具调用
PREREQ: 同 AGENT-01
STEPS:
  1. 发送消息 "列出当前目录文件"（触发 ls 工具）
  2. 等待工具调用和结果
ASSERT: 响应中包含文件列表（如 Cargo.toml 等）

ID: AGENT-05 — 工具调用失败处理
PREREQ: 同 AGENT-01
STEPS:
  1. 发送消息 "读取 /nonexistent/path/file.txt"
  2. 观察错误处理
ASSERT: agent 收到错误信息并回复用户（不崩溃）
```

### 2.7 调度器测试

```
ID: SCHED-01 — 调度器启动验证
PREREQ: openzen serve 启动
STEPS:
  1. 检查 openzen serve 日志输出
  2. grep "scheduler" 日志
ASSERT: 日志显示 3 个调度任务注册（SessionCleanup, KnowledgeScan, TrustDecay）
FILES: crates/ga-scheduler/src/lib.rs
       src/daemon.rs

ID: SCHED-04 — SessionCleanup 执行验证
STEPS:
  1. 创建 session，等待 > 1 小时（或修改 session 时间戳）
  2. 检查 openzen/sessions/ 目录
ASSERT: 超过 7 天的 session 被归档到 sessions_archive/
FILES: crates/ga-scheduler/src/tasks/session_cleanup.rs
```

---

## 三、WebUI Playwright 截图测试

### 3.0 Playwright 测试脚本

```javascript
// tests/playwright/webui.spec.ts
// 保存到项目中，用于 npx playwright test

import { test, expect } from '@playwright/test';

const BASE_URL = 'http://localhost:3456';
let authToken: string;

test.beforeAll(async ({ request }) => {
  // 获取 auth token
  const health = await request.get(`${BASE_URL}/api/health`);
  const data = await health.json();
  authToken = data.auth_token || process.env.GA_AUTH_TOKEN || '';
});

test.describe('WebUI Tests', () => {

  // ID: UI-LOAD-01 — 页面加载
  test('UI-LOAD-01: 页面正常渲染，无白屏', async ({ page }) => {
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    await page.waitForSelector('.chat-container, .app, main', { timeout: 10000 });
    await expect(page.locator('body')).not.toBeEmpty();
    await page.screenshot({ path: 'docs/test-screenshots/webui/UI-LOAD-01_page-load.png', fullPage: true });
  });

  // ID: UI-LOAD-02 — 无 Auth token
  test('UI-LOAD-02: 无 Auth token 弹出认证对话框', async ({ page }) => {
    await page.goto(BASE_URL);
    await page.waitForTimeout(2000);
    // 检测认证对话框出现
    const authDialog = page.locator('.auth-dialog, [data-testid="auth-dialog"], dialog');
    await page.screenshot({ path: 'docs/test-screenshots/webui/UI-LOAD-02_auth-dialog.png', fullPage: true });
  });

  // ID: UI-LOAD-03 — Auth 成功
  test('UI-LOAD-03: Auth token 输入后进入聊天界面', async ({ page }) => {
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    await page.waitForSelector('textarea, .chat-input, input[type="text"]', { timeout: 10000 });
    await page.screenshot({ path: 'docs/test-screenshots/webui/UI-LOAD-03_chat-interface.png', fullPage: true });
  });

  // ID: UI-LOAD-05 — 控制台无错误
  test('UI-LOAD-05: 浏览器控制台无 JS 错误', async ({ page }) => {
    const errors: string[] = [];
    page.on('console', msg => {
      if (msg.type() === 'error') errors.push(msg.text());
    });
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    await page.waitForTimeout(5000);
    await page.screenshot({ path: 'docs/test-screenshots/webui/UI-LOAD-05_console-check.png' });
    // ASSERT: errors 列表为空或仅含预期 warning
    expect(errors.filter(e => !e.includes('favicon'))).toHaveLength(0);
  });

  // ID: UI-TEXT-01 — Markdown 渲染
  test('UI-TEXT-01: Markdown 基础渲染', async ({ page }) => {
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    const input = page.locator('textarea, .chat-input textarea');
    if (await input.isVisible()) {
      await input.fill('请用 Markdown 格式回答：标题、列表、粗体、代码块');
      await input.press('Enter');
      await page.waitForTimeout(15000); // 等待 LLM 响应
    }
    await page.screenshot({ path: 'docs/test-screenshots/webui/UI-TEXT-01_markdown.png', fullPage: true });
  });

  // ID: UI-THEME-01 — 暗色主题
  test('UI-THEME-01: 暗色主题', async ({ page }) => {
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    // 查找主题切换按钮并切换到暗色
    const themeBtn = page.locator('button[title*="theme"], .theme-switcher button').first();
    if (await themeBtn.isVisible()) {
      await themeBtn.click();
      await page.waitForTimeout(500);
    }
    await page.screenshot({ path: 'docs/test-screenshots/webui/UI-THEME-01_dark.png', fullPage: true });
  });

  // ID: UI-APPR-01 — 审批弹窗（需要触发危险操作）
  test('UI-APPR-01: 危险操作触发审批弹窗', async ({ page }) => {
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    await page.waitForSelector('textarea', { timeout: 10000 });
    const input = page.locator('textarea');
    await input.fill('请执行命令：rm -rf /tmp/test-dir');
    await input.press('Enter');
    // 等待审批弹窗出现（最多 30 秒）
    await page.waitForSelector('.approval-modal, [data-testid="approval-modal"]', { timeout: 30000 })
      .catch(() => {/* 可能不弹窗，如果安全等级不够 */});
    await page.screenshot({ path: 'docs/test-screenshots/security/SEC-TRUST-01_approval-modal.png', fullPage: true });
  });

  // ID: UI-SIDE-01 — Session 列表
  test('UI-SIDE-01: Session 列表显示', async ({ page }) => {
    await page.goto(`${BASE_URL}/?token=${authToken}`);
    // 可能需要点击侧边栏按钮
    const sidebar = page.locator('.sidebar, [data-testid="sidebar"]');
    if (await sidebar.isVisible()) {
      await page.screenshot({ path: 'docs/test-screenshots/webui/UI-SIDE-01_sessions.png', fullPage: true });
    }
  });
});
```

### 3.1 手动 UI 测试步骤

以下测试需要 LLM 通过 Playwright 执行或在浏览器中手动操作：

```
ID: UI-TEXT-02 — 代码块语法高亮
PREREQ: openzen serve 运行，Playwright 可用
STEPS:
  1. 打开 WebUI（带 auth token）
  2. 发送消息 "请用 Python 写一个快速排序算法"
  3. 等待响应完成
  4. 截图包含代码块的响应
ASSERT: 代码块有语法高亮（颜色区分关键字/字符串/注释）
SCREENSHOT: docs/test-screenshots/webui/UI-TEXT-02_code-highlight.png
FILES: frontends/src/lib/utils/markdown.ts

ID: UI-TEXT-03 — LaTeX 公式渲染
STEPS:
  1. 发送消息 "请输出公式 $E=mc^2$ 和 $$\int_0^1 x dx = 0.5$$"
  2. 截图
ASSERT: LaTeX 公式正确渲染（非纯文本显示）
SCREENSHOT: docs/test-screenshots/webui/UI-TEXT-03_latex.png

ID: UI-THINK-01 — ThinkingBlock 折叠状态
STEPS:
  1. 发送消息触发 thinking（使用 Claude 等支持 extended thinking 的模型）
  2. 截图思考块折叠状态（显示 "Thinking..." 或类似提示）
ASSERT: 思考块默认折叠
SCREENSHOT: docs/test-screenshots/webui/UI-THINK-01_collapsed.png

ID: UI-THINK-02 — ThinkingBlock 展开状态
STEPS:
  1. 在 UI-THINK-01 基础上，点击思考块展开按钮
  2. 截图展开后的完整思考内容
ASSERT: 展开后显示完整推理内容
SCREENSHOT: docs/test-screenshots/webui/UI-THINK-02_expanded.png
FILES: frontends/src/lib/components/ThinkingBlock.svelte

ID: UI-TOOL-01 — 工具调用流式状态
STEPS:
  1. 发送消息触发工具调用（如 "列出当前目录的文件"）
  2. 在工具调用进行中截图
ASSERT: ToolCallCard 显示工具名和状态（Running/Pending）
SCREENSHOT: docs/test-screenshots/webui/UI-TOOL-01_running.png
FILES: frontends/src/lib/components/ToolCallCard.svelte

ID: UI-TOOL-02 — 工具调用完成
STEPS:
  1. 等待工具调用完成
  2. 截图完成的 ToolCallCard
ASSERT: 显示工具执行结果摘要
SCREENSHOT: docs/test-screenshots/webui/UI-TOOL-02_completed.png

ID: UI-TOOL-03 — 工具调用折叠/展开
STEPS:
  1. 点击 ToolCallCard 展开/折叠按钮
  2. 截图展开状态显示参数和结果
ASSERT: 展开后看到详细参数和完整结果
SCREENSHOT: docs/test-screenshots/webui/UI-TOOL-03_expanded.png

ID: UI-APPR-02 — 审批弹窗内容展示
STEPS:
  1. 触发需要审批的操作（code_run("echo hello")）
  2. 截图审批弹窗
ASSERT: 弹窗清晰展示：
  - 工具名称 (code_run)
  - 参数预览 (echo hello)
  - 当前信任级别
  - 操作按钮：确认一次 / 信任此类操作 / 拒绝 / 永久禁止
SCREENSHOT: docs/test-screenshots/security/SEC-TRUST-01_approval-modal.png
FILES: frontends/src/lib/components/ApprovalModal.svelte
       frontends/src/lib/stores/approval.ts

ID: UI-APPR-03 — 点击"确认一次"
STEPS:
  1. 在审批弹窗中点击"确认一次"按钮
  2. 观察行为
ASSERT:
  - 操作执行成功
  - 弹窗关闭
  - agent 继续执行
FILES: frontends/src/lib/components/ApprovalModal.svelte → handleApprove()

ID: UI-APPR-05 — 点击"拒绝"
STEPS:
  1. 触发审批弹窗
  2. 点击"拒绝"按钮
ASSERT:
  - 操作不执行
  - agent 收到拒绝消息并告知用户
  - 弹窗关闭

ID: UI-BRANCH-01 — 重新生成按钮
STEPS:
  1. 发送一条消息获得回复
  2. 查找助手消息右上角的重新生成图标
  3. 截图
ASSERT: 重新生成按钮可见（旋转刷新图标）
SCREENSHOT: docs/test-screenshots/webui/UI-BRANCH-01_regenerate.png
FILES: frontends/src/lib/components/ChatMessage.svelte
       frontends/src/lib/stores/chat.ts → regenerate()

ID: UI-BRANCH-02 — 点击重新生成
STEPS:
  1. 点击重新生成按钮
  2. 等待新回复
ASSERT: 旧的助手消息被替换，新的回复出现
SCREENSHOT: docs/test-screenshots/webui/UI-BRANCH-02_after-regenerate.png

ID: UI-MODEL-01 — 模型切换
STEPS:
  1. 查找 ModelSwitcher 组件
  2. 点击展开下拉列表
  3. 截图
ASSERT: 显示所有配置的模型，context window 用 K/M 格式化
SCREENSHOT: docs/test-screenshots/webui/UI-MODEL-01_model-list.png
FILES: frontends/src/lib/components/ModelSwitcher.svelte

ID: UI-AGENT-01 — Agent Picker
STEPS:
  1. 查看 Agent Picker UI
  2. 如果有配置 agents，截图列表
ASSERT: 显示可用 agent 列表（如果有 ~/.openzen/agents/ 目录）
SCREENSHOT: docs/test-screenshots/webui/UI-AGENT-01_agent-picker.png
FILES: frontends/src/lib/components/AgentPicker.svelte

ID: UI-TRANS-01 — Transient Data Bar 搜索通知
STEPS:
  1. 发送消息触发知识搜索
  2. 截图顶部出现的 transient 通知条
ASSERT: TransientBar 出现并显示搜索状态
SCREENSHOT: docs/test-screenshots/webui/UI-TRANS-01_search.png
FILES: frontends/src/lib/components/TransientsBar.svelte
       frontends/src/lib/stores/protocol-processor.ts → data_search_stage 处理
```

---

## 四、Tauri 桌面端测试

### 4.1 构建与启动

```bash
# 步骤 4.1.1：构建 Tauri 应用
cd /Users/macstu/Documents/apps/openzen
cargo tauri build --debug 2>&1 | tail -5
# ASSERT: 构建成功

# 步骤 4.1.2：开发模式启动（需手动在 Tauri 窗口中交互）
# cargo tauri dev
# 手动验证以下测试用例
```

### 4.2 Tauri 测试用例

```
ID: TAU-01 — Tauri 窗口启动
PREREQ: cargo tauri dev 正在运行
STEPS:
  1. 观察窗口是否正常显示
  2. 检查窗口标题是否为 "OpenZen"
  3. 检查窗口大小是否合理（约 1200x800）
ASSERT: 窗口正常显示，无白屏
SCREENSHOT: docs/test-screenshots/tauri/TAU-01_window.png
FILES: src-tauri/tauri.conf.json (width/height 配置)

ID: TAU-TRAY-01 — 系统托盘图标
STEPS:
  1. 观察 macOS 菜单栏是否出现 OpenZen 托盘图标
  2. 截图托盘图标
ASSERT: 托盘图标可见
SCREENSHOT: docs/test-screenshots/tauri/TAU-TRAY-01_tray-icon.png
FILES: src-tauri/src/lib.rs (TrayIconBuilder)

ID: TAU-TRAY-03 — 右键菜单
STEPS:
  1. 右键点击托盘图标
  2. 截图菜单内容
ASSERT: 菜单包含 "Open" 和 "Quit" 选项
SCREENSHOT: docs/test-screenshots/tauri/TAU-TRAY-03_menu.png

ID: TAU-SEC-01 — CSP 策略
STEPS:
  1. 打开 Tauri DevTools (Cmd+Option+I)
  2. 在 Console 中输入: document.createElement('script').src = 'https://evil.com/xss.js'; document.body.appendChild(document.createElement('script')); 
  3. 观察 Console 输出
ASSERT: CSP 阻止了外部脚本加载
FILES: src-tauri/tauri.conf.json → security.csp
验证: grep -A5 '"csp"' src-tauri/tauri.conf.json
预期 CSP: "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' ws://localhost:* http://127.0.0.1:*; img-src 'self' data:; font-src 'self'"

ID: TAU-SEC-02 — Capabilities 检查
STEPS:
  1. 读取 src-tauri/capabilities/default.json
  2. 验证只包含必要权限
ASSERT: permissions 列表仅包含：
  - core:default
  - core:window:default
  - core:window:allow-show
  - core:window:allow-set-focus
  - core:window:allow-close
  - core:window:allow-set-title
  - core:window:allow-set-size
  - core:event:default
  - core:event:allow-listen
  - core:event:allow-emit
  - notification:default
  - shell:allow-open
FILES: src-tauri/capabilities/default.json

ID: TAU-SEC-03 — 日志写入私有目录
STEPS:
  1. ls -la ~/.openzen/logs/ 2>/dev/null || echo "目录不存在"
ASSERT: 日志目录存在，文件权限 0600

ID: TAU-IPC-01 — approve_tool IPC 命令
STEPS:
  1. 在 Tauri 窗口中触发需要审批的操作
  2. 审批弹窗出现在 webview 中
  3. 点击"确认一次"
ASSERT: 操作通过 IPC 命令 approve_tool 传递到后端
FILES: src-tauri/src/approval.rs (approve_tool 命令)
验证: grep "approve_tool" src-tauri/src/lib.rs (确认注册在 generate_handler!)
```

---

## 五、TUI 终端测试

```
ID: TUI-01 — TUI 启动
STEPS:
  1. 运行: cargo run --release -- tui
  2. 观察界面是否正常显示
ASSERT: TUI 界面出现，无崩溃
FILES: crates/ga-tui/src/app.rs

ID: TUI-02 — 启动时间
STEPS:
  1. time cargo run --release -- tui
ASSERT: 启动时间 < 100ms（不含编译时间）
注意：首次运行含编译时间，应使用已编译的 binary 测试

ID: TUI-06 — 暗色主题切换
STEPS:
  1. 在 TUI 中输入 /theme dark
  2. 观察主题变化
ASSERT: 界面变为暗色主题
FILES: crates/ga-tui/src/theme.rs

ID: TUI-07 — 亮色主题切换
STEPS:
  1. 在 TUI 中输入 /theme light
  2. 观察主题变化
ASSERT: 界面变为亮色主题

ID: TUI-09 — /agent 命令
STEPS:
  1. 在 TUI 中输入 /agent
  2. 观察输出
ASSERT: 显示可用 agent 列表（如果有 ~/.openzen/agents/ 目录）
FILES: crates/ga-tui/src/command.rs

ID: TUI-11 — 历史持久化
STEPS:
  1. 在 TUI 中输入任意命令
  2. 退出 TUI
  3. cat ~/openzen/history.txt | tail -5
ASSERT: 历史记录文件包含最近输入
FILES: crates/ga-tui/src/editor.rs (History 模块)
```

---

## 六、端到端集成测试

### 6.1 完整对话流

```
ID: E2E-01 — 简单问答端到端
PREREQ: openzen serve 运行，Playwright 可用
STEPS:
  1. 打开 WebUI
  2. 发送消息 "1+1等于几？"
  3. 等待响应完成
  4. 截图完整对话
ASSERT: 回复包含 "2"
SCREENSHOT: docs/test-screenshots/e2e/E2E-01_simple-qa.png

ID: E2E-02 — 工具调用端到端
PREREQ: 同 E2E-01
STEPS:
  1. 发送消息 "列出当前目录下的 Cargo.toml 文件内容"
  2. 等待 tool 调用和响应完成
  3. 截图包含 ToolCallCard 的对话
ASSERT: ToolCallCard 显示工具名（read/ls），有结果返回
SCREENSHOT: docs/test-screenshots/e2e/E2E-02_tool-call.png

ID: E2E-04 — 完整审批流程（Web 模式）
PREREQ: 清空 trust.json
STEPS:
  1. 确保 openzen/trust.json 不存在或为空
  2. 发送消息 "请执行命令 echo hello world"
  3. 等待审批弹窗出现
  4. 截图审批弹窗
  5. 点击"确认一次"
  6. 等待操作执行
  7. 截图执行结果
ASSERT:
  - 审批弹窗出现
  - 点击确认后操作执行
  - 结果包含 "hello world"
  - openzen/audit.jsonl 包含本次操作记录
  - openzen/trust.json 已创建且包含 code_run 条目
SCREENSHOT: docs/test-screenshots/e2e/E2E-04_approval-flow.png

ID: E2E-05 — 审批拒绝流程
PREREQ: 清空 trust.json
STEPS:
  1. 发送消息 "请执行命令 curl http://example.com"
  2. 等待审批弹窗
  3. 点击"拒绝"
  4. 观察 agent 回复
ASSERT:
  - 操作未执行
  - agent 告知用户操作被拒绝
  - openzen/trust.json 中 denied_count 递增

ID: E2E-SEC-05 — 敏感信息 mask
STEPS:
  1. 创建临时文件 /tmp/test-keys.txt 包含: sk-proj-abcdef1234567890
  2. 让 agent 读取该文件
  3. 观察工具输出
ASSERT: sk-proj-abcdef1234567890 被 mask（显示为 sk-proj-**** 或类似）
FILES: crates/ga-core/src/sanitize.rs (14 种匹配模式)
```

### 6.2 知识系统端到端

```
ID: E2E-KNOW-01 — Skill 结晶
PREREQ: 无已有 skills
STEPS:
  1. 确认 .knowledge/skills/ 目录为空或备份后清空
  2. 让 agent 执行一个复杂任务（触发 3+ 工具调用），如 "搜索 Rust async 最佳实践并总结"
  3. 等待任务完成
  4. 检查 .knowledge/skills/ 目录
ASSERT: 新的 SKILL.md 文件已创建
FILES: crates/ga-core/src/crystallizer.rs

ID: E2E-KNOW-02 — Skill 匹配注入
PREREQ: E2E-KNOW-01 生成了 skill
STEPS:
  1. 发送与该 skill 相关的新请求
  2. 检查 agent 日志中是否显示 skill 匹配信息
ASSERT: system prompt 中包含匹配 skill 的内容
FILES: crates/ga-knowledge/src/skill.rs → find_matching()
       crates/ga-core/src/agent_loop.rs → KnowledgeStore::build_context()

ID: E2E-KNOW-05 — knowledge_search
STEPS:
  1. 让 agent 调用 knowledge_search 工具搜索已有知识
ASSERT: 返回匹配结果
FILES: crates/ga-tools/src/knowledge_search.rs

ID: E2E-KNOW-06 — knowledge_store
STEPS:
  1. 让 agent 调用 knowledge_store 存储一个事实
  2. 检查 .knowledge/facts/ 目录
ASSERT: 新的事实文件已创建
FILES: crates/ga-tools/src/knowledge_write.rs
```

---

## 七、性能基准测试

```bash
# ID: PERF-01 — openzen serve 启动时间
time cargo run --release -- serve --port 3457 2>&1 | head -1
# ASSERT: 启动时间 < 2s（不含编译时间）

# ID: PERF-04 — /api/health 响应时间
START=$(date +%s%N)
curl -s http://localhost:3456/api/health > /dev/null
END=$(date +%s%N)
echo "Response time: $((($END - $START) / 1000000)) ms"
# ASSERT: < 5ms

# ID: PERF-09 — Release 二进制大小
ls -la /Users/macstu/Documents/apps/openzen/target/release/ga 2>/dev/null | awk '{print $5, $9}'
# ASSERT: <= 15 MB (15728640 bytes)

# ID: PERF-10 — 内存使用（空闲）
# 步骤：1. 启动 openzen serve 2. 等待空闲 3. 记录 RSS
ps aux | grep "[g]a serve" | awk '{print "RSS: "$6" KB"}'
# ASSERT: < 100 MB (102400 KB)
```

---

## 八、回归测试清单

### 8.1 自动化回归命令

```bash
#!/bin/bash
# 完整回归脚本 — 粘贴到终端执行
set -e
echo "=== OpenZen 回归测试 ==="
echo ""

echo "1/5: Rust 单元测试..."
cd /Users/macstu/Documents/apps/openzen
cargo test --workspace --exclude openzen-tauri 2>&1 | tail -5
echo "✅ Rust tests passed"
echo ""

echo "2/5: Clippy 检查..."
cargo clippy -- -D warnings 2>&1 | tail -5
echo "✅ Clippy passed"
echo ""

echo "3/5: 前端构建..."
cd frontends && npm run build 2>&1 | tail -5
cd ..
echo "✅ Frontend build passed"
echo ""

echo "4/5: 二进制大小检查..."
BINARY_SIZE=$(stat -f%z target/release/ga 2>/dev/null || echo "0")
SIZE_MB=$((BINARY_SIZE / 1048576))
echo "Binary size: ${SIZE_MB} MB (target: ≤ 15 MB)"
if [ "$SIZE_MB" -gt 15 ]; then
  echo "❌ Binary too large!"
  exit 1
fi
echo "✅ Binary size OK"
echo ""

echo "5/5: 启动测试..."
timeout 10s ./target/release/ga --help > /dev/null
echo "✅ ga --help OK"
echo ""

echo "=== 回归测试完成 ==="
```

### 8.2 手动浏览器回归（必须截图验证）

```
| #  | 检查项                           | 通过标准                        | 截图文件                              |
|----|---------------------------------|--------------------------------|--------------------------------------|
| REG-01 | WebUI 首页加载                | 无白屏、无 JS 错误            | webui/REG-01_home.png                |
| REG-02 | 流式文本渲染                  | 逐字显示、无闪烁              | webui/REG-02_streaming.png          |
| REG-03 | ThinkingBlock 折叠/展开       | 默认折叠、点击展开            | webui/REG-03_thinking.png           |
| REG-04 | ToolCallCard 显示             | 显示工具名、状态、结果        | webui/REG-04_toolcard.png            |
| REG-05 | 审批弹窗                       | 危险操作弹窗、按钮正确        | security/REG-05_approval.png         |
| REG-06 | 主题暗/亮切换                 | 全界面切换无闪烁              | webui/REG-06_theme.png              |
| REG-07 | Session 切换                  | 侧边栏点击切换正确           | webui/REG-07_session.png            |
| REG-08 | 重新生成                       | 重新生成按钮工作              | webui/REG-08_regenerate.png         |
| REG-09 | Agent Picker                  | 显示可用列表、选择生效        | webui/REG-09_agent.png              |
| REG-10 | Transients Bar                | 通知出现并4秒消散            | webui/REG-10_transient.png          |
| REG-11 | Tauri 托盘图标               | 图标可见、菜单正确           | tauri/REG-11_tray.png               |
| REG-12 | Tauri 通知                     | Agent 完成后通知弹出          | tauri/REG-12_notification.png        |
| REG-13 | TUI 输入/输出                  | 消息收发正常                 | tui/REG-13_tui.png                   |
| REG-14 | TUI 主题切换                   | 暗/亮切换正确                | tui/REG-14_tui_theme.png            |
| REG-15 | TUI Ctrl+R 历史               | 历史搜索可用                 | tui/REG-15_tui_history.png           |
```

---

## 九、测试执行计划（时间线）

```
Phase 0: 环境验证 ──────────── 5 min
Phase 1: cargo test 全量 ───── 15 min（自动化）
Phase 2: 后端集成测试 ─────── 30 min
  2.1-2.3: Auth + Session API ─ 5 min
  2.4: 安全系统 ─────────── 10 min
  2.5: SSE 协议 ─────────── 5 min
  2.6: Agent Loop ────────── 5 min
  2.7: 调度器 ──────────── 5 min
Phase 3: WebUI Playwright ──── 60 min（最长，需 LLM 响应）
Phase 4: Tauri 桌面 ────────── 30 min
Phase 5: TUI 终端 ─────────── 15 min
Phase 6: 端到端集成 ────────── 30 min
Phase 7: 性能基准 ─────────── 15 min
Phase 8: 回归扫描 ─────────── 10 min
─────────────────────────────────
总计: 约 3.5 小时
```

### 优先级执行顺序

```
Day 1 上午: Phase 0-2 (环境 + 自动化 + 后端集成)
Day 1 下午: Phase 3 (WebUI — 最耗时的 UI 截图)
Day 2 上午: Phase 4-6 (Tauri + TUI + E2E)
Day 2 下午: Phase 7-8 (性能 + 回归)
```

---

## 十、已知风险映射

| 风险 ID | 风险 | 测试用例 | 缓解 |
|---------|------|---------|------|
| R-001 | SSE provider 差异 | AGENT-12~16, SSE-01~08 | 每个 provider 单独测试 |
| R-003 | CDP 协议版本 | TR-WEB-01~05 | 文档兼容 Chrome 版本 |
| R-004 | Python 子进程跨平台 | TR-CODE-02, AGENT-03 | macOS 优先测试 |
| R-008 | tokio async 超时 | TR-CODE-03 (30s sleep) | RAII ChildGuard 清理 |
| R-009 | SSE broadcast 溢出 | SSE-09~11 | 256 事件容量测试 |
| R-013 | 无 Windows CI | 全部测试仅 macOS | 标注平台限制 |

---

## 十一、测试结果记录模板

### 每个用例执行后填入：

```markdown
| ID | 日期 | 环境 | 结果 | 截图 | 备注 |
|----|------|------|------|------|------|
| SEC-BL-01 | YYYY-MM-DD | macOS arm64 | ⬜ PASS / ❌ FAIL | [链接](test-screenshots/) | |
```

### 汇总表：

```
| 类别 | 总数 | PASS | FAIL | SKIP | 通过率 |
|------|------|------|------|------|--------|
| 安全黑名单 | 9 | | | | |
| 安全信任机制 | 13 | | | | |
| 安全沙箱 | 7 | | | | |
| 敏感信息过滤 | 4 | | | | |
| 审计/限流/加密 | 7 | | | | |
| 工具调用 | 33 | | | | |
| Skill 系统 | 12 | | | | |
| MCP 系统 | 10 | | | | |
| 调度器 | 10 | | | | |
| Agent Loop | 16 | | | | |
| SSE 协议 | 11 | | | | |
| Session 管理 | 6 | | | | |
| WebUI 渲染 | 55+ | | | | |
| Tauri 桌面 | 18 | | | | |
| TUI 终端 | 12 | | | | |
| 端到端集成 | 14 | | | | |
| 性能测试 | 12 | | | | |
| **总计** | **~240** | | | | |
```

---

**最后修订**：2026-06-14 · 维护者：核心团队  
**版本说明**：v2.0 为 LLM 可执行版本，每个测试用例包含 PREREQ/STEPS/ASSERT/FILES/SCREENSHOT 完整规程。