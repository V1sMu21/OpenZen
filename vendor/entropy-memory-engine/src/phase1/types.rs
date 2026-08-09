use crate::core::types::KnowledgeSource;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConflictResolution {
    Supplement,
    Sublimate,
    Overturn,
}

#[derive(Debug, Clone, Copy)]
pub struct ConflictScore {
    pub ccs: f32,
    pub factuality_gap: f32,
    pub compatibility: f32,
}

impl Default for ConflictScore {
    fn default() -> Self {
        Self {
            ccs: 0.5,
            factuality_gap: 0.0,
            compatibility: 0.5,
        }
    }
}

impl ConflictScore {
    pub fn resolution(&self) -> ConflictResolution {
        if self.ccs > 0.15 {
            ConflictResolution::Supplement
        } else if self.ccs > 0.05 {
            ConflictResolution::Sublimate
        } else {
            ConflictResolution::Overturn
        }
    }
}

pub fn default_factuality(source: KnowledgeSource) -> f32 {
    match source {
        KnowledgeSource::ExternalInput => 0.7,
        KnowledgeSource::Consolidation => 0.6,
        KnowledgeSource::Rambling => 0.3,
        KnowledgeSource::Inferred => 0.4,
    }
}
