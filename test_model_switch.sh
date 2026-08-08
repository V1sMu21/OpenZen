#!/usr/bin/env bash
# test_model_switch.sh - 测试 agents-a1-8bit 模型切换功能

set -euo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
LOG_FILE="$REPO/model_switch_$(date +%Y%m%d_%H%M%S).log"
SCREENS="/tmp/tauri-model-switch"
CGTYPE="/tmp/cgtype.py"
CGCLICK="/tmp/cgclick.py"

mkdir -p "$SCREENS"

echo "====================================="
echo "测试 agents-a1-8bit 模型切换功能"
echo "日志：$LOG_FILE"
echo "截图：$SCREENS/"
echo "====================================="

# 确保 Tauri 应用正在运行
echo "[步骤 1] 确认 Tauri 应用状态..."
if pgrep -f "openzen-tauri" > /dev/null; then
    echo "✅ Tauri 应用已运行"
else
    echo "❌ Tauri 应用未运行，启动中..."
    cd "$REPO" && bash scripts/tauri-dev.sh > "$LOG_FILE" 2>&1 &
    sleep 10
fi

# 检查配置文件
echo "[步骤 2] 验证模型配置..."
if grep -q "agents-a1-8bit" "$REPO/config/mykey.toml"; then
    echo "✅ agents-a1-8bit 存在于配置中"
else
    echo "❌ agents-a1-8bit 不在配置中"
    exit 1
fi

# 手动验证步骤（由于自动化复杂度高）
echo ""
echo "[步骤 3] 手动验证流程"
echo "====================================="
cat << 'EOF'
请按以下步骤操作：

1. 确保 Tauri 窗口在前台可见
2. 在聊天输入框中键入：/model
3. 等待模型切换器出现
4. 在列表中找到 "agents-a1-8bit"（应该包含以下信息）：
   - Name: agents-a1-8bit
   - Model: agents-a1-8bit
   - Provider: Local
   - Context: 256000
5. 点击该选项进行切换
6. 观察输入框上方是否显示 "Local agents-a1-8bit" 标签
7. 发送一条测试消息："Hello, agents-a1-8bit! Can you introduce yourself?"
8. 确认收到回复

验证要点：
- 模型切换器中应包含 agents-a1-8bit 选项
- 切换后模型信息正确显示
- 发送消息成功
- 收到有效回复（提及自身能力、上下文窗口等）

截图建议：
- 模型切换器打开时的截图
- 消息发送后的回复截图

EOF
echo "====================================="
echo ""

# 如果用户想自动验证，可以使用 E2E 脚本
echo "[步骤 4] 自动化验证（可选）"
echo "要使用自动化脚本，需要先设置 E2E 环境："
cat << 'EOF'

# 1. 创建 Python venv (一次性的)
python3.12 -m venv /tmp/e2e_venv
/tmp/e2e_venv/bin/pip install pyobjc-framework-Quartz pillow

# 2. 创建 /tmp/cgtype.py (使用 Quartz CGEvent)
cat > /tmp/cgtype.py << 'PY'
import sys
from Quartz.CoreGraphics import (
    CGEventCreateKeyboardEvent,
    CGEventKeyboardSetUnicodeString,
    CGEventPostToSession,
    kCGSessionSessionWideID
)

def type_text(text):
    for char in text:
        # Create key down event with unicode
        down = CGEventCreateKeyboardEvent(None, 0, True)
        CGEventKeyboardSetUnicodeString(down, len(char), char)
        CGEventPostToSession(kCGSessionSessionWideID, down)
        
        # Create key up event
        up = CGEventCreateKeyboardEvent(None, 0, False)
        CGEventPostToSession(kCGSessionSessionWideID, up)

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: cgtype.py 'text to type'")
    else:
        type_text(sys.argv[1])
PY

# 3. 创建 /tmp/cgclick.py (模拟鼠标点击)
cat > /tmp/cgclick.py << 'PY'
import sys
from Quartz.CoreGraphics import (
    CGEventCreateMouseEvent,
    CGEventPostToSession,
    CGEventSetIntegerValueField,
    kCGSessionSessionWideID,
    kCGMouseEventDragPosition,
    kCGLeftMouseUp,
    kCGLeftMouseDown
)

if len(sys.argv) < 4:
    print("Usage: cgclick.py x y duration_ms")
else:
    x, y = int(sys.argv[1]), int(sys.argv[2])
    duration = int(sys.argv[3])
    
    # Mouse down
    event_down = CGEventCreateMouseEvent(None, kCGMouseDown, (x, y), 0)
    CGEventSetIntegerValueField(event_down, kCGMouseEventDragPosition, x << 16 | y)
    CGEventPostToSession(kCGSessionSessionWideID, event_down)
    
    # Wait
    import time
    time.sleep(duration / 1000.0)
    
    # Mouse up
    event_up = CGEventCreateMouseEvent(None, kCGMouseUp, (x, y), 0)
    CGEventSetIntegerValueField(event_up, kCGMouseEventDragPosition, x << 16 | y)
    CGEventPostToSession(kCGSessionSessionWideID, event_up)
PY

# 4. 运行自动化脚本（参考 tauri_ask_user_e2e.sh）
# bash scripts/e2e/tauri_ask_user_e2e.sh

EOF

echo ""
echo "手动验证完成后，检查应用日志以确认模型加载："
echo "tail -f ~/.openzen/logs/openzen.log"
echo ""

# 等待用户完成手动验证（可选）
read -p "按 Enter 键继续检查日志..."

# 检查 Tauri 日志
echo ""
echo "[步骤 5] 检查 Tauri 应用日志..."
LOG_PATH="$HOME/Documents/apps/openzen/.openzen/logs/openzen.log"
if [[ -f "$LOG_PATH" ]]; then
    echo "=== 最新日志 ==="
    tail -50 "$LOG_PATH" | grep -i "model\|session" || echo "没有找到模型相关日志"
else
    echo "⚠️  Tauri 日志文件不存在：$LOG_PATH"
fi

echo ""
echo "测试完成！"
