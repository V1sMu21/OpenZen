use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::now_nanos;
use crate::l3::L3Engine;

/// 生命叙事：方案 B（叙事自我）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LifeNarrative {
    /// 章节（按时间/主题）
    pub chapters: Vec<Chapter>,
    /// 当前叙事弧线
    pub current_arc: String,
    pub last_rebuilt_nanos: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chapter {
    pub title: String,
    /// (start_nanos, end_nanos)
    pub period: (i64, i64),
    pub summary: String,
    /// 支撑该章节的记忆 ID
    pub source_ids: Vec<u64>,
}

impl LifeNarrative {
    pub fn new() -> Self {
        Self {
            chapters: Vec::new(),
            current_arc: String::new(),
            last_rebuilt_nanos: 0,
        }
    }

    /// 最近一章的摘要（注入用）。
    pub fn latest_summary(&self) -> &str {
        self.chapters
            .last()
            .map(|c| c.summary.as_str())
            .unwrap_or("")
    }
}

impl Default for LifeNarrative {
    fn default() -> Self {
        Self::new()
    }
}

/// 叙事生成配置
#[derive(Debug, Clone)]
pub struct NarrativeConfig {
    /// 章节时间窗（纳秒）。默认 7 天。
    pub window_nanos: i64,
    /// 每章保留的 Top-K 高 importance 记忆数
    pub top_k_per_chapter: usize,
    /// 章节上限（防无限增长）
    pub max_chapters: usize,
}

impl Default for NarrativeConfig {
    fn default() -> Self {
        Self {
            window_nanos: 7 * 24 * 3600 * 1_000_000_000,
            top_k_per_chapter: 5,
            max_chapters: 32,
        }
    }
}

/// 叙事生成器。纯启发式，不依赖 LLM（MLX/LLM 仅作增强）。
pub struct NarrativeBuilder {
    config: NarrativeConfig,
}

impl NarrativeBuilder {
    pub fn new(config: NarrativeConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &NarrativeConfig {
        &self.config
    }

    /// 从 L3 记忆构建叙事。
    ///
    /// 1. 过滤 `since_nanos` 之后的记忆；
    /// 2. 按 `window_nanos` 时间窗切分章节；
    /// 3. 每章取 Top-K 高 importance 记忆（复用 `metadata.importance`）；
    /// 4. 摘要 = 代表性记忆文本拼接 + 高频主题词；
    /// 5. 当前弧线 = 最近一章与上一章的主题漂移方向。
    pub fn build(&self, l3: &L3Engine, since_nanos: i64) -> LifeNarrative {
        let all = l3.storage().all();
        let recent: Vec<_> = all
            .into_iter()
            .filter(|m| m.metadata.created_at >= since_nanos)
            .collect();

        let mut narrative = LifeNarrative::new();
        narrative.last_rebuilt_nanos = now_nanos();

        if recent.is_empty() {
            return narrative;
        }

        // 1) 按时间窗切分
        let mut windows: Vec<Vec<(u64, String, f32, i64)>> = Vec::new();
        for mem in recent {
            let created = mem.metadata.created_at;
            let idx = ((created - since_nanos) / self.config.window_nanos) as usize;
            while windows.len() <= idx {
                windows.push(Vec::new());
            }
            windows[idx].push((mem.id, mem.content_text(), mem.metadata.importance, created));
        }

        // 2) 每章取 Top-K 高 importance
        for (idx, group) in windows.iter().enumerate() {
            if group.is_empty() {
                continue;
            }
            let mut sorted = group.clone();
            sorted.sort_by(|a, b| b.2.total_cmp(&a.2)); // importance 降序
            sorted.truncate(self.config.top_k_per_chapter);

            let start = since_nanos + idx as i64 * self.config.window_nanos;
            let end = start + self.config.window_nanos;
            let summary = self.build_summary(&sorted);
            let title = self.build_title(idx, &sorted);
            let source_ids: Vec<u64> = sorted.iter().map(|(id, _, _, _)| *id).collect();

            narrative.chapters.push(Chapter {
                title,
                period: (start, end),
                summary,
                source_ids,
            });
            if narrative.chapters.len() >= self.config.max_chapters {
                break;
            }
        }

        // 3) 当前弧线 = 最近两章的主题漂移
        narrative.current_arc = self.detect_arc(&narrative.chapters);

        narrative
    }

    fn build_summary(&self, top: &[(u64, String, f32, i64)]) -> String {
        let texts: Vec<&str> = top.iter().map(|(_, text, _, _)| text.as_str()).collect();
        let keywords = self.extract_keywords(&texts);
        let mut summary = keywords;
        // 附加代表性记忆文本（前 2 条）
        for text in texts.iter().take(2) {
            let clipped: String = text.chars().take(80).collect();
            summary.push(clipped);
        }
        summary.join("；")
    }

    fn build_title(&self, idx: usize, top: &[(u64, String, f32, i64)]) -> String {
        if let Some((_, _, _, created)) = top.first() {
            let t = *created;
            let (secs, _) = (t / 1_000_000_000, t % 1_000_000_000);
            format!("第{}章 · epoch {}", idx + 1, secs)
        } else {
            format!("第{}章", idx + 1)
        }
    }

    /// 简单关键词提取：取高频词（长度 >= 2，排除停用词）。
    fn extract_keywords(&self, texts: &[&str]) -> Vec<String> {
        let stop: &[&str] = &[
            "the", "a", "an", "of", "to", "in", "is", "and", "for", "on", "with",
        ];
        let mut freq: HashMap<String, usize> = HashMap::new();
        for text in texts {
            for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
                let w = word.to_lowercase();
                if w.len() < 2 || stop.contains(&w.as_str()) {
                    continue;
                }
                *freq.entry(w).or_insert(0) += 1;
            }
        }
        let mut words: Vec<(String, usize)> = freq.into_iter().collect();
        words.sort_by(|a, b| b.1.cmp(&a.1));
        words.truncate(3);
        words.into_iter().map(|(w, _)| w).collect()
    }

