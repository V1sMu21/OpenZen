use teloxide::prelude::*;

pub async fn handle_command(bot: &Bot, msg: &Message, text: &str) {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let op = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();

    match op.as_str() {
        "/help" => {
            let _ = bot
                .send_message(msg.chat.id, "📖 命令列表:\n/help - 帮助\n/stop - 停止\n/new - 新对话\n/status - 状态\n/llm - 模型列表")
                .await;
        }
        "/stop" => {
            let _ = bot.send_message(msg.chat.id, "⏹️ 正在停止...").await;
        }
        "/new" => {
            let _ = bot.send_message(msg.chat.id, "✅ 新对话已开启").await;
        }
        "/status" => {
            let _ = bot.send_message(msg.chat.id, "🟢 OpenZen 运行中（通过平台适配器）").await;
        }
        "/llm" => {
            let _ = bot.send_message(msg.chat.id, "模型列表功能开发中").await;
        }
        _ => {
            let _ = bot
                .send_message(msg.chat.id, format!("未知命令: {text}"))
                .await;
        }
    }
}
