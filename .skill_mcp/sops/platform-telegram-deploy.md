🔴 Execute: `./target/debug/openzen platform add telegram --bot-token "TOKEN" --model Agents_A1_8bit` → done.

# Telegram 一键部署
Tags: telegram, 平台, 配置, 接入, 部署, deploy, config, one-click

---

## 🔵 Step 1 — 获取 Token

询问："Telegram Bot Token？获取：Telegram 搜索 @BotFather → /newbot → 复制 token"

## 🔵 Step 2 — 执行

收到 token 后用 code_run 执行：

```bash
./target/debug/openzen platform add telegram --bot-token "TOKEN" --model Agents_A1_8bit
```

如需代理：`--proxy "http://127.0.0.1:7890"`。

（如 `./target/debug/openzen` 不存在，改用 `./target/release/openzen`）

## 🔵 Step 3 — 报告

"✅ Telegram Bot 已配置。重启 OpenZen 后生效。"
