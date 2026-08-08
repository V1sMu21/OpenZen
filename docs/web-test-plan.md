# WebUI 用户体验综合测试计划

> LLM 可执行的真实用户模拟测试计划。覆盖从"新用户第一次打开应用"到"高级用户做复杂多步任务"的完整路径。  
> 每个测试用例包含：用户故事前提、详细操作步骤（用户视角）、**Pass 标准**（可观察 + 可断言）、**失败时排查提示**、**截图要求**、**涉及文件**。

---

## 0. 测试环境基线

### 0.1 启动顺序

```bash
# 1. 启动 ga-server
cd /Users/macstu/Documents/apps/openzen
nohup ./target/release/openzen serve --port 18567 --frontend-dir ./frontends/dist > /tmp/openzen-server.log 2>&1 &

# 2. 启动 vite (开发模式，便于热重载测试前端修改)
cd ./frontends
nohup npm run dev > /tmp/vite.log 2>&1 &

# 3. 验证
curl -s http://localhost:18567/api/health    # 应返回 {"status":"ok",...}
curl -s http://localhost:5173/ | head -1    # 应返回 HTML
```

### 0.2 测试前获取 token

```bash
TOKEN=$(curl -s http://localhost:5173/api/health | python3 -c "import sys,json; print(json.load(sys.stdin)['auth_token'])")
```

### 0.3 通用断言原则

- **零错误**：浏览器 console 不应出现 `error`（`401 加载` 来自首次进入页面时 localStorage 还没写入 token，可忽略）
- **零 effect_update_depth_exceeded**：Svelte 5 effect 循环错误
- **空状态不闪退**：网络断开、token 失效等边界
- **键盘可达**：Tab/Enter 应能在主交互路径上走通

---

## 一、新用户冷启动路径（Test Group 1）

### Test 1.1: 全新会话创建

**用户故事**：张三第一次打开 OpenZen，看到一个空的新聊天页面，键入第一条消息。

**前提**：应用已启动，浏览器无 localStorage 历史。

**步骤**：
1. 打开 `http://localhost:5173/`
2. 等待 3 秒页面完全加载
3. 观察空状态：应显示 OpenZen 标题、欢迎语、`/help` 提示、Enter 提示
4. 在侧边栏点击 `+ New Chat`
5. 观察：应创建一个新会话，侧边栏出现新条目，主区域显示空状态
6. 侧边栏应显示新的会话条目（标题为 "New Chat" 或类似）

**Pass 标准**：
- [ ] 侧边栏出现新会话条目
- [ ] 主区域显示空状态欢迎页
- [ ] 浏览器 console 无 error
- [ ] localStorage 写入 `ga_auth_token` 与 `currentSessionId`

**失败排查**：检查 `/api/sessions` POST 是否成功、是否 401、vite 代理是否正常。

---

### Test 1.2: 首条消息发送

**用户故事**：张三在新会话里输入"你好"并发送，agent 应当回复问候。

**步骤**：
1. 在主区域输入框键入 `你好`
2. 按 Enter 发送
3. 等待 5-15 秒
4. 观察：用户消息出现在右侧、agent 消息出现在左侧

**Pass 标准**：
- [ ] 用户消息气泡在右侧、蓝色或品牌色
- [ ] Agent 消息气泡在左侧、灰色或默认色
- [ ] Agent 回复至少 1 字符
- [ ] 发送按钮在发送后变为 disabled，处理中显示"Processing..."或"Running"
- [ ] 完成后输入框被清空

**截图要求**：显示两条消息的整页截图。

---

### Test 1.3: 多轮对话历史保留

**用户故事**：张三在同一个会话里问了 3 个相关问题。

**步骤**：
1. 发送 "1+1 等于几？" → 等待完成
2. 发送 "那 2+2 呢？" → 等待完成
3. 发送 "3+3？" → 等待完成
4. 刷新页面 → 等待 3 秒
5. 观察历史消息

**Pass 标准**：
- [ ] 三条用户消息 + 三条 agent 消息都在
- [ ] 每条 agent 消息都有"已完成"（Done）徽标
- [ ] 刷新后历史完整保留
- [ ] 侧边栏对应会话的 msg 计数 = 6

---

