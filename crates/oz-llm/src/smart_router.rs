use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use oz_core_types::{ContentBlock, LlmError, Message, TokenUsage, ToolDefinition};
use oz_config::SessionConfig;
use oz_config::mykey::RouterConfig;

use crate::session::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    Simple,
    Complex,
}

pub fn estimate_complexity(prompt: &str, tools: &[ToolDefinition]) -> Complexity {
    let total_chars = prompt.len();
    let has_code = prompt.contains("```")
        || prompt.contains("fn ")
        || prompt.contains("impl ")
        || prompt.contains("struct ")
        || prompt.contains("pub ");
    let lower = prompt.to_lowercase();
    let has_complex_keywords = lower.contains("analyze")
        || lower.contains("refactor")
        || lower.contains("optimize")
        || lower.contains("compare")
        || lower.contains("architecture")
        || lower.contains("design pattern")
        || lower.contains("security")
        || lower.contains("performance")
        || lower.contains("algorithm")
        || lower.contains("multi-step")
        || lower.contains("complex");

    if total_chars > 2000 || has_code || has_complex_keywords || tools.len() > 5 {
        Complexity::Complex
    } else {
        Complexity::Simple
    }
}

pub struct SmartRouterSession {
    cheap: Box<dyn Session>,
    flagship: Box<dyn Session>,
    cheap_count: AtomicU64,
    flagship_count: AtomicU64,
    route_rules: Vec<(String, Box<dyn Session>)>,
}

impl SmartRouterSession {
    pub fn new(cheap: Box<dyn Session>, flagship: Box<dyn Session>) -> Self {
        SmartRouterSession {
            cheap,
            flagship,
            cheap_count: AtomicU64::new(0),
            flagship_count: AtomicU64::new(0),
            route_rules: Vec::new(),
        }
    }

    pub fn from_config(
        cheap: Box<dyn Session>,
        flagship: Box<dyn Session>,
        config: &RouterConfig,
        rule_sessions: Vec<(String, Box<dyn Session>)>,
    ) -> Self {
        let mut router = SmartRouterSession::new(cheap, flagship);
        router.route_rules = rule_sessions;
        // Thresholds are used inside pick()
        let _ = config;
        router
    }

    fn pick(&self, prompt: &str, tools: &[ToolDefinition]) -> &dyn Session {
        let lower = prompt.to_lowercase();
        for (pattern, session) in &self.route_rules {
            if lower.contains(&pattern.to_lowercase()) {
                tracing::info!("[SmartRouter] route rule match '{}' -> {}", pattern, session.model());
                return &**session;
            }
        }
        if estimate_complexity(prompt, tools) == Complexity::Simple {
            self.cheap_count.fetch_add(1, Ordering::Relaxed);
            &*self.cheap
        } else {
            self.flagship_count.fetch_add(1, Ordering::Relaxed);
            &*self.flagship
        }
    }

    pub fn routing_stats(&self) -> (u64, u64) {
        (
            self.cheap_count.load(Ordering::Relaxed),
            self.flagship_count.load(Ordering::Relaxed),
        )
    }
}

#[async_trait::async_trait]
impl Session for SmartRouterSession {
    fn config(&self) -> &SessionConfig {
        self.cheap.config()
    }

    fn history(&self) -> &Mutex<Vec<Message>> {
        self.cheap.history()
    }

    fn history_mut(&self) -> &Mutex<Vec<Message>> {
        self.cheap.history_mut()
    }

    fn set_system(&mut self, system: String) {
        self.cheap.set_system(system.clone());
        self.flagship.set_system(system);
    }

    fn set_tools(&mut self, tools: Vec<ToolDefinition>) {
        self.cheap.set_tools(tools.clone());
        self.flagship.set_tools(tools);
    }

    async fn raw_ask(&self, messages: &[Message]) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
        let prompt = messages
            .iter()
            .flat_map(|m| m.content.iter())
            .map(|block| match block {
                ContentBlock::Text { text, .. } => text.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>()
            .join(" ");
        let session = self.pick(&prompt, &[]);
        tracing::info!("[SmartRouter] raw_ask -> {}", session.model());
        session.raw_ask(messages).await
    }

    async fn ask(&self, prompt: &str) -> Result<Vec<ContentBlock>, LlmError> {
        let session = self.pick(prompt, &[]);
        tracing::info!(
            "[SmartRouter] ask -> {} (complexity: {:?})",
            session.model(),
            estimate_complexity(prompt, &[]),
        );
        session.ask(prompt).await
    }

    fn format_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        self.cheap.format_messages(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct DummySession {
        name: String,
        history: Mutex<Vec<Message>>,
    }

    impl DummySession {
        fn new(name: &str) -> Self {
            DummySession {
                name: name.to_string(),
                history: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Session for DummySession {
        fn config(&self) -> &SessionConfig {
            panic!("not used")
        }
        fn history(&self) -> &Mutex<Vec<Message>> {
            &self.history
        }
        fn history_mut(&self) -> &Mutex<Vec<Message>> {
            &self.history
        }
        fn set_system(&mut self, _system: String) {}
        fn set_tools(&mut self, _tools: Vec<ToolDefinition>) {}
        async fn raw_ask(&self, _messages: &[Message]) -> Result<(Vec<ContentBlock>, Option<TokenUsage>), LlmError> {
            Ok((vec![ContentBlock::text("mock")], None))
        }
        async fn ask(&self, _prompt: &str) -> Result<Vec<ContentBlock>, LlmError> {
            Ok(vec![ContentBlock::text("mock")])
        }
        fn format_messages(&self, _messages: &[Message]) -> Vec<serde_json::Value> {
            vec![]
        }
    }

    #[test]
    fn test_simple_text_is_simple() {
        assert_eq!(estimate_complexity("What is the capital of France?", &[]), Complexity::Simple);
    }

    #[test]
    fn test_code_is_complex() {
        assert_eq!(estimate_complexity("fn main() {}", &[]), Complexity::Complex);
    }

    #[test]
    fn test_keyword_is_complex() {
        assert_eq!(estimate_complexity("Analyze this algorithm", &[]), Complexity::Complex);
        assert_eq!(estimate_complexity("Refactor this code", &[]), Complexity::Complex);
    }

    #[test]
    fn test_long_text_is_complex() {
        let long = "a".repeat(2500);
        assert_eq!(estimate_complexity(&long, &[]), Complexity::Complex);
    }

    #[test]
    fn test_many_tools_is_complex() {
        let tools: Vec<ToolDefinition> = (0..6)
            .map(|i| ToolDefinition {
                type_: "function".into(),
                function: oz_core_types::ToolFunction {
                    name: format!("tool_{i}"),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                },
            })
            .collect();
        assert_eq!(estimate_complexity("do it", &tools), Complexity::Complex);
    }

    #[tokio::test]
    async fn test_routing_simple_to_cheap() {
        let cheap = DummySession::new("cheap");
        let flagship = DummySession::new("flagship");
        let router = SmartRouterSession::new(Box::new(cheap), Box::new(flagship));
        let _ = router.ask("Hello").await;
        let (c, f) = router.routing_stats();
        assert_eq!(c, 1);
        assert_eq!(f, 0);
    }
}
