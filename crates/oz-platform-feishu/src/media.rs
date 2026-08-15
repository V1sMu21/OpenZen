use std::path::Path;

use crate::client::FeishuClient;

const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "tif",
];

pub async fn send_local_file(
    client: &FeishuClient,
    receive_id: &str,
    file_path: &str,
    receive_id_type: &str,
) -> Result<(), String> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("file not found: {file_path}"));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if IMAGE_EXTS.contains(&ext.as_str()) {
        let image_key = client.upload_image(path).await?;
        let content = serde_json::json!({ "image_key": image_key }).to_string();
        client
            .send_raw(receive_id, &content, "image", receive_id_type)
            .await
            .map(|_| ())
    } else {
        let file_key = client.upload_file(path).await?;
        let content = serde_json::json!({ "file_key": file_key }).to_string();
        client
            .send_raw(receive_id, &content, "file", receive_id_type)
            .await
            .map(|_| ())
    }
}