## 二、连续多轮工具调用（Test Group 2）

### Test 2.1: 单工具单步任务

**用户故事**：用户让 agent 写一个文件。

**步骤**：
1. 发送 `Write a python file to /tmp/test_e2e.py with content: name = 'e2e'`
2. 等待完成
3. 观察：ToolCallCard 显示 Edit 或 Write 工具、参数、Done 状态
4. 验证：ls /tmp/test_e2e.py 存在且内容为 `name = 'e2e'`

**Pass 标准**：
- [ ] ToolCallCard 可见，参数 file_path 正确
- [ ] 状态显示 Done（不是 Running...）
- [ ] Footer 显示 Tools 计数 ≥ 1
- [ ] Token 数字 in/out 均 > 0
- [ ] 文件确实写到了磁盘

---

### Test 2.2: 多工具多步任务

**用户故事**：用户让 agent 写文件、读文件、再做分析。

**步骤**：
1. 发送 `1) write /tmp/multi.py with print('hi') 2) read it back 3) tell me the second character`
2. 等待完成（可能 10-30 秒）
3. 观察：应出现 2 个 ToolCallCard（Edit + Read）+ 最终文本
4. 验证

**Pass 标准**：
- [ ] 至少 2 个 ToolCallCard
- [ ] Tools 总时间 > 各单步时间之和（说明各步是分别计时的）
- [ ] 最终文本正确回答"second character"
- [ ] Footer 显示 Tools N× (N=2+)

---

## 三、BUG #1 验证 — agent 不重复执行历史任务

### Test 3.1: 多轮后不重做历史工具（核心 Bug）

**用户故事**：用户先让 agent 写文件，再问一个完全不相关的问题。Agent 应当只回答新问题，**不要**再调一次 write。

**前提**：会话内已有 1 个写文件的成功工具调用。

**步骤**：
1. 在同一会话内发送 `Do this: 1) write /tmp/bug1_test.py with content: name='world' 2) read it back`
2. 等待完成
3. 发送 `NEW task: just say '4' and nothing else. Do not call any tool.`
4. 等待完成
5. 在 UI 上数 ToolCallCard 数量
6. **关键断言**：第 2 轮（"say 4"）的 agent 消息里**没有**任何 ToolCallCard

**Pass 标准**：
- [ ] 第 2 轮 agent 消息只包含 Thinking 卡片 + 纯文本"4"
- [ ] 第 2 轮 Footer 中 Tools 计数 = 0
- [ ] 整个 session 的 messages 数组中，assistant 消息 [3] 的 tool_input_available 事件数 = 0

**API 断言**（更可靠）：
```bash
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:5173/api/sessions/$SID | \
  python3 -c "
import json, sys
d = json.load(sys.stdin)
for i, m in enumerate(d['messages']):
    if m['role'] != 'assistant': continue
    n = sum(1 for e in m.get('streamEvents',[]) if e['type']=='tool_input_available')
    print(f'  [{i}] tools={n} content={m[\"content\"][:60]!r}')
"
# 期望：第 1 轮 1 tool，第 2 轮 0 tool
```

**失败排查**：
- 浏览器 console 有 `effect_update_depth_exceeded` → Svelte 状态循环
- 第 2 轮仍然有 tool → `mod.rs:reconstruct_assistant_turn` 没走 summary_only 分支
- 系统 prompt 的「多轮防回声」段落丢失 → 检查 `assets/sys_prompt.txt` 是否包含「多轮防回声」

**截图**：`screen-17-test2-bug1.png` 模式

---

### Test 3.2: 旧会话加载后不重做（持久化场景）

**用户故事**：用户关闭应用后再打开，看到历史消息，然后发新问题。Agent 应当基于历史但**不要**重做历史工具。

**步骤**：
1. 在会话 A 中执行 Test 3.1 的步骤 1
2. **关闭浏览器**（不卸载 localStorage）
3. 重新打开 `http://localhost:5173/`
4. 侧边栏点击会话 A
5. 发送 `NEW unrelated task: what is 5+5? one line, no tool.`
6. 验证 agent 没有再调 write/read

**Pass 标准**：同 Test 3.1，且 session 重新加载后历史完整。

---

## 四、BUG #2 验证 — Token 统计正确显示

