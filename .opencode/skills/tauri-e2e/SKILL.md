# Tauri E2E CGEvent + VL Model Test & Debug Skill v1.0

> 生成日期：2026-07-07
> 用途：全自动 Tauri 桌面端 E2E 测试与调试——CGEvent 注入输入 + VL 模型截图验证 + 闭环修复。
> 原则：从注入消息到验证结果到修复代码，全流程可被 AI agent 独立执行。

---

## 一、架构总览

```
┌─────────────────────────────────────────────────────────────┐
│                    AI Agent (OpenCode)                       │
│  1. 读代码/读截图  2. 判断问题  3. 修改代码  4. 重新测试      │
└──────────┬──────────────────────────────────────────────────┘
           │
    ┌──────┴──────┐
    │  CGEvent     │────→ Tauri WKWebView ────→ Agent Loop
    │  注入输入    │      (点击/打字/发送)         (LLM 处理)
    └──────┬──────┘
           │
    ┌──────┴──────┐
    │screencapture │────→ 截图 (1920×1080) ──→ crop to window
    └──────┬──────┘
           │
    ┌──────┴──────┐
    │ VL Model    │────→ 分析截图 ──→ YES/NO verdict
    │Qwen3.6-35B  │
    └──────┬──────┘
           │
           ▼
    PASS → 下一个测试     FAIL → 读代码 → 修复 → 重新测试
```

## 二、前置条件

```bash
# 1. CGEvent Python 脚本（必需）
/tmp/cgclick.py    # CGEvent 鼠标点击（含 hold 参数）
/tmp/cgtype.py     # CGEvent Unicode 键盘输入（绕过 IME）

# 2. Python 依赖
python3 -c "import Quartz; import requests; from PIL import Image"

# 3. VL 模型服务
# endpoint: http://127.0.0.1:8000/v1/chat/completions
# model:    omlx/Qwen3.6-35B-A3B-8bit
# apikey:   YOUR_OmlX_API_KEY

# 4. Tauri 桌面端运行中
pgrep -fl "openzen-tauri"  # 确认进程存在
```

## 三、Tauri 窗口坐标表

> 当前布局 — 窗口位置 (614, 72)，大小 1200×800，macOS 26.5

### 3.1 窗口信息获取

```bash
# 获取窗口位置和大小
osascript -e 'tell application "System Events" to get {position, size} of window 1 of process "openzen-tauri"'
# 返回: 614, 72, 1200, 800

# 获取进程名
osascript -e 'tell application "System Events" to get name of every process whose name contains "openzen"'
# 返回: openzen-tauri
```

### 3.2 屏幕坐标 = 窗口左上角 + 相对偏移

| UI 元素 | 窗口内相对位置 | 屏幕绝对坐标 | 说明 |
|---------|---------------|-------------|------|
| 标题栏（获取焦点） | (286, 8) | (900, 80) | click 60ms hold |
| 侧边栏 "New Chat" 区域 | (150, 160) | (764, 232) | click 80ms hold |
| 聊天文本输入框 | (600, 740) | (1214, 812) | 窗口水平中心，底部 |
| Send 按钮 | (1150, 740) | (1764, 812) | 100ms hold 必须 |
| 侧边面板区域 | (1100, 400) | (1714, 472) | 右边缘附近 |

> ⚠️ 如果窗口位置/大小变化，重新运行窗口信息获取命令更新上表。

### 3.3 坐标计算公式

```python
# 获取当前窗口
import subprocess
result = subprocess.run(["osascript", "-e",
    'tell application "System Events" to get {position, size} of window 1 of process "openzen-tauri"'],
    capture_output=True, text=True)
parts = result.stdout.strip().split(", ")
WIN_X, WIN_Y = int(parts[0]), int(parts[1])
WIN_W, WIN_H = int(parts[2]), int(parts[3])

# 关键屏幕坐标
CHAT_AREA = (WIN_X + WIN_W//2, WIN_Y + WIN_H - 60)      # 文本输入框中心
SEND_BTN  = (WIN_X + WIN_W - 50, WIN_Y + WIN_H - 60)    # Send 按钮
NEW_CHAT  = (WIN_X + 150, WIN_Y + 160)                   # 新建聊天
TITLE_BAR = (WIN_X + WIN_W//3, WIN_Y + 8)                # 标题栏聚焦
```

