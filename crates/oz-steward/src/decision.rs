//! Provider-neutral typed decision API.
//!
//! The shape mirrors StartLux's `/v1/systemone`: a state plus typed questions,
//! and an answer with probabilities/none_probability per question. Keeping it
//! provider-neutral lets the StartLux adapter stay testable without a model.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, StewardError};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    #[serde(rename = "type")]
    pub kind: String,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<Value>,
}

impl Question {
    pub fn choice(
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let criteria = options.into_iter().map(Into::into).collect::<Vec<String>>();
        Self {
            kind: "choice".into(),
            instructions: instructions.into(),
            criteria: Some(serde_json::to_value(criteria).unwrap_or(Value::Null)),
        }
    }

    pub fn score(
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let levels = levels.into_iter().map(Into::into).collect::<Vec<String>>();
        Self {
            kind: "score".into(),
            instructions: instructions.into(),
            criteria: Some(serde_json::to_value(levels).unwrap_or(Value::Null)),
        }
    }

    pub fn yes_no(instructions: impl Into<String>) -> Self {
        Self {
            kind: "noul".into(),
            instructions: instructions.into(),
            criteria: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noul: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(default)]
    pub probabilities: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub none_probability: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legend: Option<Value>,
}

impl Answer {
    /// `none_probability` above the threshold means "none of the listed
    /// options fit"; the caller should abstain/escalate instead of taking the
    /// highest-probability option.
    pub fn abstained(&self, threshold: f32) -> bool {
        self.none_probability
            .map(|p| p >= threshold)
            .unwrap_or(false)
    }

    pub fn top_probability(&self) -> Option<f32> {
        self.probabilities
            .values()
            .copied()
            .fold(None, |acc, p| Some(acc.map_or(p, |a: f32| a.max(p))))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DecisionRequest {
    pub state: Value,
    pub questions: BTreeMap<String, Question>,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_budget: Option<u32>,
    #[serde(default)]
    pub allow_none: bool,
    #[serde(default)]
    pub none_check: bool,
}

impl DecisionRequest {
    pub fn new(state: Value) -> Self {
        Self {
            state,
            questions: BTreeMap::new(),
            images: Vec::new(),
            think_mode: None,
            gate: None,
            think_budget: None,
            allow_none: true,
            none_check: true,
        }
    }

    pub fn ask(mut self, id: impl Into<String>, q: Question) -> Self {
        self.questions.insert(id.into(), q);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DecisionResponse {
    #[serde(default)]
    pub answers: BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

#[async_trait]
pub trait DecisionProvider: Send + Sync {
    async fn evaluate(&self, req: &DecisionRequest) -> Result<DecisionResponse>;
    fn name(&self) -> &'static str;
}

pub fn require_answer<'a>(resp: &'a DecisionResponse, id: &str) -> Result<&'a Answer> {
    resp.answers
        .get(id)
        .ok_or_else(|| StewardError::Provider(format!("provider omitted answer `{id}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_constructors_use_expected_types() {
        let choice = Question::choice("pick", ["a", "b"]);
        assert_eq!(choice.kind, "choice");
        assert_eq!(choice.criteria, Some(serde_json::json!(["a", "b"])));
        let score = Question::score("rate", ["low", "high"]);
        assert_eq!(score.kind, "score");
        let yn = Question::yes_no("ok?");
        assert_eq!(yn.kind, "noul");
    }

    #[test]
    fn abstention_uses_none_probability_threshold() {
        let mut a = Answer {
            none_probability: Some(0.2),
            ..Answer::default()
        };
        assert!(a.abstained(0.2));
        assert!(!a.abstained(0.21));
        a.none_probability = None;
        assert!(!a.abstained(0.0));
    }

    #[test]
    fn missing_answer_is_an_explicit_error() {
        let resp = DecisionResponse::default();
        assert!(require_answer(&resp, "verdict").is_err());
    }

    #[test]
    fn top_probability_picks_max() {
        let mut a = Answer::default();
        a.probabilities.insert("a".into(), 0.2);
        a.probabilities.insert("b".into(), 0.7);
        assert_eq!(a.top_probability(), Some(0.7));
    }
}
