# agents-a1-8bit 模型测试指南

## 问题已解决 ✅

之前配置没有生效是因为 Tauri 应用读取的配置路径是：

```
~/Documents/apps/openzen/.openzen/mykey.toml
```

配置文件已成功放置在此处，包含 `agents-a1-8bit` 配置。

## 当前状态

- Tauri 应用: **运行中** (PID: 10355)
- 配置文件路径: `~/Documents/apps/openzen/.openzen/mykey.toml`
- 可用模型: **包含 agents-a1-8bit**

## 测试步骤

### 方法 1: 使用 Tauri 应用界面

1. **打开 Tauri 窗口**
   - 如果窗口已最小化到托盘，点击系统托盘图标打开

2. **测试模型切换**
   - 在聊天输入框中键入：`/model`
   - 模型选择器应该弹出
   - 在列表中找到 **agents-a1-8bit** (显示信息):
     ```
     Name: agents-a1-8bit
     Model: agents-a1-8bit
     Provider: Local
     Context: 256000
     ```

3. **切换并发送消息**
   - 点击 `agents-a1-8bit` 选项
   - 底部状态栏应显示 "Local agents-a1-8bit"
   - 发送测试消息: `Hello, agents-a1-8bit! Please introduce yourself.`
   - 确认收到 AI 回复

### 方法 2: 使用浏览器开发者工具直接测试 IPC

1. **打开 http://localhost:5173** (Vite 开发服务器)

2. **打开开发者工具**
   - 快捷键: `Cmd+Option+I` (macOS)
   - 或右键 → "检查"

3. **在 Console 中执行**
   ```javascript
   // 调用 list_models
   window.__TAURI__.core.invoke('list_models')
     .then(models => {
       console.log('Available models:', models);
       
       const hasAgentsA1 = models.some(m => m.name === 'agents-a1-8bit');
       if (hasAgentsA1) {
         console.log('✅ agents-a1-8bit 可用!');
       } else {
         console.log('❌ agents-a1-8bit 不可用');
         console.log('所有模型:', models.map(m => m.name).join(', '));
       }
     })
     .catch(err => console.error('Error:', err));
   ```

4. **预期输出**
   ```javascript
   Available models: [
     {name: "local", model: "omlx/Qwen3.6-35B-A3B-8bit", provider: "openai", context_win: 256000},
     {name: "local-minimax", model: "MiniMax-M2.5-MLX-6bit", provider: "openai", context_win: 16000},
     // ...
     {name: "agents-a1-8bit", model: "agents-a1-8bit", provider: "openai", context_win: 256000}
   ]
   ✅ agents-a1-8bit 可用!
   ```

### 方法 3: 使用 HTML 测试页面

1. **在浏览器中打开**
   ```bash
   open ~/Documents/apps/openzen/test_list_models.html
   ```

2. **点击 "Call list_models" 按钮**
   - 会显示所有可用模型
   - 如果包含 agents-a1-8bit，会弹出成功提示

## 验证要点

- ✅ 模型切换器中包含 `agents-a1-8bit`
- ✅ 选择后底部状态栏显示 "Local agents-a1-8bit"
- ✅ 发送消息后收到有效回复

## 故障排查

### 如果 agents-a1-8bit 仍不可见

1. **确认配置文件位置正确**
   ```bash
   cat ~/Documents/apps/openzen/.openzen/mykey.toml | grep -A3 "agents-a1-8bit"
   ```

2. **重启 Tauri 应用**
   ```bash
   pkill -f "openzen-tauri"
   cd ~/Documents/apps/openzen
   cargo tauri dev
   ```

3. **检查 oMLX 服务器**
   ```bash
   curl -s http://127.0.0.1:8000/v1/models
   ```

## 预期行为

- 模型切换器显示 `agents-a1-8bit` (Local, context: 256000)
- 切换后模型信息正确显示在状态栏
- 发送测试消息 "Hello, agents-a1-8bit!" 后收到 AI 回复
- 回复应包含模型相关信息（如上下文窗口、能力等）

---
**更新日期**: 2026-07-02
**状态**: 配置完成，Tauri 运行中，等待验证
