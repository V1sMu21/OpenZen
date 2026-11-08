//! StartLux-Decision provider (`/v1/systemone`).
//!
//! StartLux is not an oMLX chat model: Instant uses bidirectional prompt
//! attention and a letter read-out, so it must be served by the official
//! `startlux-decide` process. This provider only speaks HTTP to that process.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::decision::{DecisionProvider, DecisionRequest, DecisionResponse};
use crate::error::{Result, StewardError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartLuxTier {
    B0_8,
    B2,
    B4,
    B9,
    B27,
}

impl StartLuxTier {
    pub fn label(self) -> &'static str {
        match self {
            StartLuxTier::B0_8 => "0.8B",
            StartLuxTier::B2 => "2B",
            StartLuxTier::B4 => "4B",
            StartLuxTier::B9 => "9B",
            StartLuxTier::B27 => "27B",
        }
    }

    pub fn approx_4bit_gb(self) -> f32 {
        match self {
            StartLuxTier::B0_8 => 0.6,
            StartLuxTier::B2 => 1.3,
            StartLuxTier::B4 => 2.5,
            StartLuxTier::B9 => 5.0,
            StartLuxTier::B27 => 15.0,
        }
    }

    /// 2B/0.8B only ship Instant checkpoints; only 4B/9B/27B have AutoThink.
    pub fn supports_auto_think(self) -> bool {
        matches!(
            self,
            StartLuxTier::B4 | StartLuxTier::B9 | StartLuxTier::B27
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartLuxVariant {
    Instant,
    AutoThink,
}

impl StartLuxVariant {
    pub fn label(self) -> &'static str {
        match self {
            StartLuxVariant::Instant => "Instant",
            StartLuxVariant::AutoThink => "AutoThink",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StartLuxConfig {
    pub endpoint: String,
    pub model: String,
    pub tier: StartLuxTier,
    pub variant: StartLuxVariant,
    pub none_threshold: f32,
    pub timeout_ms: u64,
    /// AutoThink only; ignored by Instant.
    pub gate: f32,
    pub think_budget: u32,
}

impl StartLuxConfig {
    pub fn new(tier: StartLuxTier, variant: StartLuxVariant) -> Result<Self> {
        if variant == StartLuxVariant::AutoThink && !tier.supports_auto_think() {
            return Err(StewardError::Invalid(format!(
                "{} has no AutoThink checkpoint; use Instant",
                tier.label()
            )));
        }
        let timeout_ms = match (variant, tier) {
            (StartLuxVariant::AutoThink, StartLuxTier::B27) => 45_000,
            (StartLuxVariant::AutoThink, _) => 15_000,
            (
                StartLuxVariant::Instant,
                StartLuxTier::B0_8 | StartLuxTier::B2 | StartLuxTier::B4,
            ) => 1_500,
            (StartLuxVariant::Instant, StartLuxTier::B9) => 3_000,
            (StartLuxVariant::Instant, StartLuxTier::B27) => 8_000,
        };
        Ok(Self {
            endpoint: "http://127.0.0.1:8001".into(),
            model: format!("StartLux-Decision-{}-{}", tier.label(), variant.label()),
            tier,
            variant,
            none_threshold: 0.2,
            timeout_ms,
            gate: 0.9,
            think_budget: 384,
        })
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_none_threshold(mut self, threshold: f32) -> Self {
        self.none_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms.max(1);
        self
    }

    pub fn with_auto_think_budget(mut self, gate: f32, think_budget: u32) -> Self {
        self.gate = gate.clamp(0.0, 1.0);
        self.think_budget = think_budget.max(1);
        self
    }

    pub fn body(&self, req: &DecisionRequest) -> Value {
        let mut body = json!({
            "state": req.state,
            "questions": req.questions,
            "images": req.images,
            "allow_none": req.allow_none,
            "none_check": req.none_check,
        });
        match self.variant {
            StartLuxVariant::Instant => {
                body["think_mode"] = json!("never");
            }
            StartLuxVariant::AutoThink => {
                body["think_mode"] = json!("auto");
                body["gate"] = json!(self.gate);
                body["think_budget"] = json!(self.think_budget);
            }
        }
        body
    }

    pub fn abstained(&self, resp: &DecisionResponse, question: &str) -> bool {
        resp.answers
            .get(question)
            .map(|a| a.abstained(self.none_threshold))
            .unwrap_or(false)
    }
}

pub struct StartLuxProvider {
    client: reqwest::Client,
    config: StartLuxConfig,
}

impl StartLuxProvider {
    pub fn new(config: StartLuxConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .build()?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &StartLuxConfig {
        &self.config
    }

    /// GET /health. Used by startup checks to verify Instant is served with
    /// bidirectional prompt attention, never via a causal chat path.
    pub async fn health(&self) -> Result<Value> {
        let resp = self
            .client
            .get(format!("{}/health", self.config.endpoint))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(StewardError::Provider(format!(
                "startlux health HTTP {}",
                resp.status()
            )));
        }
        Ok(resp.json().await?)
    }
}

#[async_trait]
impl DecisionProvider for StartLuxProvider {
    async fn evaluate(&self, req: &DecisionRequest) -> Result<DecisionResponse> {
        let body = self.config.body(req);
        let resp = self
            .client
            .post(format!("{}/v1/systemone", self.config.endpoint))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(StewardError::Provider(format!(
                "startlux HTTP {status}: {}",
                text.chars().take(500).collect::<String>()
            )));
        }
        Ok(resp.json::<DecisionResponse>().await?)
    }

    fn name(&self) -> &'static str {
        "startlux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Answer, Question};
    use std::collections::BTreeMap;

    #[test]
    fn two_b_has_no_auto_think() {
        assert!(StartLuxConfig::new(StartLuxTier::B2, StartLuxVariant::AutoThink).is_err());
        assert!(StartLuxConfig::new(StartLuxTier::B0_8, StartLuxVariant::AutoThink).is_err());
        assert!(StartLuxConfig::new(StartLuxTier::B4, StartLuxVariant::AutoThink).is_ok());
        assert!(StartLuxConfig::new(StartLuxTier::B27, StartLuxVariant::AutoThink).is_ok());
    }

    #[test]
    fn instant_body_never_thinks() {
        let cfg = StartLuxConfig::new(StartLuxTier::B4, StartLuxVariant::Instant).unwrap();
        let req = DecisionRequest::new(serde_json::json!({"task":"x"}))
            .ask("verdict", Question::choice("on track?", ["yes", "no"]));
        let body = cfg.body(&req);
        assert_eq!(body["think_mode"], "never");
        assert!(body.get("gate").is_none());
        assert_eq!(body["questions"]["verdict"]["type"], "choice");
    }

    #[test]
    fn auto_think_body_carries_gate_and_budget() {
        let cfg = StartLuxConfig::new(StartLuxTier::B9, StartLuxVariant::AutoThink)
            .unwrap()
            .with_auto_think_budget(0.9, 384);
        let req = DecisionRequest::new(serde_json::json!({}));
        let body = cfg.body(&req);
        assert_eq!(body["think_mode"], "auto");
        assert!((body["gate"].as_f64().unwrap() - 0.9).abs() < 1e-6);
        assert_eq!(body["think_budget"], 384);
    }

    #[test]
    fn response_none_probability_marks_abstention() {
        let cfg = StartLuxConfig::new(StartLuxTier::B4, StartLuxVariant::Instant)
            .unwrap()
            .with_none_threshold(0.2);
        let mut answers = BTreeMap::new();
        answers.insert(
            "verdict".to_string(),
            Answer {
                none_probability: Some(0.25),
                ..Answer::default()
            },
        );
        let resp = DecisionResponse {
            answers,
            ..DecisionResponse::default()
        };
        assert!(cfg.abstained(&resp, "verdict"));
        assert!(!cfg.abstained(&resp, "missing"));
    }

    #[test]
    fn tier_memory_and_timeout_are_sane() {
        assert!(StartLuxTier::B27.approx_4bit_gb() > StartLuxTier::B9.approx_4bit_gb());
        let cfg = StartLuxConfig::new(StartLuxTier::B27, StartLuxVariant::Instant).unwrap();
        assert_eq!(cfg.timeout_ms, 8_000);
        let auto = StartLuxConfig::new(StartLuxTier::B27, StartLuxVariant::AutoThink).unwrap();
        assert_eq!(auto.timeout_ms, 45_000);
        let auto9 = StartLuxConfig::new(StartLuxTier::B9, StartLuxVariant::AutoThink).unwrap();
        assert_eq!(auto9.timeout_ms, 15_000);
    }
}
