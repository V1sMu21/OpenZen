use std::path::Path;

pub fn load_tools_schema(
    assets_dir: &Path,
) -> Result<serde_json::Value, oz_core_types::ConfigError> {
    let path = assets_dir.join("tools_schema.json");
    let content = std::fs::read_to_string(&path).map_err(|e| {
        oz_core_types::ConfigError::LoadFailed(format!("Failed to read {}: {e}", path.display()))
    })?;
    serde_json::from_str(&content).map_err(|e| {
        oz_core_types::ConfigError::LoadFailed(format!("Failed to parse tools_schema.json: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn load_tools_schema_nonexistent_dir() {
        let result = load_tools_schema(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_err());

        if let Err(oz_core_types::ConfigError::LoadFailed(msg)) = result {
            assert!(msg.contains("tools_schema.json"));
        } else {
            panic!("Expected LoadFailed error");
        }
    }

    #[test]
    fn load_tools_schema_dir_exists_but_no_file() {
        let tmp_dir = env::temp_dir().join("oz_schema_test_empty_dir");
        fs::create_dir_all(&tmp_dir).unwrap();

        let result = load_tools_schema(&tmp_dir);
        assert!(result.is_err());

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn load_tools_schema_valid_minimal() {
        let tmp_dir = env::temp_dir().join("oz_schema_test_valid");
        fs::create_dir_all(&tmp_dir).unwrap();

        fs::write(
            tmp_dir.join("tools_schema.json"),
            r#"{"type":"object","properties":{},"required":[]}"#,
        )
        .unwrap();

        let result = load_tools_schema(&tmp_dir);
        assert!(result.is_ok());

        let value = result.unwrap();
        assert_eq!(value["type"], "object");

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn load_tools_schema_with_properties() {
        let tmp_dir = env::temp_dir().join("oz_schema_test_props");
        fs::create_dir_all(&tmp_dir).unwrap();

        let schema = r#"
        {
          "type": "object",
          "properties": {
            "name": { "type": "string" },
            "count": { "type": "integer" }
          },
          "required": ["name"]
        }
        "#;
        fs::write(tmp_dir.join("tools_schema.json"), schema).unwrap();

        let result = load_tools_schema(&tmp_dir);
        assert!(result.is_ok());

        let value = result.unwrap();
        assert_eq!(value["properties"]["name"]["type"], "string");
        assert_eq!(value["properties"]["count"]["type"], "integer");
        assert_eq!(value["required"][0], "name");

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    #[test]
    fn load_tools_schema_invalid_json() {
        let tmp_dir = env::temp_dir().join("oz_schema_test_invalid");
        fs::create_dir_all(&tmp_dir).unwrap();

        fs::write(tmp_dir.join("tools_schema.json"), "not valid json {{{").unwrap();

        let result = load_tools_schema(&tmp_dir);
        assert!(result.is_err());

        if let Err(oz_core_types::ConfigError::LoadFailed(msg)) = result {
            assert!(msg.contains("parse"));
        } else {
            panic!("Expected LoadFailed error for invalid JSON");
        }

        fs::remove_dir_all(&tmp_dir).unwrap();
    }
}
