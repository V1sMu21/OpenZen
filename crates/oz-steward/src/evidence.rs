//! Image evidence for visual decisions. Files are read locally and sent as
//! data URIs; there is no upload path and no network fetch.

use std::path::Path;

use base64::Engine;

use crate::error::{Result, StewardError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInput {
    pub mime: String,
    pub base64: String,
}

impl ImageInput {
    pub fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.base64)
    }
}

pub fn mime_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

pub fn load_image(path: impl AsRef<Path>, max_bytes: u64) -> Result<ImageInput> {
    let path = path.as_ref();
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes.max(1) {
        return Err(StewardError::Invalid(format!(
            "image `{}` is {} bytes (limit {})",
            path.display(),
            meta.len(),
            max_bytes
        )));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| StewardError::Invalid("image has no extension".into()))?;
    let mime = mime_for_extension(ext)
        .ok_or_else(|| StewardError::Invalid(format!("unsupported image extension `{ext}`")))?;
    let bytes = std::fs::read(path)?;
    Ok(ImageInput {
        mime: mime.into(),
        base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

pub fn load_images(paths: &[impl AsRef<Path>], max_bytes: u64) -> Result<Vec<String>> {
    paths
        .iter()
        .map(|p| load_image(p, max_bytes).map(|i| i.data_uri()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn png_becomes_data_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"\x89PNG\r\n\x1a\nfake").unwrap();
        let img = load_image(&path, 1024).unwrap();
        assert_eq!(img.mime, "image/png");
        assert!(img.data_uri().starts_with("data:image/png;base64,"));
    }

    #[test]
    fn oversize_and_unknown_extension_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, vec![0u8; 64]).unwrap();
        assert!(load_image(&path, 8).is_err());
        let bad = dir.path().join("a.bmp");
        std::fs::write(&bad, b"x").unwrap();
        assert!(load_image(&bad, 1024).is_err());
    }
}
