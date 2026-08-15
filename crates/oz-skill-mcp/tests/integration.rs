#[cfg(test)]
mod e2e_tests {
    use oz_core_types::{SkillMcpMetadata, SkillMcpType};
    use oz_skill_mcp::{
        migration::{is_migrated, migrate_memory_to_skill_mcp},
        skill::Skill,
        MatchConfig, Matcher, MetaStore, SkillMcpMemory, SkillMcpStore, StalenessChecker,
    };
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    fn setup_store() -> (tempfile::TempDir, SkillMcpStore) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SkillMcpStore::new(dir.path(), None);
        let s = Skill {
            name: "web_search".into(),
            description: "Search the web".into(),
            tags: vec!["web".into()],
            required_tools: vec![],
            content: "# web_search — Search the web\n\n## Procedure\n1. Search\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("web_search", "desc", vec![]),
            quality: 0.8,
        };
        store.skills.register(s).unwrap();
        let s2 = Skill {
            name: "file_reader".into(),
            description: "Read files".into(),
            tags: vec!["file".into()],
            required_tools: vec![],
            content: "# file_reader — Read files\n\n## Procedure\n1. Read\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("file_reader", "desc", vec![]),
            quality: 0.6,
        };
        store.skills.register(s2).unwrap();
        (dir, store)
    }

    #[test]
    fn test_e2e_skill_lifecycle() {
        let (_dir, mut store) = setup_store();
        let matched = store.find_skills("search the internet");
        assert!(!matched.is_empty());
        assert_eq!(matched[0].name, "web_search");

        store.record_skill_success("web_search", 3).unwrap();
        store.record_skill_success("web_search", 2).unwrap();
        let skill = store.skills.get("web_search").unwrap();
        assert_eq!(skill.metadata.success_count, 3);

        store.record_skill_failure("web_search").unwrap();
        assert_eq!(
            store
                .skills
                .get("web_search")
                .unwrap()
                .metadata
                .failure_count,
            1
        );

        let seq = vec![("read".to_string(), serde_json::json!({"path": "/tmp"}))];
        store.crystallise_sop("check", "Check", &seq, None).unwrap();
        assert_eq!(store.sop_count(), 1);
        assert!(!store.find_sops("check").is_empty());

        store.reload().unwrap();
        assert_eq!(store.skill_count(), 2);
    }

    #[test]
    fn test_e2e_build_context() {
        let (_dir, mut store) = setup_store();
        let seq = vec![("grep".to_string(), serde_json::json!({"pattern": "t"}))];
        store
            .crystallise_sop("grep_test", "Run grep", &seq, None)
            .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let ctx = rt.block_on(store.build_context("search", Path::new("/tmp"), None));
        assert!(ctx.contains("web_search"));
        assert!(!ctx.contains("file_reader"));

        let ctx2 = rt.block_on(store.build_context("read file", Path::new("/tmp"), None));
        assert!(ctx2.contains("file_reader"));
    }

    #[test]
    fn test_metadata_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SkillMcpStore::new(dir.path(), None);
        let s = Skill {
            name: "persistent".into(),
            description: "".into(),
            tags: vec![],
            required_tools: vec![],
            content: "# persistent\n\nSteps.\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("persistent", "", vec![]),
            quality: 0.5,
        };
        store.skills.register(s).unwrap();
        store.record_skill_success("persistent", 5).unwrap();
        store.record_skill_success("persistent", 3).unwrap();

        let store2 = SkillMcpStore::new(dir.path(), None);
        assert_eq!(store2.skill_count(), 1);
        let loaded = store2.skills.get("persistent").unwrap();
        assert_eq!(loaded.metadata.success_count, 3);
    }

    #[test]
    fn test_migration_full() {
        let dir = tempfile::tempdir().unwrap();
        let mem = dir.path().join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("global_mem.txt"), "data\n").unwrap();
        std::fs::write(mem.join("global_mem_insight.txt"), "insight\n").unwrap();
        std::fs::write(mem.join("sop_test.md"), "# SOP\n").unwrap();
        let l4 = mem.join("L4_raw_sessions");
        std::fs::create_dir_all(&l4).unwrap();
        std::fs::write(l4.join("s.md"), "session\n").unwrap();

        assert!(!is_migrated(dir.path()));
        let r = migrate_memory_to_skill_mcp(dir.path()).unwrap();
        assert!(r.facts_migrated);
        assert!(r.insights_migrated);
        assert!(r.sops_migrated > 0);
        assert!(r.sessions_migrated > 0);
        assert!(is_migrated(dir.path()));
    }

    #[test]
    fn test_matching_performance() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SkillMcpStore::new(dir.path(), None);
        for i in 0..50 {
            let name = format!("skill_{:02}", i);
            let s = Skill {
                name: name.clone(),
                description: format!("s{}", i),
                tags: vec![format!("t{}", i % 5)],
                required_tools: vec![],
                content: format!("# {}\n\nDo.\n", name),
                source_path: PathBuf::new(),
                metadata: SkillMcpMetadata::new(&name, "", vec![]),
                quality: 0.5,
            };
            store.skills.register(s).unwrap();
        }
        assert_eq!(store.skill_count(), 50);
        let start = Instant::now();
        for _ in 0..100 {
            let _ = store.find_skills("search the web for information");
        }
        assert!(start.elapsed().as_millis() < 50);
    }

    #[test]
    fn test_staleness_scan() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SkillMcpStore::new(dir.path(), None);

        let bad = Skill {
            name: "bad".into(),
            description: "".into(),
            tags: vec![],
            required_tools: vec![],
            content: "# bad\n\nX.\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("bad", "", vec![]),
            quality: 0.5,
        };
        store.skills.register(bad).unwrap();

        if let Some(s) = store.skills.get_mut("bad") {
            s.metadata.quality_score = 0.15;
            s.quality = 0.15;
            store.meta.save("skills", "bad", &s.metadata).unwrap();
        }

        let good = Skill {
            name: "good".into(),
            description: "".into(),
            tags: vec![],
            required_tools: vec![],
            content: "# good\n\nY.\n".into(),
            source_path: PathBuf::new(),
            metadata: SkillMcpMetadata::new("good", "", vec![]),
            quality: 0.9,
        };
        store.skills.register(good).unwrap();

        let checker = StalenessChecker::new(store.base_dir(), None);
        let stale = checker.scan_all().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "bad");
    }

    #[tokio::test]
    async fn test_memory_operations() {
        let dir = tempfile::tempdir().unwrap();
        let mem = SkillMcpMemory::new(dir.path());
        mem.append_fact("k1").await.unwrap();
        mem.append_fact("k1").await.unwrap();
        assert_eq!(mem.read_facts().await.unwrap().matches("k1").count(), 1);
        mem.write_insight("w").await.unwrap();
        assert!(mem.read_insight().await.unwrap().contains("w"));
        assert!(mem.archive_session("d").await.unwrap().exists());
    }

    #[test]
    fn test_meta_cross_category() {
        let dir = tempfile::tempdir().unwrap();
        let meta = MetaStore::new(dir.path());
        meta.save("skills", "a", &SkillMcpMetadata::new("a", "", vec![]))
            .unwrap();
        meta.save("sops", "b", &SkillMcpMetadata::new("b", "", vec![]))
            .unwrap();
        assert_eq!(meta.list_category("skills").unwrap().len(), 1);
        assert_eq!(meta.list_category("sops").unwrap().len(), 1);
    }

    #[test]
    fn test_matcher_edge_cases() {
        assert_eq!(Matcher::keyword_overlap("", ""), 0.0);
        assert_eq!(Matcher::keyword_overlap("a", "b"), 0.0);
        assert!(Matcher::keyword_overlap("hello", "hello world") > 0.0);
    }

    #[test]
    fn test_skill_mcp_type() {
        for (kt, s, d) in [
            (SkillMcpType::Skill, "skill", "skills"),
            (SkillMcpType::Fact, "fact", "facts"),
        ] {
            assert_eq!(kt.as_str(), s);
            assert_eq!(kt.as_dir(), d);
        }
    }
}