    /// 检测弧线：比较最近两章的关键词集差异。
    fn detect_arc(&self, chapters: &[Chapter]) -> String {
        if chapters.is_empty() {
            return String::new();
        }
        if chapters.len() == 1 {
            return chapters[0].title.clone();
        }
        let prev: Vec<&str> = chapters[chapters.len() - 2].summary.split('；').collect();
        let last: Vec<&str> = chapters[chapters.len() - 1].summary.split('；').collect();
        // 出现于最新章但未见于上一章的词 → 新方向
        let new_direction: Vec<&str> = last
            .iter()
            .filter(|s| !prev.contains(s) && !s.is_empty())
            .take(2)
            .cloned()
            .collect();
        if new_direction.is_empty() {
            format!("延续：{}", chapters.last().unwrap().title)
        } else {
            format!("转向：{}", new_direction.join(" → "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{Fact, MemoryContent, MemoryInput, MemoryMeta};
    use crate::l3::{BudgetConfig, L3Config};
    use tempfile::tempdir;

    fn make_l3() -> (tempfile::TempDir, L3Engine) {
        let dir = tempdir().unwrap();
        let engine = L3Engine::new(L3Config {
            storage_path: dir.path().join("narr.bin"),
            budget: BudgetConfig {
                daily_token_limit: 1_000_000,
                annual_storage_limit: 10_000_000,
            },
            ..Default::default()
        });
        (dir, engine)
    }

    #[test]
    fn test_empty_l3_produces_empty_narrative() {
        let (_dir, l3) = make_l3();
        let builder = NarrativeBuilder::new(NarrativeConfig::default());
        let n = builder.build(&l3, 0);
        assert!(n.chapters.is_empty());
    }

    #[test]
    fn test_build_chapterized_narrative() {
        let (_dir, l3) = make_l3();
        let now = now_nanos();
        // 注入 3 条高 importance 记忆（时间戳可控）
        for i in 0..3u64 {
            let mut input =
                MemoryInput::new(MemoryContent::Fact(Fact::new("user", "prefers", "rust")));
            input.importance = 0.9;
            let id = l3.insert(input).unwrap();
            l3.update_metadata(id, move |meta: &mut MemoryMeta| {
                meta.created_at = now - (i as i64 * 1000);
            });
        }
        let builder = NarrativeBuilder::new(NarrativeConfig {
            window_nanos: 1_000_000_000,
            ..Default::default()
        });
        let n = builder.build(&l3, now - 1_000_000_000);
        assert!(
            !n.chapters.is_empty(),
            "should produce at least one chapter"
        );
        assert!(!n.current_arc.is_empty());
        let total_sources: usize = n.chapters.iter().map(|c| c.source_ids.len()).sum();
        assert!(total_sources >= 1);
    }

    #[test]
    fn test_chapter_has_source_ids() {
        let (_dir, l3) = make_l3();
        let mut input = MemoryInput::new(MemoryContent::Summary(
            "user is learning distributed systems".into(),
        ));
        input.importance = 1.0;
        let id = l3.insert(input).unwrap();
        let builder = NarrativeBuilder::new(NarrativeConfig::default());
        let n = builder.build(&l3, 0);
        assert_eq!(n.chapters.len(), 1);
        assert!(n.chapters[0].source_ids.contains(&id));
    }
}
