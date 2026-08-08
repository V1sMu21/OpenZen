#!/usr/bin/env bash
# 获取并更新所有 omlx 模型到 OpenZen 配置

set -euo pipefail

CONFIG_PATH="$HOME/Documents/apps/openzen/.openzen/mykey.toml"
TEMP_CONFIG=$(mktemp)

echo "=== 获取 omlx 模型列表 ==="
# 从 oMLX 服务器获取可用模型
MODELS_RESPONSE=$(curl -s --connect-timeout 10 http://127.0.0.1:8000/v1/models -H "Authorization: Bearer YOUR_OmlX_API_KEY" 2>/dev/null || echo "[]")
echo "服务器响应: $MODELS_RESPONSE"

# 解析模型名称
echo ""
echo "=== 处理模型列表 ==="
MODEL_NAMES=$(echo "$MODELS_RESPONSE" | python3 -c "import sys, json; data=json.load(sys.stdin); print('\n'.join([m['id'] for m in data.get('data', []) if 'dflash' not in m['id'].lower()]))" 2>/dev/null || echo "")

if [ -z "$MODEL_NAMES" ]; then
    echo "❌ 未获取到模型列表或响应格式不正确"
    exit 1
fi

echo "发现 $([ -z "$MODEL_NAMES" ] && echo 0 || echo $(echo "$MODEL_NAMES" | wc -l)) 个模型（排除 dflash）:"
echo "$MODEL_NAMES"

# 备份现有配置
cp "$CONFIG_PATH" "${CONFIG_PATH}.backup.$(date +%Y%m%d%H%M%S)"

# 读取现有配置内容
echo ""
echo "=== 更新配置文件 ==="
EXCLUDE_KEYS=("agents-a1-8bit")  # 保留已有的 agents-a1-8bit

# 读取现有配置
cat > "$TEMP_CONFIG" << EOF
default_session = "local"

EOF

# 处理现有配置中的其他条目（除 agents-a1-8bit）
while IFS= read -r line; do
    if [[ "$line" =~ ^\[.*\] ]]; then
        key=$(echo "$line" | tr -d '[]')
        if [[ ! " ${EXCLUDE_KEYS[*]} " =~ " $key " ]]; then
            echo -e "\n[$key]" >> "$TEMP_CONFIG"
        fi
    elif [[ "$line" =~ ^[a-z_]+= ]]; then
        echo "$line" >> "$TEMP_CONFIG"
    fi
done < "$CONFIG_PATH"

# 添加新模型配置（保留已有但不排除的）
for model in $MODEL_NAMES; do
    # 跳过已存在的
    if grep -q "model = \"$model\"" "$TEMP_CONFIG"; then
        echo "✓ 已存在: $model"
        continue
    fi
    
    # 生成会话名称（从模型名生成友好名称）
    session_name=$(echo "$model" | sed -e 's/[^a-zA-Z0-9]/_/g' -e 's/^_//' -e 's/_$//')
    if [ "$session_name" = "" ]; then
        session_name="omlx_model_$(echo $model | md5sum | cut -c1-8)"
    fi
    
    # 设置 context_win 基于模型大小（简单启发式）
    if [[ "$model" =~ [1-9][0-9]{2,}B ]]; then
        context_win=256000  # 大模型
    elif [[ "$model" =~ [1-9][0-9]B ]]; then
        context_win=16000   # 中等模型
    else
        context_win=256000  # 默认
    fi
    
    cat >> "$TEMP_CONFIG" << EOF

[$session_name]
apibase = "http://127.0.0.1:8000/v1"
apikey = "YOUR_OmlX_API_KEY"
context_win = $context_win
model = "$model"
EOF
    
    echo "✓ 添加: $session_name -> $model (context: $context_win)"
done

# 检查是否需要更新
if cmp -s "$CONFIG_PATH" "$TEMP_CONFIG"; then
    echo ""
    echo "⚠️  配置文件未发生变化"
else
    # 替换原配置文件
    mv "$TEMP_CONFIG" "$CONFIG_PATH"
    echo ""
    echo "✅ 配置文件已更新：$CONFIG_PATH"
    
    # 重启 Tauri 应用以加载新配置
    echo ""
    echo "=== 重启 Tauri 应用 ==="
    pkill -f "openzen-tauri" || true
    sleep 2
    cd ~/Documents/apps/openzen && cargo tauri dev > /tmp/tauri-final.log 2>&1 &
    echo "Tauri 重启命令已发送..."
    sleep 5
    if pgrep -f "openzen-tauri" >/dev/null; then
        echo "✅ Tauri 应用已重启"
    else
        echo "⚠️ Tauri 应用可能启动失败，请检查日志"
    fi
fi

# 清理临时文件
rm -f "$TEMP_CONFIG"

echo ""
echo "=== 验证 ==="
tail -100 "$CONFIG_PATH"