### Test 4.1: 实时显示 in/out tokens

**用户故事**：用户关注 agent 的成本和上下文使用，footer 应当显示 input/output tokens。

**步骤**：
1. 新建会话，发送任何问题
2. 等待完成
3. 检查每条 agent 消息底部的 footer

**Pass 标准**：
- [ ] 至少有 "X out · Y in" 两个数字显示
- [ ] out 数字 > 0（agent 至少生成了几个 token）
- [ ] in 数字 > 0（输入也至少几百 token）
- [ ] 数字与回复长度大致匹配（如回复 100 字 ≈ 200-300 out tokens）
- [ ] **不能**完全缺失 token 数字（即使老的、不带 token 数据的历史消息也至少要显示 "—" 占位）

**API 断言**：
```bash
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:5173/api/sessions/$SID | \
  python3 -c "
import json, sys
d = json.load(sys.stdin)
for m in d['messages']:
    if m['role']=='assistant':
        print(f'tokensIn={m.get(\"tokensIn\")} tokensOut={m.get(\"tokensOut\")}')
"
```

**修复参考**：`crates/ga-server/src/webui/mod.rs` 中已写入 `tokensIn` / `tokensOut` 到消息；前端 `ChatMessage.svelte` 的 footer 已改为总是渲染 token 区域。

---

### Test 4.2: 长会话的 token 累加合理性

**步骤**：
1. 在新会话连续发 5 条消息（简单问题）
2. 检查每条的 in/out tokens
3. 数值应该逐渐增长（in 因为 history 变长），不应该全是 0 或 0+0

**Pass 标准**：每条都有合理的 in/out 数字。

---

## 五、BUG #3 验证 — 简单任务不调工具

### Test 5.1: 当前时间直接答

**用户故事**：用户问"现在几点"——agent 应当**直接答当前时间**，**不要**调 code_run、web_search 或任何工具。

**步骤**：
1. 新建会话
2. 发送 `What is the current time? Just give me one line, no tool.`
3. 等待完成
4. 观察 ToolCallCard 数量

**Pass 标准**：
- [ ] 没有 ToolCallCard
- [ ] 回复包含当前时间或日期（YYYY-MM-DD HH:MM）
- [ ] 响应时间 < 8 秒

---

### Test 5.2: 路径建议直接答

**用户故事**：用户问"opencode 装在哪里"。

**步骤**：
1. 发送 `Where is opencode installed? Just give me common paths, no tool.`
2. 验证

**Pass 标准**：
- [ ] 没有 ToolCallCard
- [ ] 回复给出常见路径（`~/.local/bin/opencode`、`/opt/homebrew/bin/opencode` 等）
- [ ] 提示用户可以用 `which opencode` 自己确认

---

### Test 5.3: 数学计算直接答

**步骤**：
1. 发送 `What is 15°C in °F? Just compute, no tool.`
2. 验证

**Pass 标准**：
- [ ] 没有 ToolCallCard
- [ ] 回复包含 "59°F" 或类似正确数值（15 × 9/5 + 32 = 59）

---

### Test 5.4: 闲聊直接答

**步骤**：
1. 发送 `Hi, are you there?`
2. 验证

**Pass 标准**：
- [ ] 没有 ToolCallCard
- [ ] 友好的问候回复

---

### Test 5.5: 知识问答直接答

**步骤**：
1. 发送 `Explain Rust's ownership rules in 3 bullet points.`
2. 验证

**Pass 标准**：
- [ ] 没有 ToolCallCard
- [ ] 回复包含三条所有权规则
- [ ] 响应时间 < 8 秒

---

### Test 5.6: 用户已提供内容时直接分析（不重读文件）

**步骤**：
1. 发送一段 python 代码：`def foo(x): return x*2`
2. 接着发送 `What does this function do? Don't re-read any file.`
3. 验证

**Pass 标准**：
- [ ] 第 2 条回复包含 "doubles"、"multiply by 2" 等解释
- [ ] 没有 read 工具调用

---

## 六、BUG #4 验证 — 计时器一致性

### Test 6.1: Tool 卡片上的计时器和最终值一致

