//! Read-only probe for an ERME data directory: counts, tag coverage, and a
//! scope-filtered keyword recall. Used by the ERME verification run; it never
//! writes to the store.
use std::sync::Arc;

use entropy_memory_engine::consolidation::ConsolidationConfig;
use entropy_memory_engine::l1::L1Cache;
use entropy_memory_engine::l2::{HnswConfig, L2Config, L2Engine};
use entropy_memory_engine::l3::{BudgetConfig, L3Config, L3Engine};
use entropy_memory_engine::memory_store::MemoryStore;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("data dir");
    let query = args.next().unwrap_or_default();
    let scope = args.next().unwrap_or_default();
    let dir = std::path::PathBuf::from(dir);
    let l3 = L3Engine::new(L3Config {
        storage_path: dir.join("erme_memory.bin"),
        budget: BudgetConfig::default(),
        ..Default::default()
    });
    let memories = l3.storage().all();
    let tagged = memories.iter().filter(|m| !m.tags.is_empty()).count();
    println!(
        "entries={} tagged={} untagged={} archived={}",
        memories.len(),
        tagged,
        memories.len() - tagged,
        memories
            .iter()
            .filter(|m| m.tags.iter().any(|t| t == "archived"))
            .count()
    );
    let mut tags: Vec<String> = memories.iter().flat_map(|m| m.tags.clone()).collect();
    tags.sort();
    tags.dedup();
    println!("tags={:?}", &tags[..tags.len().min(8)]);
    if query.is_empty() {
        return;
    }
    let l2 = L2Engine::new(L2Config {
        hnsw: HnswConfig {
            dimension: 384,
            ..Default::default()
        },
        dimension: 384,
        ..Default::default()
    });
    let store = MemoryStore::new(
        L1Cache::builder().capacity(100).build(),
        Arc::new(l2),
        l3,
        ConsolidationConfig::default(),
    );
    let hits = store
        .keyword_recall(&query, &[scope.clone()], 5)
        .expect("recall");
    println!("scope={scope} hits={}", hits.len());
    for (m, dist, _) in hits {
        println!(
            "  {dist:.2} tags={:?} {}",
            m.tags,
            m.content_text().chars().take(80).collect::<String>()
        );
    }
    let wrong = store
        .keyword_recall(&query, &["project:definitely-other".to_string()], 5)
        .expect("recall");
    println!("other-scope hits={} (must be 0)", wrong.len());
}
