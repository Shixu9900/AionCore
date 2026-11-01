use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::types::{PageKind, PageRow};

pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64, Vec<u8>)> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok((hex::encode(hasher.finalize()), bytes.len() as u64, bytes))
}

pub fn classify_path(path: &Path, bytes: &[u8]) -> (u32, Vec<PageRow>, String) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => classify_pdf(bytes),
        "docx" | "pptx" => classify_office(bytes, ext.as_str()),
        "doc" | "ppt" => unsupported_old_office(),
        "xlsx" | "xls" | "csv" => classify_sheet(bytes, ext.as_str()),
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" => classify_image(),
        _ => (
            1,
            vec![blank_page(1, PageKind::Unknown, 0, 0.0, 0.0)],
            String::new(),
        ),
    }
}

fn classify_pdf(bytes: &[u8]) -> (u32, Vec<PageRow>, String) {
    let Ok(doc) = lopdf::Document::load_mem(bytes) else {
        return (
            1,
            vec![blank_page(1, PageKind::Unknown, 0, 1.0, 0.0)],
            String::new(),
        );
    };
    let pages = doc.get_pages();
    let mut rows = Vec::new();
    let mut all_text = String::new();
    for (index, (page_id, obj_id)) in pages.iter().enumerate() {
        let page_no = index as u32 + 1;
        let text = doc.extract_text(&[*page_id]).unwrap_or_default();
        let text_chars = text.chars().count() as u32;
        let garbled = garbled_ratio(&text);
        let image_coverage = page_image_coverage(&doc, *obj_id);
        let rotation = page_rotation(&doc, *obj_id);
        let kind = if image_coverage >= 0.85 && text_chars > 40 {
            // Full-page image plus a text layer: treat as a scan with an old OCR overlay.
            PageKind::CleanScan
        } else if image_coverage >= 0.85 || text_chars < 20 {
            PageKind::CleanScan
        } else {
            PageKind::Digital
        };
        all_text.push_str(&text);
        all_text.push('\n');
        rows.push(PageRow {
            sha256: String::new(),
            page_no,
            kind: kind.as_str().into(),
            rotation,
            skew_degrees: None,
            text_chars,
            image_coverage,
            garbled_ratio: garbled,
            status: "pending".into(),
            confidence: None,
            provider: Some("triage".into()),
        });
    }
    if rows.is_empty() {
        rows.push(blank_page(1, PageKind::Unknown, 0, 0.0, 0.0));
    }
    (rows.len() as u32, rows, all_text)
}

fn classify_office(bytes: &[u8], _ext: &str) -> (u32, Vec<PageRow>, String) {
    let text = office_text(bytes);
    let chars = text.chars().count() as u32;
    (
        1,
        vec![blank_page(1, PageKind::Office, chars, 0.0, garbled_ratio(&text))],
        text,
    )
}

fn classify_sheet(bytes: &[u8], ext: &str) -> (u32, Vec<PageRow>, String) {
    let text = if ext == "csv" {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        sheet_text(bytes)
    };
    let chars = text.chars().count() as u32;
    (
        1,
        vec![blank_page(1, PageKind::Spreadsheet, chars, 0.0, 0.0)],
        text,
    )
}

fn classify_image() -> (u32, Vec<PageRow>, String) {
    (1, vec![blank_page(1, PageKind::Image, 0, 1.0, 0.0)], String::new())
}

fn unsupported_old_office() -> (u32, Vec<PageRow>, String) {
    (1, vec![blank_page(1, PageKind::Unknown, 0, 0.0, 0.0)], String::new())
}

fn blank_page(page_no: u32, kind: PageKind, text_chars: u32, image_coverage: f64, garbled: f64) -> PageRow {
    PageRow {
        sha256: String::new(),
        page_no,
        kind: kind.as_str().into(),
        rotation: 0,
        skew_degrees: None,
        text_chars,
        image_coverage,
        garbled_ratio: garbled,
        status: "pending".into(),
        confidence: None,
        provider: Some("triage".into()),
    }
}

fn garbled_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let bad = text.chars().filter(|c| matches!(c, '\u{FFFD}' | '□' | '■')).count();
    bad as f64 / text.chars().count() as f64
}

fn page_rotation(doc: &lopdf::Document, obj_id: lopdf::ObjectId) -> i32 {
    doc.get_object(obj_id)
        .ok()
        .and_then(|obj| obj.as_dict().ok())
        .and_then(|dict| dict.get(b"Rotate").ok())
        .and_then(|obj| obj.as_i64().ok())
        .unwrap_or(0) as i32
}

fn page_image_coverage(doc: &lopdf::Document, obj_id: lopdf::ObjectId) -> f64 {
    let Ok(obj) = doc.get_object(obj_id) else {
        return 0.0;
    };
    let Ok(dict) = obj.as_dict() else {
        return 0.0;
    };
    let resources = dict.get(b"Resources").ok().and_then(|r| match r {
        lopdf::Object::Dictionary(d) => Some(d.clone()),
        lopdf::Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_dict().ok().cloned()),
        _ => None,
    });
    let Some(resources) = resources else {
        return 0.0;
    };
    let xobject = resources.get(b"XObject").ok().and_then(|r| match r {
        lopdf::Object::Dictionary(d) => Some(d.len()),
        lopdf::Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_dict().ok().map(|d| d.len())),
        _ => None,
    });
    match xobject {
        Some(n) if n > 0 => 0.9,
        _ => 0.0,
    }
}

fn office_text(bytes: &[u8]) -> String {
    let cursor = std::io::Cursor::new(bytes);
    let Ok(mut archive) = zip::ZipArchive::new(cursor) else {
        return String::new();
    };
    let mut out = String::new();
    for i in 0..archive.len() {
        let Ok(mut file) = archive.by_index(i) else {
            continue;
        };
        let name = file.name().to_string();
        if !name.ends_with(".xml") {
            continue;
        }
        let mut xml = String::new();
        if file.read_to_string(&mut xml).is_err() {
            continue;
        }
        for piece in xml.split("<w:t").skip(1) {
            if let Some(rest) = piece.split_once('>')
                && let Some(text) = rest.1.split("</w:t>").next()
            {
                out.push_str(text);
                out.push(' ');
            }
        }
        for piece in xml.split("<a:t").skip(1) {
            if let Some(rest) = piece.split_once('>')
                && let Some(text) = rest.1.split("</a:t>").next()
            {
                out.push_str(text);
                out.push(' ');
            }
        }
    }
    out
}

fn sheet_text(bytes: &[u8]) -> String {
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let Ok(mut workbook) = calamine::open_workbook_auto_from_rs(cursor) else {
        return String::new();
    };
    use calamine::Reader;
    let mut out = String::new();
    let names = workbook.sheet_names().to_vec();
    for name in names {
        if let Ok(range) = workbook.worksheet_range(&name) {
            for row in range.rows() {
                for cell in row {
                    out.push_str(&cell.to_string());
                    out.push('\t');
                }
                out.push('\n');
            }
        }
    }
    out
}
