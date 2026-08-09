use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::core::now_nanos;
use crate::l0::core::SoulCore;
use crate::l0::narrative::LifeNarrative;
use crate::l0::portrait::{Portrait, Relationship};
use crate::l0::state::SoulState;

/// 灵魂模型当前 schema 版本
pub const SOUL_VERSION: u64 = 1;

/// L0 灵魂模型 = 核心良知 + 情境状态 + 画像对 + 生命叙事
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SoulModel {
    /// 慢变：良知本体
    pub core: SoulCore,
    /// 快变：情境自我
    pub state: SoulState,
    /// 自画像
    pub self_portrait: Portrait,
    /// 用户画像
    pub user_portrait: Portrait,
    /// 我与用户的关系
    pub relationship: Relationship,
    /// 生命叙事
    pub narrative: LifeNarrative,
    /// 单调递增，供外部缓存失效
    pub version: u64,
}

impl SoulModel {
    pub fn new() -> Self {
        Self {
            core: SoulCore::default(),
            state: SoulState::new(),
            self_portrait: Portrait::new(),
            user_portrait: Portrait::new(),
            relationship: Relationship::default(),
            narrative: LifeNarrative::new(),
            version: SOUL_VERSION,
        }
    }

    /// 从 JSON 字符串加载（向后兼容：旧 L0SelfModel 的 data 字段迁移）。
    ///
    /// 迁移规则：
    /// - 旧 `data.core_identity` → `SoulCore.identity`
    /// - 旧 `data.last_updated` → `SoulState.last_updated_nanos`
    /// - 其余字段保留默认值；解析失败时返回错误。
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        // 先尝试新格式
        if let Ok(soul) = serde_json::from_str::<SoulModel>(json) {
            return Ok(soul);
        }
        // 旧格式：{ data: { core_identity, ... } }
        let legacy: JsonValue = serde_json::from_str(json)?;
        let data = legacy.get("data").unwrap_or(&legacy);
        let mut soul = SoulModel::new();
        if let Some(identity) = data.get("core_identity").and_then(|v| v.as_str()) {
            if !identity.is_empty() {
                soul.core.identity = identity.to_string();
            }
        }
        if let Some(ts) = data.get("last_updated").and_then(|v| v.as_i64()) {
            soul.state.last_updated_nanos = ts;
        }
        if let Some(total) = data.get("total_memories").and_then(|v| v.as_u64()) {
            soul.state.confidence = if total == 0 { 0.0 } else { 1.0 };
        }
        Ok(soul)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// 原子写：临时文件 + rename，崩溃安全。
    pub fn save_atomic(&self, path: &Path) -> std::io::Result<()> {
        let json = self.to_json();
        let tmp: PathBuf = path.with_extension("json.tmp");
        fs::write(&tmp, json.as_bytes())?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 从磁盘加载；文件不存在时返回默认模型。
    pub fn load_from(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let json = fs::read_to_string(path)?;
        Self::from_json(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    /// 画像更新后的统一收尾：递增版本、刷新时间戳。
    pub fn bump_version(&mut self) {
        self.version = self.version.saturating_add(1);
        self.state.last_updated_nanos = now_nanos();
    }
}

impl Default for SoulModel {
    fn default() -> Self {
        Self::new()
    }
}

/// 线程安全的灵魂句柄（与现有 `Arc<RwLock<L0SelfModel>>` 模式一致）。
pub type SoulHandle = Arc<RwLock<SoulModel>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l0::portrait::PortraitFact;

    fn fact(statement: &str, confidence: f32) -> PortraitFact {
        PortraitFact {
            statement: statement.into(),
            confidence,
            supporting_ids: Vec::new(),
            contradicting_ids: Vec::new(),
        }
    }

    #[test]
    fn test_json_roundtrip() {
        let mut soul = SoulModel::new();
        soul.user_portrait.insert_fact(fact("喜欢咖啡", 0.7));
        soul.bump_version();
        let json = soul.to_json();
        let back = SoulModel::from_json(&json).unwrap();
        assert_eq!(back, soul);
        assert!(back.version >= 1);
    }

    #[test]
    fn test_legacy_migration() {
        let legacy = r#"{
            "data": {
                "core_identity": "legacy agent",
                "recent_insights": [],
                "active_questions": [],
                "total_memories": 42,
                "last_updated": 1719600000000
            }
        }"#;
        let soul = SoulModel::from_json(legacy).unwrap();
        assert_eq!(soul.core.identity, "legacy agent");
        assert_eq!(soul.state.last_updated_nanos, 1719600000000);
        assert_eq!(soul.state.confidence, 1.0);
    }

    #[test]
    fn test_bare_legacy_migration() {
        // 旧 L0SelfModel::to_json 输出的是裸 data 对象（无 data 包装）
        let bare = r#"{"core_identity":"bare","last_updated":100}"#;
        let soul = SoulModel::from_json(bare).unwrap();
        assert_eq!(soul.core.identity, "bare");
        assert_eq!(soul.state.last_updated_nanos, 100);
    }

    #[test]
    fn test_atomic_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("l0_soul.json");
        let mut soul = SoulModel::new();
        soul.core.identity = "persisted".into();
        soul.save_atomic(&path).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());
        let loaded = SoulModel::load_from(&path).unwrap();
        assert_eq!(loaded.core.identity, "persisted");
    }

    #[test]
    fn test_load_missing_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let soul = SoulModel::load_from(&path).unwrap();
        // 时间戳在两次构造间不同，逐字段比较结构而非整体相等
        assert_eq!(soul.core.identity, SoulModel::new().core.identity);
        assert_eq!(soul.version, SoulModel::new().version);
        assert!(soul.narrative.chapters.is_empty());
        assert!(soul.user_portrait.facts.is_empty());
    }

    #[test]
    fn test_version_increments() {
        let mut soul = SoulModel::new();
        let v0 = soul.version;
        soul.bump_version();
        assert_eq!(soul.version, v0 + 1);
    }
}