## 四、核心操作函数

### 4.1 聚焦 Tauri 窗口

```bash
osascript -e 'tell application "openzen-tauri" to activate'
sleep 0.5
# 再点标题栏确保聚焦
python3 /tmp/cgclick.py 900 80 60
```

### 4.2 点击 UI 元素

```python
import subprocess

def tauri_click(rel_x, rel_y, hold_ms=80):
    """点击 Tauri 窗口内的相对位置。"""
    win_x, win_y = 614, 72  # 从 osascript 获取
    screen_x = win_x + rel_x
    screen_y = win_y + rel_y
    subprocess.run(["python3", "/tmp/cgclick.py",
        str(screen_x), str(screen_y), str(hold_ms)])

# 使用示例
tauri_click(150, 160)     # 新建聊天
tauri_click(600, 740)     # 文本输入框
tauri_click(1150, 740, 100)  # Send 按钮（需要 100ms hold）
```

### 4.3 输入文本

```python
def tauri_type(text):
    """向 Tauri 窗口输入文本（CGEvent Unicode 注入）。"""
    # 先切换输入法到 ABC
    subprocess.run(["osascript", "-e",
        'tell application "System Events" to keystroke " " using {command down, control down}'])
    time.sleep(0.3)
    subprocess.run(["python3", "/tmp/cgtype.py", text])

# 必须在点击文本输入框之后调用
tauri_click(600, 740)   # 先聚焦输入框
tauri_type("What is 1+1?")  # 再输入
```

### 4.4 截图并裁剪

```python
from PIL import Image

def tauri_screenshot(name, win_x=614, win_y=72, win_w=1200, win_h=800):
    """截取全桌面，裁剪到 Tauri 窗口，保存为 PNG。"""
    subprocess.run(["screencapture", "-x", "-t", "png", "/tmp/snap.png"])
    img = Image.open("/tmp/snap.png")
    crop = img.crop((win_x, win_y, win_x + win_w, win_y + win_h))
    path = f"docs/test-screenshots/tauri/{name}.png"
    crop.save(path)
    return path
```

### 4.5 VL 模型验证截图

```python
import base64, requests

VL_URL = "http://127.0.0.1:8000/v1/chat/completions"
VL_MODEL = "omlx/Qwen3.6-35B-A3B-8bit"
VL_KEY = "YOUR_OmlX_API_KEY"

def vl_verify(image_path, question, max_tokens=2000):
    """发送截图到 VL 模型，返回 (PASS/FAIL, detail)。"""
    with open(image_path, "rb") as f:
        img_b64 = base64.b64encode(f.read()).decode()

    payload = {
        "model": VL_MODEL,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{img_b64}"}},
                {"type": "text", "text": question}
            ]
        }],
        "max_tokens": max_tokens,
        "temperature": 0
    }

    r = requests.post(VL_URL,
        headers={"Authorization": f"Bearer {VL_KEY}"},
        json=payload, timeout=300)
    content = r.json()["choices"][0]["message"]["content"]

    # 从思考型模型输出中提取 YES/NO
    lines = [l.strip() for l in content.split('\n') if l.strip()]
    for line in reversed(lines):
        upper = line.upper()
        if 'YES' in upper and 'NO' not in upper:
            return ('PASS', line[:120])
        if 'NO' in upper and 'YES' not in upper:
            return ('FAIL', line[:120])

    # 回退：检查最后 300 字符
    tail = content[-300:].upper()
    if 'YES' in tail: return ('PASS', '(inferred)')
    if 'NO' in tail: return ('FAIL', '(inferred)')
    return ('UNKNOWN', content[-100:])

# 使用示例
status, detail = vl_verify("docs/test-screenshots/tauri/TAU-WIN-01_window.png",
    "Is this a working dark-themed OpenZen desktop app? Answer ONLY YES or NO.")
print(f"VL verdict: {status} — {detail}")
```

