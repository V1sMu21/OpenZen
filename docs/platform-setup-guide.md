# 平台渠道接入指南

OpenZen 支持通过 Telegram、飞书、QQ、微信等消息平台使用 AI 助手。
本文档说明如何配置和启用各个平台。

## 前置条件

- OpenZen 已安装并配置了 LLM（`mykey.toml` 中至少配置一个模型）
- 各平台对应的 Bot/应用已创建

## 配置方式

### 方式一：在 mykey.toml 中配置（推荐）

在 `~/.openzen/mykey.toml` 或项目根目录的 `config/mykey.toml` 中添加 `[platforms]` 配置节：

```toml
[platforms.telegram]
enabled = true
bot_token = "你的Bot Token"
allowed_users = []         # 空 = 公开访问, ["123"] = 仅允许指定用户
default_model = "claude_sonnet"

[platforms.feishu]
enabled = true
app_id = "cli_xxxx"
app_secret = "xxxx"
allowed_users = ["*"]      # "*" = 公开访问
default_model = "claude_sonnet"

[platforms.qq]
enabled = false
app_id = "你的App ID"
app_secret = "你的App Secret"
allowed_users = ["*"]

[platforms.wechat]
enabled = false
# 微信通过 QR 码登录，无需预先配置 token
```

### 方式二：通过 OpenZen 对话配置

直接对 OpenZen 说：

> "帮我配置 Telegram 平台接入，我的 bot token 是 xxx"

Agent 会引导你完成配置。

---

## 各平台接入步骤

### 1. Telegram

**准备工作**：
1. 在 Telegram 搜索 `@BotFather`，发送 `/newbot` 创建一个 Bot
2. 复制 BotFather 返回的 HTTP API Token

**配置文件**：
```toml
[platforms.telegram]
enabled = true
bot_token = "1234567890:ABCdefGHIjklMNOpqrsTUVwxyz"
allowed_users = []       # 留空允许所有人使用
default_model = "claude_sonnet"
```

**启动**：重启 OpenZen，Bot 会自动上线。在 Telegram 中搜索你的 Bot 用户名开始对话。

**故障排查**：
- 如果 Bot 无响应，检查 `bot_token` 是否正确
- 如果网络受限，可设置代理：
  ```toml
  proxy = "http://127.0.0.1:7890"
  ```

---

### 2. 飞书（Lark）

**准备工作**：
1. 访问 [飞书开放平台](https://open.feishu.cn/) 创建企业自建应用
2. 在「应用功能」→「机器人」中启用机器人功能
3. 在「权限管理」中添加以下权限：
   - `im:message` — 获取与发送单聊、群组消息
   - `im:message.p2p_msg:readonly` — 读取用户发给机器人的单聊消息
   - `im:message.group_msg:readonly` — 读取群组中机器人消息
   - `im:resource` — 获取与上传图片或文件资源
4. 在「安全设置」中配置 IP 白名单（如不需要可跳过）
5. 在「凭证与基础信息」中获取 **App ID** 和 **App Secret**
6. **发布应用**并等待管理员审批

**配置文件**：
```toml
[platforms.feishu]
enabled = true
app_id = "cli_a1234567890abcdef"
app_secret = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
allowed_users = ["*"]
default_model = "claude_sonnet"
```

**启动**：重启 OpenZen。在飞书中搜索你的应用名称，发送消息即可开始对话。

**故障排查**：
- 确保应用已发布且审批通过
- 检查权限是否包含 `im:message`
- 查看 OpenZen 日志中的 `[feishu]` 前缀信息
- 飞书事件订阅使用的是 WebSocket 长连接模式，无需配置回调 URL

---

### 3. QQ

**准备工作**：
1. 访问 [QQ 开放平台](https://q.qq.com/) 创建机器人应用
2. 在「开发设置」中获取 **BotAppID** 和 **BotSecret**
3. 在「事件订阅」中确保开启以下事件：
   - C2C 消息（私聊）
   - 群 @ 消息

**配置文件**：
```toml
[platforms.qq]
enabled = true
app_id = "你的BotAppID"
app_secret = "你的BotSecret"
allowed_users = ["*"]
default_model = "claude_sonnet"
```

**启动**：重启 OpenZen。

**故障排查**：
- 确认 QQ Bot 后台已开启 C2C 和群 @ 消息事件
- 检查 `app_id` 和 `app_secret` 是否正确
- 延迟重连是正常行为（指数退避 5s → 300s）

---

### 4. 微信

**准备工作**：
- 微信平台适配器使用 **iLink Bot API**，需要微信官方的 Bot 账号

**配置文件**：
```toml
[platforms.wechat]
enabled = true
# 首次启动会弹出 QR 码，用微信扫码登录
default_model = "claude_sonnet"
```

**启动**：
1. 重启 OpenZen
2. 终端会输出 QR 码链接，用微信扫码确认登录
3. 登录成功后 token 保存在 `~/.wxbot/token.json`

**故障排查**：
- 如果 QR 码过期，重启 OpenZen 会生成新的
- 登录状态保存在 `~/.wxbot/token.json`，删除此文件可强制重新登录
- 微信的长轮询超时（30s）后会立即重试，正常现象

---

## 通用问题

### 如何限制使用用户？

在 `allowed_users` 中填写允许的用户 ID：

```toml
# Telegram: 填写数字 User ID
allowed_users = [123456789, 987654321]

# 飞书/QQ: 填写 open_id
allowed_users = ["ou_xxxx", "ou_yyyy"]

# 公开访问（不限制）
allowed_users = ["*"]
```

### 如何切换模型？

在对话中发送 `/llm` 查看可用模型列表，发送 `/llm 1` 切换到第 1 个模型。

### 如何配置代理？

```toml
[platforms.telegram]
enabled = true
bot_token = "..."
proxy = "http://127.0.0.1:7890"
```

目前仅 Telegram 和飞书适配器支持代理设置。

### 日志在哪？

各平台适配器的日志输出到 OpenZen 的标准日志系统（`~/.openzen/logs/openzen.log`），以 `[telegram]`、`[feishu]`、`[qq]`、`[wechat]` 前缀标识。

### 性能说明

- 同一会话同时只能有一个 agent 运行
- 全局最多 3 个并发 agent（跨所有平台）
- 每个平台适配器占用极低资源（~10MB 内存）