**用户故事**：用户在 tool 执行期间看到"运行中"计时器，结束后最终值应当**没有视觉跳变**（即最终值 ≈ 倒数最后一次 live 读数）。

**步骤**：
1. 发送一个会触发慢工具的任务（用 web_search 或多步读文件）
2. 在工具运行时观察 ToolCallCard 上的计时器（应显示 "X.Ys" 实时增长）
3. 等待完成
4. 比较最终值与最后一次看到的 live 值

**Pass 标准**：
- [ ] 工具运行期间计时器持续更新（间隔 ≤ 500ms）
- [ ] 完成后最终值 ≤ 运行期间看到的最后一个 live 值 + 1 秒
- [ ] **不应该**出现"运行中显示 5.0s，完成后跳到 2.7s"这种后退
- [ ] Thinking 卡片的 durationMs 与总时间比例合理

**截图**：执行中 + 完成后的两张截图

---

### Test 6.2: 多步任务的 Tools 累计时间

**步骤**：
1. 发送多步任务（write + read + write）
2. 检查 Footer 的 "Tools X.Xs · peak Y.Ys · 3×"
3. 验证 Tools 总时间 = 各步时间之和（± 100ms 误差）
4. Peak = 最长单步时间

**Pass 标准**：
- [ ] Tools 时间合理
- [ ] Peak 标注为最长单步

---

## 七、SSE 流式稳定性（Test Group 7）

### Test 7.1: 流式传输中事件顺序

**步骤**：
1. 打开 DevTools Network 标签
2. 过滤 `/api/events` 请求
3. 发送任何消息
4. 观察事件流

**Pass 标准**：
- [ ] 连接建立：HTTP 200 + content-type `text/event-stream`
- [ ] 事件顺序：`protocol_v1` 系列（reasoning_start/delta/end, text_start/delta/end, tool_input_start/delta/available, tool_output_available）
- [ ] 最后一个事件是 `done`，包含 `exit_reason`、`input_tokens_est`、`output_tokens_est`

---

### Test 7.2: 长任务不卡死

**步骤**：
1. 发送一个长任务（如 web_search）
2. 在 30 秒内观察 SSE 事件
3. 验证

**Pass 标准**：
- [ ] 事件持续到达（间隔 ≤ 5 秒）
- [ ] 最终 `done` 事件正常触发
- [ ] 30 分钟无响应不触发"Processing timed out"提示

---

### Test 7.3: 页面刷新后 SSE 重连

**步骤**：
1. 启动一个长任务
2. 在任务执行中刷新页面
3. 观察控制台日志

**Pass 标准**：
- [ ] 旧 SSE 连接关闭（不出现 hang）
- [ ] 新 SSE 连接自动建立
- [ ] 任务继续在后台进行（不受页面刷新影响）

---

## 八、会话管理（Test Group 8）

### Test 8.1: 侧边栏会话列表

**前提**：已经有 ≥ 3 个会话。

**步骤**：
1. 检查侧边栏：每个会话显示标题、消息数、最后活跃时间
2. 标题按修改时间倒序排列

**Pass 标准**：
- [ ] 标题不为空
- [ ] 消息数 ≥ 0
- [ ] 时间格式可读
- [ ] 当前活跃会话有高亮

---

### Test 8.2: 切换会话

**步骤**：
1. 在会话 A 中
2. 点击侧边栏的会话 B
3. 观察：主区域切换到 B 的消息
4. 切换到 C，验证

**Pass 标准**：
- [ ] 主区域立即显示新会话的消息
- [ ] 输入框清空
- [ ] URL 不变（应用是 SPA）
- [ ] localStorage 的 `currentSessionId` 更新

---

### Test 8.3: 重命名会话

**步骤**：
1. 在会话标题上 hover
2. 出现重命名入口（可能需要双击或菜单）
3. 改为 "My Test Session"
4. 验证

**Pass 标准**：
- [ ] 标题立即更新
- [ ] 持久化（刷新后保留）

---

### Test 8.4: 删除会话

**步骤**：
1. hover 会话条目，找到删除按钮
2. 点击删除
3. 确认对话框
4. 验证

**Pass 标准**：
- [ ] 会话从侧边栏消失
- [ ] API 返回 200
- [ ] 如果删除的是当前会话，UI 自动切换到另一个会话

