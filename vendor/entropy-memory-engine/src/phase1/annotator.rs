use crate::core::types::{MemoryContent, MemoryInput};

const HIGH_ABSTRACTION_KEYWORDS: &[&str] = &[
    "philosophy",
    "principle",
    "theory",
    "concept",
    "abstract",
    "general",
    "universal",
    "meta",
    "framework",
    "paradigm",
    "law",
    "rule",
    "belief",
    "value",
    "ethic",
    "moral",
];

const OPINION_KEYWORDS: &[&str] = &[
    "think", "believe", "feel", "prefer", "like", "dislike", "love", "hate", "opinion", "favorite",
    "best", "worst", "should", "maybe", "perhaps",
];

pub struct MetadataAnnotator;

impl MetadataAnnotator {
    pub fn new() -> Self {
        Self
    }

    pub fn annotate(&self, input: &MemoryInput) -> (f32, f32) {
        let text = match &input.content {
            MemoryContent::Fact(f) => {
                format!("{} {} {}", f.subject, f.predicate, f.object)
            }
            MemoryContent::Summary(s) => s.clone(),
            _ => String::new(),
        };
        let text_lower = text.to_lowercase();

        let factuality = self.score_factuality(&text_lower, input);
        let abstraction = self.score_abstraction(&text_lower, input);

        (factuality, abstraction)
    }

    fn score_factuality(&self, text: &str, input: &MemoryInput) -> f32 {
        let mut score: f32 = 0.5;

        if has_digits_or_dates(text) {
            score += 0.2;
        }
        if is_opinion_text(text) {
            score -= 0.2;
        }
        if let MemoryContent::Summary(_) = &input.content {
            score -= 0.1;
        }

        score.clamp(0.0, 1.0)
    }

    fn score_abstraction(&self, text: &str, input: &MemoryInput) -> f32 {
        let mut score = 0.1;

        if let MemoryContent::Summary(_) = &input.content {
            score = 0.5;
        }

        let keyword_count = HIGH_ABSTRACTION_KEYWORDS
            .iter()
            .filter(|kw| text.contains(*kw))
            .count();
        if keyword_count > 0 {
            score = (score + 0.1 * keyword_count as f32).min(0.9);
        }

        score.clamp(0.0, 1.0)
    }
}

impl Default for MetadataAnnotator {
    fn default() -> Self {
        Self::new()
    }
}

fn has_digits_or_dates(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_digit())
}

fn is_opinion_text(text: &str) -> bool {
    OPINION_KEYWORDS.iter().any(|kw| text.contains(kw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::Fact;

    fn make_fact_input(subject: &str, predicate: &str, object: &str) -> MemoryInput {
        MemoryInput::new(MemoryContent::Fact(Fact::new(subject, predicate, object)))
    }

    #[test]
    fn test_annotate_factual_numeric() {
        let annotator = MetadataAnnotator::new();
        let input = make_fact_input("earth", "orbits", "sun in 365 days");
        let (factuality, _) = annotator.annotate(&input);
        assert!(
            factuality > 0.6,
            "numeric facts should have higher factuality, got {}",
            factuality
        );
    }

    #[test]
    fn test_annotate_opinion_lower_factuality() {
        let annotator = MetadataAnnotator::new();
        let input = make_fact_input("user", "thinks", "rust is the best language");
        let (factuality, _) = annotator.annotate(&input);
        assert!(
            factuality < 0.6,
            "opinion facts should have lower factuality, got {}",
            factuality
        );
    }

    #[test]
    fn test_annotate_abstract_keyword() {
        let annotator = MetadataAnnotator::new();
        let input = make_fact_input("philosophy", "teaches", "universal principles of ethics");
        let (_, abstraction) = annotator.annotate(&input);
        assert!(
            abstraction > 0.2,
            "abstract keywords should raise abstraction, got {}",
            abstraction
        );
    }

    #[test]
    fn test_annotate_summary_higher_abstraction() {
        let annotator = MetadataAnnotator::new();
        let input = MemoryInput::new(MemoryContent::Summary(
            "consolidated facts about user preferences".into(),
        ));
        let (_, abstraction) = annotator.annotate(&input);
        assert!(
            abstraction > 0.3,
            "summaries should have higher abstraction, got {}",
            abstraction
        );
    }

    #[test]
    fn test_annotate_factuality_clamped() {
        let annotator = MetadataAnnotator::new();
        let input = make_fact_input("user", "thinks", "the value of pi is 3.14");
        let (factuality, _) = annotator.annotate(&input);
        assert!(
            (0.0..=1.0).contains(&factuality),
            "factuality must be in [0,1], got {}",
            factuality
        );
    }

    #[test]
    fn test_abstraction_clamped() {
        let annotator = MetadataAnnotator::new();
        let input = MemoryInput::new(MemoryContent::Summary(
            "philosophy theory concept paradigm framework".into(),
        ));
        let (_, abstraction) = annotator.annotate(&input);
        assert!(
            (0.0..=1.0).contains(&abstraction),
            "abstraction must be in [0,1], got {}",
            abstraction
        );
    }
}