### 4.6 发送消息（完整流程）

```python
def tauri_send_message(text, wait_seconds=30):
    """完整流程：聚焦窗口 → 新建聊天 → 输入文本 → 点击发送 → 等待 → 截图验证"""
    import time

    # 1. 聚焦
    subprocess.run(["osascript", "-e", 'tell application "openzen-tauri" to activate'])
    time.sleep(0.5)
    tauri_click(286, 8, 60)  # 标题栏聚焦
    time.sleep(0.3)

    # 2. 新建聊天
    tauri_click(150, 160, 80)
    time.sleep(0.5)
    tauri_screenshot("before-send")

    # 3. 输入文本
    tauri_click(600, 740, 80)
    time.sleep(0.3)
    tauri_type(text)
    time.sleep(0.5)
    tauri_screenshot("after-type")

    # 4. 发送（Enter 或点击 Send 按钮）
    subprocess.run(["osascript", "-e", 'tell application "System Events" to keystroke return'])
    time.sleep(0.5)
    # 再点 Send 按钮（双保险）
    tauri_click(1150, 740, 100)
    time.sleep(2)
    tauri_screenshot("after-send")

    # 5. 等待 Agent 响应
    time.sleep(wait_seconds)
    path = tauri_screenshot("after-response")

    # 6. VL 验证
    status, detail = vl_verify(path,
        "Is there an AI agent response visible in this chat app after user asked a question? Answer YES or NO.")
    return status == 'PASS', path
```

## 五、完整 Debug Loop 工作流

```
1. AI Agent 收到 Bug 报告 或 代码变更
        ↓
2. 编译验证：cargo check -p oz-core -p oz-llm -p oz-tools
        ↓
3. Rust 测试：cargo test -p oz-core --lib
        ↓
4. 重启 Tauri（如需要）：
   pkill -f openzen-tauri
   cd /path/to/openzen && cargo tauri dev &
   wait_for_window
        ↓
5. 发送测试消息：tauri_send_message("test input")
        ↓
6. VL 验证截图：vl_verify(screenshot, question)
        ↓
    ┌── PASS ──→ 7. 标记测试通过，继续下一个
    │
    └── FAIL ──→ 8. 读相关代码找根因
                 ↓
                 9. 修复代码
                 ↓
                 10. 返回步骤 2（编译 → 测试 → Tauri → 验证）
```

## 六、常见调试场景

### 6.1 验证 Agent 能否收到消息

```python
tauri_send_message("What is 1+1?")
# VL question: "Is 'What is 1+1?' visible as a user message bubble?"
# Expected: PASS
```

### 6.2 验证 Agent 是否正在处理

```python
tauri_send_message("read Cargo.toml", wait_seconds=5)
# VL question: "Is the agent status showing 'Running' or 'Processing...'?"
# Expected: PASS
```

### 6.3 验证 Checklist Gate

```python
tauri_send_message("create two files: /tmp/a.txt and /tmp/b.txt")
# VL question: "Did the agent create a todowrite checklist before responding?"
# Expected: PASS (agent uses todowrite for complex tasks)
```

### 6.4 验证意图检测

```python
tauri_send_message("read /tmp/test.txt for me")
# VL question: "If the agent said 'I will read' but didn't call tools, 
#              did it continue the loop instead of exiting?"
# Expected: PASS (agent continues, doesn't exit)
```

### 6.5 验证 Side Panel 工件显示

```python
tauri_send_message("create /tmp/hello.html with '<h1>Hello</h1>' and open it in side panel")
# VL question: "Is there a right side panel visible showing hello.html content?"
# Expected: PASS
```

### 6.6 验证窗口管理

```python
# 最小化
subprocess.run(["osascript", "-e",
    'tell application "System Events" to tell process "openzen-tauri" to set value of attribute "AXMinimized" of window 1 to true'])
tauri_screenshot("minimized")
# VL question: "Is the OpenZen window hidden/minimized?"
# Expected: PASS (NO visible window)

# 恢复
subprocess.run(["osascript", "-e",
    'tell application "System Events" to tell process "openzen-tauri" to set value of attribute "AXMinimized" of window 1 to false'])
tauri_screenshot("restored")
# VL question: "Is the OpenZen window visible and restored?"
# Expected: PASS (YES visible)
```

