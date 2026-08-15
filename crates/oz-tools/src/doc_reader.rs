use std::io::Read;
use std::path::Path;

use base64::Engine;

pub fn read_document(path: &str, start: usize, count: usize) -> Result<serde_json::Value, String> {
    let p = Path::new(path);
    let ext = p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "pdf" => read_pdf(path, start, count),
        "xlsx" | "xls" => read_xlsx(path, start, count),
        "docx" => read_docx(path, start, count),
        "pptx" => read_pptx(path, start, count),
        other => Err(format!("unsupported document format: .{other}")),
    }
}

pub fn is_supported_doc(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "pdf" | "xlsx" | "xls" | "docx" | "pptx")
}

pub fn is_image(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp")
}

pub fn media_type_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

pub fn read_image_base64(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("cannot read image: {e}"))?;

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mime = media_type_for(&ext);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn slice_lines(text: &str, start: usize, count: usize) -> serde_json::Value {
    let lines: Vec<&str> = text.lines().collect();
    let from = start.saturating_sub(1).min(lines.len());
    let to = (from + count).min(lines.len());
    let excerpt = lines[from..to].join("\n");
    serde_json::json!({
        "content": excerpt,
        "total_lines": lines.len(),
        "start_line": from + 1,
        "end_line": to,
    })
}

fn read_pdf(path: &str, start: usize, count: usize) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read PDF: {e}"))?;
    let text = pdf_extract::extract_text_from_mem(&bytes)
        .map_err(|e| format!("PDF parse error: {e}"))?;
    Ok(slice_lines(&text, start, count))
}

fn read_xlsx(path: &str, start: usize, count: usize) -> Result<serde_json::Value, String> {
    use calamine::{open_workbook, Reader, Xlsx};

    let mut workbook: Xlsx<_> = open_workbook(path)
        .map_err(|e| format!("cannot open Excel file: {e}"))?;

    let sheet_names = workbook.sheet_names().to_vec();
    let mut output = String::new();

    for sheet_name in &sheet_names {
        if let Ok(range) = workbook.worksheet_range(sheet_name) {
            output.push_str(&format!("\n## Sheet: {sheet_name}\n\n"));

            let rows_iter = range.rows();
            let mut is_first = true;
            let mut col_count = 0;

            for row in rows_iter {
                if col_count == 0 {
                    col_count = row.len();
                }
                let cells: Vec<String> = row.iter()
                    .map(|c| c.to_string().trim().to_string())
                    .collect();

                if is_first {
                    output.push_str("| ");
                    output.push_str(&cells.join(" | "));
                    output.push_str(" |\n");
                    output.push('|');
                    for _ in 0..col_count {
                        output.push_str(" --- |");
                    }
                    output.push('\n');
                    is_first = false;
                } else {
                    output.push_str("| ");
                    output.push_str(&cells.join(" | "));
                    output.push_str(" |\n");
                }
            }

            if is_first {
                output.push_str("_(empty sheet)_\n");
            }
        }
    }

    Ok(slice_lines(&output, start, count))
}

fn read_docx(path: &str, start: usize, count: usize) -> Result<serde_json::Value, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("cannot open DOCX: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("cannot read DOCX (not a valid ZIP): {e}"))?;

    let mut doc_xml = String::new();
    {
        let mut entry = archive.by_name("word/document.xml")
            .map_err(|_| "DOCX missing word/document.xml".to_string())?;
        entry.read_to_string(&mut doc_xml)
            .map_err(|e| format!("cannot read document.xml: {e}"))?;
    }

    let text = extract_docx_text(&doc_xml);
    Ok(slice_lines(&text, start, count))
}

fn extract_docx_text(xml: &str) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut output = String::new();
    let mut in_paragraph = false;
    let mut para_text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name.as_str() == "w:p" {
                    in_paragraph = true;
                    para_text.clear();
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "w:p" && in_paragraph {
                    in_paragraph = false;
                    let trimmed = para_text.trim();
                    if !trimmed.is_empty() {
                        output.push_str(trimmed);
                        output.push('\n');
                    }
                }
            }
            Ok(Event::Text(ref e))
                if in_paragraph => {
                    if let Ok(t) = e.unescape() {
                        para_text.push_str(&t);
                    }
                }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    output.trim().to_string()
}

fn read_pptx(path: &str, start: usize, count: usize) -> Result<serde_json::Value, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("cannot open PPTX: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("cannot read PPTX (not a valid ZIP): {e}"))?;

    let mut output = String::new();
    let mut slide_num = 0u32;

    loop {
        slide_num += 1;
        let entry_name = format!("ppt/slides/slide{}.xml", slide_num);
        let mut slide_xml = String::new();

        match archive.by_name(&entry_name) {
            Ok(mut entry) => {
                entry.read_to_string(&mut slide_xml)
                    .map_err(|e| format!("cannot read {entry_name}: {e}"))?;
            }
            Err(_) => break,
        }

        let slide_text = extract_pptx_slide_text(&slide_xml);
        let trimmed = slide_text.trim();
        if !trimmed.is_empty() {
            output.push_str(&format!("\n--- Slide {slide_num} ---\n"));
            output.push_str(trimmed);
            output.push('\n');
        }
    }

    if output.is_empty() {
        output = "(no text content found in slides)".to_string();
    }

    Ok(slice_lines(&output, start, count))
}

fn extract_pptx_slide_text(xml: &str) -> String {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "a:t" {
                    if let Ok(Event::Text(ref t)) = reader.read_event_into(&mut buf) {
                        if let Ok(txt) = t.unescape() {
                            let trimmed = txt.trim();
                            if !trimmed.is_empty() {
                                text_parts.push(trimmed.to_string());
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    text_parts.join("\n")
}