---

### Test 8.5: 自动标题

**用户故事**：新会话的标题应当从第一条用户消息自动生成。

**步骤**：
1. 新建会话
2. 发送 `请用 Python 写一个 hello world`
3. 等待完成
4. 检查侧边栏会话标题

**Pass 标准**：
- [ ] 标题以用户消息前 28 字符生成
- [ ] 标题不含 `/` 开头（避免误判为命令）

---

## 九、输入与命令（Test Group 9）

### Test 9.1: /help 命令

**步骤**：
1. 新建会话
2. 在输入框键入 `/help`
3. 按 Enter
4. 观察

**Pass 标准**：
- [ ] 显示可用命令列表
- [ ] 或者弹出自动补全菜单
- [ ] 没有触发 LLM 调用

---

### Test 9.2: 斜杠命令自动补全

**步骤**：
1. 键入 `/`
2. 观察自动补全菜单出现
3. 键入 `/h` 过滤
4. 验证

**Pass 标准**：
- [ ] 自动补全弹出
- [ ] 过滤有效
- [ ] Esc 关闭

---

### Test 9.3: 多行输入

**步骤**：
1. 键入第一行
2. Shift+Enter 换行
3. 键入第二行
4. 验证

**Pass 标准**：
- [ ] Shift+Enter 插入换行
- [ ] Enter（无 Shift）发送
- [ ] 多行消息被正确发送

---

### Test 9.4: 长消息滚动

**步骤**：
1. 粘贴 1000 字符的文本
2. 发送
3. 观察输入框行为

**Pass 标准**：
- [ ] 输入框不卡死
- [ ] 消息正确发送
- [ ] 消息气泡在 UI 中可滚动查看

---

## 十、错误与边界（Test Group 10）

### Test 10.1: 工具执行失败的处理

**步骤**：
1. 发送一个会失败的任务（如读不存在的文件：`read /nonexistent/file`）
2. 观察 ToolCallCard
3. 验证 agent 行为

**Pass 标准**：
- [ ] ToolCallCard 显示 Error 状态
- [ ] Agent 能优雅地报告错误
- [ ] 用户消息底部显示 exit_reason 或 "stopped"

---

### Test 10.2: 用户主动停止

**步骤**：
1. 启动一个长任务（web_search）
2. 在 5 秒内点停止按钮（红色方形按钮）
3. 验证

**Pass 标准**：
- [ ] 任务立即停止
- [ ] 后台日志显示 "stop signal received"
- [ ] 前端 ToolCallCard 从 Running 切到 Done（错误）
- [ ] 用户的"停止"消息能成功发出

---

### Test 10.3: 网络断开恢复

**步骤**：
1. 启动长任务
2. 在浏览器 DevTools → Network → Offline
3. 5 秒后恢复网络
4. 观察

**Pass 标准**：
- [ ] SSE 断线时不丢消息（重新连接后能恢复）
- [ ] 错误横幅提示网络问题（不静默失败）

---

### Test 10.4: 服务端崩溃恢复

**步骤**：
1. 启动任务
2. 在另一个终端 kill ga-server
3. 重新启动 ga-server
4. 观察 UI

**Pass 标准**：
- [ ] UI 显示"连接断开"或类似提示
- [ ] 重启后能重新建立 SSE
- [ ] 当前任务不丢（后端恢复后继续）

---

### Test 10.5: Token 失效（401）

**步骤**：
1. 在浏览器 DevTools → Application → Local Storage 改坏 token
2. 刷新页面
3. 观察

**Pass 标准**：
- [ ] 自动重新获取 token（/api/health 返回的）
- [ ] 或者显示 AuthDialog 让用户输入新 token
- [ ] 不出现白屏

---

## 十一、布局与视觉（Test Group 11）

### Test 11.1: 窗口缩放

**步骤**：
1. 拖动浏览器窗口从 1440×900 到 800×600
2. 观察布局
3. 拖到 1920×1080

**Pass 标准**：
- [ ] 侧边栏可折叠
- [ ] 消息气泡宽度自适应
- [ ] ToolCallCard 不溢出
- [ ] 输入框固定底部

---

### Test 11.2: 移动端布局