### 6.7 验证 UI 组件渲染

```python
# 截取全窗口
tauri_screenshot("full-window")

# 多问题批量验证
questions = [
    ("TAU-WIN-01", "Dark themed app with sidebar and chat? YES/NO"),
    ("TAU-UI-01",  "Left sidebar visible with project list? YES/NO"),
    ("TAU-SIDEPANEL-01", "Right side panel toggleable? YES/NO"),
]
for tid, q in questions:
    status, detail = vl_verify(f"docs/test-screenshots/tauri/full-window.png", q)
    print(f"{tid}: {status}")
```

## 七、CGEvent 脚本维护

### cgclick.py（放在 /tmp/）

```python
import sys, time, Quartz

def cgclick(x, y, hold_ms=100):
    """CGEvent mousedown + hold + mouseup at screen coords (x, y)."""
    point = Quartz.CGPoint(x, y)
    down = Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventLeftMouseDown, point, 0)
    Quartz.CGEventPost(Quartz.kCGHIDEventTap, down)
    time.sleep(hold_ms / 1000.0)
    up = Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventLeftMouseUp, point, 0)
    Quartz.CGEventPost(Quartz.kCGHIDEventTap, up)

if __name__ == '__main__':
    x, y = int(sys.argv[1]), int(sys.argv[2])
    hold = int(sys.argv[3]) if len(sys.argv) > 3 else 100
    cgclick(x, y, hold)
```

### cgtype.py（放在 /tmp/）

```python
import sys, time, subprocess, Quartz

def cgtype(text):
    """Send Unicode string via CGEvent keyboard events (bypasses IME)."""
    for ch in text:
        event = Quartz.CGEventCreateKeyboardEvent(None, 0, True)
        Quartz.CGEventKeyboardSetUnicodeString(event, len(ch), ch)
        Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)
        time.sleep(0.008)
        event_up = Quartz.CGEventCreateKeyboardEvent(None, 0, False)
        Quartz.CGEventPost(Quartz.kCGHIDEventTap, event_up)
        time.sleep(0.008)

if __name__ == '__main__':
    # Switch input source to ABC
    subprocess.run(['osascript', '-e',
        'tell application "System Events" to keystroke " " using {command down, control down}'],
        timeout=3)
    time.sleep(0.3)
    cgtype(' '.join(sys.argv[1:]))
```

## 八、与测试计划集成

本 skill 覆盖 `docs/tauri-test-plan.md` 中需要 GUI 交互的以下测试组：

| 测试组 | 覆盖方式 |
|--------|---------|
| TAU-WIN | osascript 窗口操作 + VL 验证 |
| TAU-TRAY | screencapture 菜单栏 + VL 验证 |
| TAU-IPC | ipc_integration.rs Rust 测试 |
| TAU-PROJ | ipc_integration.rs Rust 测试 |
| TAU-AGENT | CGEvent 注入消息 + VL 验证 Agent 状态 |
| TAU-APPR | Agent 触发审批 + VL 验证弹窗 |
| TAU-UI | screencapture + VL 验证组件渲染 |
| TAU-SIDEPANEL | screencapture + VL 验证面板状态 |
| TAU-PERF | ps/osascript 进程检查 |

## 九、已知局限

1. **窗口 resize**：AppleScript `set size` 对 Tauri WKWebView 窗口无效，需用 CGEvent 拖拽 resize 角落。
2. **VL 模型思考模式**：该模型是思考型（thinking model），需要设置足够大的 `max_tokens`（建议 2000+），并在输出中搜索最后的 YES/NO。
3. **托盘图标点击**：macOS 菜单栏坐标需手动确定，不同显示器分辨率下不同。
4. **Agent 完成时间**：本地 35B 模型处理消息可能需要 30-120 秒，需合理设置 `wait_seconds`。
5. **多窗口测试**：当前 Tauri 配置不支持多窗口模式，`open_session_window` 功能待实现。
