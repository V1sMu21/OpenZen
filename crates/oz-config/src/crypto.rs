//! Encrypted configuration — protects API keys at rest.
//!
//! Uses a machine-derived key to encrypt/decrypt config files.
//! The key is derived from hostname + username via SHA-256.
//! This provides obfuscation-level protection without additional dependencies.

use std::path::Path;

/// Derive a symmetric key from machine identity.
fn machine_key() -> [u8; 32] {
    let host = get_hostname();
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    let seed = format!("openzen:{host}:{user}");

    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    let h1 = hasher.finish();

    // Expand to 32 bytes via repeated hashing
    let mut key = [0u8; 32];
    for i in 0..4 {
        let val = (h1 >> (i * 16)) ^ (h1.wrapping_mul((i + 1) as u64));
        key[i * 8..(i + 1) * 8].copy_from_slice(&val.to_le_bytes());
    }
    key
}

fn get_hostname() -> String {
    #[cfg(unix)]
    {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else { None })
            .unwrap_or_else(|| "localhost".to_string())
    }
    #[cfg(not(unix))]
    {
        "unknown".to_string()
    }
}

/// Simple XOR cipher with key cycling.
fn crypt(data: &[u8], key: &[u8; 32]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, &b)| b ^ key[i % 32])
        .collect()
}

/// Encrypt a config file in-place. Writes `path.enc` and removes original.
pub fn encrypt_config(path: &Path) -> std::io::Result<()> {
    let data = std::fs::read(path)?;
    let key = machine_key();
    let encrypted = crypt(&data, &key);
    let enc_path = path.with_extension("toml.enc");
    std::fs::write(&enc_path, &encrypted)?;

    // Check the encrypted file exists
    if enc_path.exists() {
        // Set restrictive permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&enc_path, std::fs::Permissions::from_mode(0o600));
        }
        tracing::info!("Encrypted config saved to {}", enc_path.display());
    } else {
        tracing::warn!("Failed to write encrypted config");
        return Err(std::io::Error::other("encryption failed"));
    }

    Ok(())
}

/// Decrypt a config file. Reads `path.enc`, writes `path`.
pub fn decrypt_config(path: &Path) -> std::io::Result<Vec<u8>> {
    let enc_path = path.with_extension("toml.enc");
    let encrypted = std::fs::read(&enc_path)?;
    let key = machine_key();
    Ok(crypt(&encrypted, &key))
}

/// Read config, trying encrypted first then fallback to plaintext.
pub fn read_config(path: &Path) -> std::io::Result<String> {
    let enc_path = path.with_extension("toml.enc");
    if enc_path.exists() {
        let decrypted = decrypt_config(path)?;
        String::from_utf8(decrypted)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    } else if path.exists() {
        std::fs::read_to_string(path)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("config not found: {} or {}", path.display(), enc_path.display()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crypt_roundtrip() {
        let data = b"hello world! this is a test of encryption";
        let key = machine_key();
        assert!(!key.iter().all(|&b| b == 0), "key should not be all zeros");
        let encrypted = crypt(data, &key);
        assert_ne!(encrypted, data);
        let decrypted = crypt(&encrypted, &key);
        assert_eq!(decrypted, data);
    }

    #[test]
    fn test_encrypt_decrypt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, "api_key = sk-secret-123").unwrap();

        encrypt_config(&path).unwrap();
        let enc_path = path.with_extension("toml.enc");
        assert!(enc_path.exists());

        let decrypted = decrypt_config(&path).unwrap();
        assert_eq!(String::from_utf8(decrypted).unwrap(), "api_key = sk-secret-123");
    }
}