**步骤**：
1. DevTools → 切换到 iPhone 视图
2. 观察布局

**Pass 标准**：
- [ ] 侧边栏默认隐藏，有 hamburger 按钮
- [ ] 消息气泡占满宽度（max-width: 100%）
- [ ] 输入框不溢出

---

### Test 11.3: 长时间运行的 session（消息数多）

**步骤**：
1. 在一个会话里连续发 20 条消息
2. 滚动到底部
3. 观察

**Pass 标准**：
- [ ] 滚动流畅（不应卡顿）
- [ ] 自动滚动到最新消息
- [ ] 旧消息渲染正确
- [ ] 输入框始终可见

---

## 十二、回归测试（Test Group 12）

### Test 12.1: 之前的测试都能通过

**步骤**：依次执行 Test 1-11 中所有子项。

**Pass 标准**：所有项目标 ✓。

---

## 附录 A：通用失败排查速查

| 症状 | 优先排查 |
|---|---|
| 浏览器 console 有 `effect_update_depth_exceeded` | Svelte 5 state cycle，搜索 `let xxx = $state` 是否被 `$effect` 读写 |
| Token 数字全是 0 | 检查 `crates/ga-server/src/webui/mod.rs` 是否写入 `tokensIn`/`tokensOut` |
| Tool card 卡在 "Running..." | 检查协议 v1 事件是否带 `duration_ms` 后端字段 |
| Bug #1 复现：每轮都重做 | 检查 `mod.rs:reconstruct_assistant_turn` 是否 `summary_only=true`；系统 prompt 是否含「多轮防回声」 |
| 简单问题调用工具 | 检查 `assets/sys_prompt.txt` 是否含「直接回答优先」+「多轮防回声」 |
| 计时器跳变 | 检查 `ToolCallCard.svelte` 的 `liveRunningMs` 是否有 `argsSettled` + `durationMs` snap 逻辑 |

## 附录 B：涉及的修复文件

| Bug | 文件 | 修复点 |
|---|---|---|
| #1 re-execution | `crates/ga-server/src/webui/mod.rs` | `reconstruct_assistant_turn(..., summary_only=true)`：上一轮的 tool_use/result 替换为 `[System note: ...]` 的 User 消息，阻止 LLM 回声 |
| #1 re-execution | `assets/sys_prompt.txt` / `assets/sys_prompt_en.txt` | 新增「多轮防回声（自动，agent 内部规则）」段，强制 LLM 不重复执行 |
| #2 token display | `crates/ga-server/src/webui/mod.rs` | 已经在 done event 中发送 `input_tokens_est` / `output_tokens_est`；消息存档时写入 `tokensIn`/`tokensOut` |
| #2 token display | `frontends/src/lib/components/ChatMessage.svelte` | Footer 改为总是渲染 token 区域（即使旧消息也显示 "—"），同时显示 in/out |
| #2 token display | `frontends/src/lib/stores/chat.ts` | 删除 `partArrivalTimes.push()` 误用（Map 被当 Array 用），改为 `.set()` + `partArrivalOrder.push()`；删除 `tokensIn ?? (event.data as any).tokens_in` 这条 dead-code fallback |
| #3 simple tasks | `assets/sys_prompt.txt` / `assets/sys_prompt_en.txt` | 「直接回答优先」段前移到 prompt 顶部，列出具体反例（"现在几点"、"opencode 装在哪" 等） |
| #4 timer | `frontends/src/lib/components/ToolCallCard.svelte` | 添加 `argsSettled` 状态：args 文本稳定 400ms 后才启动 live 计时器（对齐 `tool_input_available` 时刻）；完成时 `liveRunningMs = durationMs` 防止跳变 |

## 附录 C：截图清单

| 文件 | 用途 |
|---|---|
| `screen-01-initial.png` | 初始页面状态 |
| `screen-04-bugs-fixed.png` | Test 3.1 验证（不重做）|
| `screen-15-final-fix.png` | v4 修复后的多轮验证 |
| `screen-16-test1-multistep.png` | 多步任务 |
| `screen-17-test2-bug1.png` | Bug #1 核心验证 |
| `screen-18-test3-bug3.png` | Bug #3 简单任务 |
| `screen-19-tokens.png` | Token 显示 |
