🔴 EXECUTE IMMEDIATELY. Use Tauri IPC or CLI.

# 一键部署消息平台（飞书 / Telegram / 微信）
Tags: platform, feishu, telegram, wechat, 平台, 飞书, 微信, deploy, config, one-click

---

DO NOT: read mykey.toml, read source code, cargo build, or check project structure.

---

## 飞书
Ask: "飞书 App ID 和 App Secret？获取：https://open.feishu.cn/ → 开发者后台 → 凭证与基础信息"
Tauri: `tauriInvoke("add_platform", { name: "feishu", app_id: "ID", app_secret: "SECRET", default_model: "Agents_A1_8bit" })`

## Telegram
Ask: "Telegram Bot Token？获取：@BotFather → /newbot"
Tauri: `tauriInvoke("add_platform", { name: "telegram", bot_token: "TOKEN", default_model: "Agents_A1_8bit" })`

## 微信
No credentials needed.
Tauri: `tauriInvoke("add_platform", { name: "wechat", default_model: "Agents_A1_8bit" })`
Report: "✅ 微信已配置。"
