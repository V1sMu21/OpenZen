🔴 Execute: ask App ID+App Secret → `./target/debug/openzen platform add feishu ...` → done.

# 飞书/Lark 一键部署
Tags: feishu, lark, 飞书, 平台, 配置, 接入, 部署, deploy, config, one-click

---

## 🔵 Step 1 — 获取凭证

询问："飞书 App ID 和 App Secret？获取：https://open.feishu.cn/ → 开发者后台 → 凭证与基础信息"

## 🔵 Step 2 — 执行

```bash
./target/debug/openzen platform add feishu --app-id "APP_ID" --app-secret "APP_SECRET" --model Agents_A1_8bit
```

## 🔵 Step 3 — 报告

"✅ 飞书机器人已配置。重启后生效。"
