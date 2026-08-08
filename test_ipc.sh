#!/usr/bin/env bash
# 测试 Tauri IPC list_models 调用

set -euo pipefail

echo "=== 测试 Tauri IPC list_models ==="

# 使用 Python 调用 tauri-commands
python3 << 'PYTHON'
import subprocess
import json

# 直接调用 openzen-tauri binary 并测试 IPC (需要 Tauri DevTools)
# 但由于我们无法自动交互，这里使用另一种方法

# 检查配置文件内容
config_path = "~/.openzen/mykey.toml"
print(f"Config file: {config_path}")

# 使用 Python toml 解析配置文件
import toml
try:
    with open(config_path, 'r') as f:
        config = toml.load(f)
    
    print("\n✅ 配置文件解析成功！")
    print(f"Sessions found: {len(config.get('sessions', config))}")
    
    # 注意：实际 toml 文件中，[agents-a1-8bit] 是 top-level table
    # MyKeyConfig.from_file() 需要处理特定的格式
    
    for key, value in config.items():
        if isinstance(value, dict) and 'apibase' in value:
            print(f"  - {key}: model={value.get('model')}")
    
    if 'agents-a1-8bit' in config:
        print("\n✅ agents-a1-8bit 存在于配置中")
    else:
        print("\n❌ agents-a1-8bit 不存在于配置中")

except Exception as e:
    print(f"❌ 错误: {e}")

PYTHON

echo ""
echo "=== Tauri 进程状态 ==="
pgrep -f openzen-tauri && echo "✅ Tauri 运行中" || echo "❌ Tauri 未运行"

echo ""
echo "=== 手动测试方法 ==="
cat << 'EOF'
由于 Tauri 应用需要 GUI 交互，请按以下步骤测试：

1. 打开 http://localhost:5173 (Vite 开发服务器)
2. 按 Cmd+Option+I 打开开发者工具
3. 在 Console 中执行:

   // 调用 tauri invoke list_models
   window.__TAURI__.core.invoke('list_models').then(console.log);

或者：

4. 在应用界面输入 /model
5. 查看模型列表是否包含 agents-a1-8bit

EOF
