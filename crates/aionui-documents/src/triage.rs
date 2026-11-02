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

/// Classifies a document into pages. The returned `Vec<String>` is index-aligned
/// with the returned `Vec<PageRow>`: entry `i` is page `rows[i]`'s own text, never
/// the whole document's text. Callers must not concatenate or broadcast these.
pub fn classify_path(path: &Path, bytes: &[u8]) -> (u32, Vec<PageRow>, Vec<String>) {
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
        _ => (1, vec![blank_page(1, PageKind::Unknown, 0, 0.0, 0.0)], vec![String::new()]),
    }
}

fn classify_pdf(bytes: &[u8]) -> (u32, Vec<PageRow>, Vec<String>) {
    let Ok(doc) = lopdf::Document::load_mem(bytes) else {
        return (1, vec![blank_page(1, PageKind::Unknown, 0, 1.0, 0.0)], vec![String::new()]);
    };
    let pages = doc.get_pages();
    let mut rows = Vec::new();
    let mut page_texts = Vec::new();
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
        page_texts.push(text);
    }
    if rows.is_empty() {
        rows.push(blank_page(1, PageKind::Unknown, 0, 0.0, 0.0));
        page_texts.push(String::new());
    }
    (rows.len() as u32, rows, page_texts)
}

fn classify_office(bytes: &[u8], _ext: &str) -> (u32, Vec<PageRow>, Vec<String>) {
    let text = office_text(bytes);
    let chars = text.chars().count() as u32;
    (
        1,
        vec![blank_page(1, PageKind::Office, chars, 0.0, garbled_ratio(&text))],
        vec![text],
    )
}

fn classify_sheet(bytes: &[u8], ext: &str) -> (u32, Vec<PageRow>, Vec<String>) {
    let text = if ext == "csv" {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        sheet_text(bytes)
    };
    let chars = text.chars().count() as u32;
    (1, vec![blank_page(1, PageKind::Spreadsheet, chars, 0.0, 0.0)], vec![text])
}

fn classify_image() -> (u32, Vec<PageRow>, Vec<String>) {
    (1, vec![blank_page(1, PageKind::Image, 0, 1.0, 0.0)], vec![String::new()])
}

fn unsupported_old_office() -> (u32, Vec<PageRow>, Vec<String>) {
    (1, vec![blank_page(1, PageKind::Unknown, 0, 0.0, 0.0)], vec![String::new()])
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

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Document, Object, Stream, dictionary};

    /// A two-page PDF where each page's content stream draws a distinct marker
    /// string, so a test can tell whether `classify_pdf` leaked one page's text
    /// into the other.
    fn two_page_pdf(page_one_marker: &str, page_two_marker: &str) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let mut kids = Vec::new();
        for marker in [page_one_marker, page_two_marker] {
            let content = format!("BT /F1 24 Tf 72 712 Td ({marker}) Tj ET");
            let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            });
            kids.push(page_id.into());
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Count" => kids.len() as i64,
                "Kids" => kids,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("saving a freshly built test PDF must not fail");
        bytes
    }

    /// Regression test for the bug where every page's reader input was the whole
    /// document's concatenated text (triage.rs previously returned one `all_text`
    /// String instead of per-page text), which both bloated what got indexed per
    /// page and let a field reader match a value from the wrong page.
    #[test]
    fn each_pdf_page_keeps_only_its_own_text() {
        let bytes = two_page_pdf("PAGE-ONE-MARKER TSN 29189", "PAGE-TWO-MARKER CSN 4585");
        let (page_count, rows, page_texts) = classify_pdf(&bytes);

        assert_eq!(page_count, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(page_texts.len(), 2, "page_texts must be index-aligned with rows, one entry per page");

        assert!(page_texts[0].contains("PAGE-ONE-MARKER"));
        assert!(
            !page_texts[0].contains("PAGE-TWO-MARKER"),
            "page 1's text must not contain page 2's marker: {:?}",
            page_texts[0]
        );

        assert!(page_texts[1].contains("PAGE-TWO-MARKER"));
        assert!(
            !page_texts[1].contains("PAGE-ONE-MARKER"),
            "page 2's text must not contain page 1's marker: {:?}",
            page_texts[1]
        );
    }

    #[test]
    fn single_page_classifiers_still_return_one_text_entry_per_row() {
        let (count, rows, texts) = classify_office(b"not a real docx, triage just needs it not to panic", "docx");
        assert_eq!(count, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(texts.len(), 1);
    }
}
