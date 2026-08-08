use oz_mcp::{McpDiscovery, McpManager};

#[test]
fn test_multi_server_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("servers.toml");
    std::fs::write(&config_path, r#"
[[servers]]
name = "server-a"
command = "echo"
args = ["mcp-a"]
enabled = true

[[servers]]
name = "server-b"
command = "echo"
args = ["mcp-b"]
enabled = true

[[servers]]
name = "disabled-server"
command = "echo"
args = ["disabled"]
enabled = false
"#).unwrap();

    let mut discovery = McpDiscovery::new(&config_path);
    let count = discovery.load().unwrap();
    assert_eq!(count, 3);
    assert_eq!(discovery.enabled_servers().len(), 2);
    assert!(discovery.find("server-a").is_some());
    assert!(discovery.find("server-b").is_some());
    assert!(discovery.find("disabled-server").is_none());

    let manager = McpManager::from_discovery(&discovery);
    assert_eq!(manager.connected_count(), 0);
    assert_eq!(manager.tool_count(), 0);
}

#[test]
fn test_default_config_template() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("servers.toml");
    let discovery = McpDiscovery::new(&config_path);
    discovery.ensure_default().unwrap();
    assert!(config_path.exists());
    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("playwright"));
    assert!(content.contains("filesystem"));
}
