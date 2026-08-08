use crate::client::FeishuClient;

const DETAIL_LIMIT: usize = 8000;
const FINAL_LIMIT: usize = 6000;

pub struct TaskCard {
    receive_id: String,
    receive_id_type: String,
    steps: Vec<(String, String)>,
    status: String,
    final_text: Option<String>,
    message_id: Option<String>,
    page_no: u32,
    turn_no: u32,
    turn_base: u32,
    note: Option<String>,
}

impl TaskCard {
    pub fn new(receive_id: String, receive_id_type: String) -> Self {
        TaskCard {
            receive_id,
            receive_id_type,
            steps: Vec::new(),
            status: "🤔 思考中...".into(),
            final_text: None,
            message_id: None,
            page_no: 1,
            turn_no: 0,
            turn_base: 1,
            note: None,
        }
    }

    pub async fn start(&mut self, client: &FeishuClient) -> Result<(), String> {
        self.push(client).await
    }

    pub async fn step(
        &mut self,
        client: &FeishuClient,
        summary: &str,
        detail: &str,
    ) -> Result<(), String> {
        self.turn_no += 1;
        let step = (summary.to_string(), detail.to_string());
        self.steps.push(step);
        self.status = format!("⏳ 工作中 · Turn {}", self.turn_no);

        match self.push(client).await {
            Ok(()) => Ok(()),
            Err(e) if e.starts_with("card_limit:") => {
                self.steps.pop();
                self.rollover();
                self.turn_base = self.turn_no;
                self.steps = vec![(summary.to_string(), detail.to_string())];
                self.push(client).await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn done(&mut self, client: &FeishuClient, text: &str) -> Result<(), String> {
        self.status = "✅ 已完成".into();
        let truncated = if text.len() > FINAL_LIMIT {
            let mut t = text[..FINAL_LIMIT].to_string();
            t.push_str(&format!("\n\n…(truncated, {} chars)", text.len()));
            t
        } else {
            text.to_string()
        };
        self.final_text = if truncated.is_empty() {
            Some("_(无文本输出)_".into())
        } else {
            Some(truncated)
        };

        match self.push(client).await {
            Ok(()) => Ok(()),
            Err(e) if e.starts_with("card_limit:") => {
                self.rollover();
                self.steps.clear();
                self.turn_base = self.turn_no + 1;
                self.push(client).await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn fail(&mut self, client: &FeishuClient, msg: &str) {
        self.status = format!("❌ {msg}");
        let _ = self.push(client).await;
    }

    fn rollover(&mut self) {
        self.page_no += 1;
        self.message_id = None;
        self.final_text = None;
        self.note = Some("⚠️ 上一张工作卡片达到飞书限制，本页继续展示后续进展。".into());
    }

    async fn push(&mut self, client: &FeishuClient) -> Result<(), String> {
        let card_json = self.build_card();
        if let Some(ref msg_id) = self.message_id {
            let result = client.patch_card(msg_id, &card_json).await;
            match result {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    self.message_id = None;
                }
                Err(e) => return Err(e),
            }
        }

        let result = client
            .send_card(&self.receive_id, &card_json, &self.receive_id_type)
            .await?;
        self.message_id = result;
        Ok(())
    }

    fn build_card(&self) -> String {
        let mut header = format!("**{}**", self.status);
        if self.page_no > 1 {
            header.push_str(&format!("\n\n📄 工作卡片 {}", self.page_no));
        }

        let mut elements: Vec<serde_json::Value> = vec![serde_json::json!({
            "tag": "markdown",
            "content": header,
        })];

        if let Some(ref note) = self.note {
            elements.push(serde_json::json!({
                "tag": "markdown",
                "content": note,
            }));
        }

        for (i, (summary, detail)) in self.steps.iter().enumerate() {
            let turn_num = self.turn_base as usize + i;
            let mut detail = detail.clone();
            if detail.len() > DETAIL_LIMIT {
                detail = format!(
                    "{}…\n\n…(truncated, {} chars)",
                    &detail[..DETAIL_LIMIT],
                    detail.len()
                );
            }
            if detail.is_empty() {
                detail = "_(无输出)_".into();
            }

            elements.push(serde_json::json!({
                "tag": "collapsible_panel",
                "expanded": false,
                "header": {
                    "title": {
                        "tag": "plain_text",
                        "content": format!("Turn {} · {}", turn_num, summary),
                    }
                },
                "elements": [
                    {
                        "tag": "markdown",
                        "content": detail,
                    }
                ],
            }));
        }

        if let Some(ref final_text) = self.final_text {
            elements.push(serde_json::json!({ "tag": "hr" }));
            elements.push(serde_json::json!({
                "tag": "markdown",
                "content": final_text,
            }));
        }

        let card = serde_json::json!({
            "schema": "2.0",
            "config": {
                "streaming_mode": false,
                "width_mode": "fill",
            },
            "body": {
                "elements": elements,
            },
        });

        serde_json::to_string(&card).unwrap_or_default()
    }
}
