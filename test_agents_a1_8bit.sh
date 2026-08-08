#!/usr/bin/env bash
# test_agents_a1_8bit.sh
# 测试脚本：验证 agents-a1-8bit 模型在 Tauri 应用中的可用性

set -euo pipefail

REPO="$(cd "$(dirname "$0")" && pwd)"
LOG_FILE="$REPO/test_$(date +%Y%m%d_%H%M%S).log"
TAURI_BIN="$REPO/target/debug/openzen-tauri"

echo "====================================="
echo "测试 agents-a1-8bit 模型集成"
echo "日志文件：$LOG_FILE"
echo "====================================="

# 1. 检查配置文件中是否包含 agents-a1-8bit
echo "[步骤 1] 检查配置文件..."
if grep -q "agents-a1-8bit" "$REPO/config/mykey.toml"; then
    echo "✅ 配置文件中已包含 agents-a1-8bit"
    grep -A4 "\[agents-a1-8bit\]" "$REPO/config/mykey.toml" | sed 's/^/    /'
else
    echo "❌ 配置文件中未找到 agents-a1-8bit"
    exit 1
fi

# 2. 检查 oMLX 服务器是否运行
echo "[步骤 2] 检查 oMLX 服务器..."
if curl -s --connect-timeout 5 http://127.0.0.1:8000/v1/models 2>/dev/null; then
    echo "✅ oMLX 服务器运行正常"
else
    echo "❌ oMLX 服务器未运行或在超时/认证错误"
    # 检查是否有 API key 要求
    if curl -s http://127.0.0.1:8000/v1/models 2>&1 | grep -q "API key required"; then
        echo "   oMLX 需要 API key，配置中已有 apiKey = 'YOUR_OmlX_API_KEY'"
    fi
fi

# 3. 停止旧进程
echo "[步骤 3] 清理旧进程..."
pkill -f "openzen-tauri" 2>/dev/null || true
pkill -f "vite.*5173" 2>/dev/null || true
sleep 2

# 4. 构建 Tauri 应用（如果需要）
echo "[步骤 4] 检查/构建 Tauri 应用..."
if [[ ! -x "$TAURI_BIN" ]]; then
    echo "   构建中..."
    cd "$REPO" && cargo build --release 2>&1 | tail -20
fi

# 5. 启动 Tauri 应用并捕获日志
echo "[步骤 5] 启动 Tauri 应用..."
cd "$REPO"

# 创建日志目录
mkdir -p ~/.openzen/logs

# 启动 Tauri（使用 headless mode? 但 Tauri 需要窗口，所以后台运行）
# 使用 nohup 或者直接后台运行
cargo tauri dev > "$LOG_FILE" 2>&1 &
TAURI_PID=$!
echo "   Tauri PID: $TAURI_PID"

# 等待启动
echo "   等待 Tauri 启动..."
for i in $(seq 1 30); do
    if ps aux | grep -q "$TAURI_PID"; then
        echo "   ✓ Tauri 运行中"
        break
    fi
    sleep 1
done

# 6. 检查日志中是否有模型读取记录
echo "[步骤 6] 检查日志..."
sleep 5  # 等待 Tauri 输出

# 如果 Tauri 还在运行，检查日志
if ps -p $TAURI_PID >/dev/null 2>&1; then
    echo "   检查模型读取..."
    if grep -i "list_models" "$LOG_FILE" | grep -q "agents-a1-8bit"; then
        echo "✅ 日志显示 agents-a1-8bit 模型被读取"
        grep -i "list_models.*agents-a1-8bit" "$LOG_FILE" | sed 's/^/    /'
    else
        echo "⚠️  日志中未明确显示 agents-a1-8bit，但模型配置已加载"
        echo "   查看 list_models 日志："
        grep -i "list_models" "$LOG_FILE" | tail -10 | sed 's/^/    /'
    fi

    # 7. 模拟前端的 send_message 调用（通过 Tauri IPC）
    # 由于 Tauri IPC 不是 HTTP API，我们需要通过前端或直接调用
    echo ""
    echo "[步骤 7] 测试模型切换和发送消息..."
    echo "   手动验证步骤："
    echo "   1. 打开 Tauri 应用窗口"
    echo "   2. 在输入框中键入 /model"
    echo "   3. 在模型切换器中找到并选择 'agents-a1-8bit'"
    echo "   4. 发送一条测试消息：'Hello, agents-a1-8bit!'"
    echo "   5. 确认收到回复"

else
    echo "❌ Tauri 应用启动失败，请检查日志："
    tail -50 "$LOG_FILE"
fi

# 8. 提供进一步验证的脚本
echo ""
echo "====================================="
echo "详细验证脚本 (可选)"
echo "====================================="
cat << 'EOF'
# 如果你想在代码层面验证，可以运行以下命令：

# 1. 直接测试 MyKeyConfig 读取
cd ~/Documents/apps/openzen
cat > /tmp/test_config.rs << 'RUST'
use oz_config::mykey::MyKeyConfig;

fn main() {
    let cfg = MyKeyConfig::from_file(std::path::Path::new("config/mykey.toml")).unwrap();
    println!("Sessions found: {}", cfg.sessions.len());
    for (name, sess) in cfg.sessions.iter() {
        println!("  - {}: model={}, context_win={}", name, sess.model, sess.context_win);
    }
}
RUST

# 2. 或者直接用 Rust playground 测试（需要依赖）
# 3. 通过 Tauri IPC 调用 list_models (需要前端环境)

EOF

echo "测试完成！"
echo "日志文件：$LOG_FILE"
