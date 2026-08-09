pub mod core;
pub mod generator;
pub mod injector;
pub mod narrative;
pub mod portrait;
pub mod reflection;
pub mod soul;
pub mod state;

pub use core::SoulCore;
pub use generator::SelfModelGenerator;
pub use injector::PromptInjector;
pub use narrative::{Chapter, LifeNarrative, NarrativeBuilder, NarrativeConfig};
pub use portrait::{Portrait, PortraitFact, Relationship};
pub use reflection::{ReflectionConfig, ReflectionEngine, ReflectionEvent};
pub use soul::{SoulHandle, SoulModel, SOUL_VERSION};
pub use state::SoulState;
