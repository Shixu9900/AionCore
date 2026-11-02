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

/// Fraction of the page covered by images that are actually drawn.
///
/// A logo or a stamp is an XObject too, so the mere presence of an image must not
/// decide the page. Coverage is the area of each image's drawing transform over
/// the page area, which is what separates a full-page scan from an electronic
/// page that happens to contain a picture.
fn page_image_coverage(doc: &lopdf::Document, obj_id: lopdf::ObjectId) -> f64 {
    let Some(dict) = doc.get_object(obj_id).ok().and_then(|obj| obj.as_dict().ok()) else {
        return 0.0;
    };
    let media = inherited_box(doc, dict);
    let page_area = (media[2] - media[0]).abs() * (media[3] - media[1]).abs();
    if page_area <= f64::EPSILON {
        return 0.0;
    }
    let Some(resources) = inherited_dict(doc, dict, b"Resources") else {
        return 0.0;
    };
    let drawn = content_streams(doc, dict)
        .iter()
        .map(|bytes| drawn_image_area(doc, &resources, bytes))
        .sum::<f64>();
    (drawn / page_area).clamp(0.0, 1.0)
}

fn inherited_dict(doc: &lopdf::Document, start: &lopdf::Dictionary, key: &[u8]) -> Option<lopdf::Dictionary> {
    let mut current = Some(start.clone());
    for _ in 0..5 {
        let node = current?;
        if let Some(found) = resolve_dict(doc, &node, key) {
            return Some(found);
        }
        current = parent_dict(doc, &node);
    }
    None
}

fn inherited_box(doc: &lopdf::Document, start: &lopdf::Dictionary) -> [f64; 4] {
    let mut current = Some(start.clone());
    for _ in 0..5 {
        let Some(node) = current else { break };
        if let Some(nums) = node.get(b"MediaBox").ok().and_then(box_of) {
            return nums;
        }
        current = parent_dict(doc, &node);
    }
    [0.0, 0.0, 612.0, 792.0]
}

fn parent_dict(doc: &lopdf::Document, node: &lopdf::Dictionary) -> Option<lopdf::Dictionary> {
    match node.get(b"Parent").ok()? {
        lopdf::Object::Reference(id) => doc.get_object(*id).ok().and_then(|obj| obj.as_dict().ok().cloned()),
        lopdf::Object::Dictionary(dict) => Some(dict.clone()),
        _ => None,
    }
}

fn resolve_dict(doc: &lopdf::Document, dict: &lopdf::Dictionary, key: &[u8]) -> Option<lopdf::Dictionary> {
    match dict.get(key).ok()? {
        lopdf::Object::Dictionary(inner) => Some(inner.clone()),
        lopdf::Object::Reference(id) => doc.get_object(*id).ok().and_then(|obj| obj.as_dict().ok().cloned()),
        _ => None,
    }
}

fn box_of(obj: &lopdf::Object) -> Option<[f64; 4]> {
    let values = obj.as_array().ok()?;
    let nums: Vec<f64> = values
        .iter()
        .map(|value| f64::from(value.as_float().unwrap_or(0.0)))
        .collect();
    (nums.len() == 4).then(|| [nums[0], nums[1], nums[2], nums[3]])
}

fn content_streams(doc: &lopdf::Document, dict: &lopdf::Dictionary) -> Vec<Vec<u8>> {
    let Ok(contents) = dict.get(b"Contents") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match contents {
        lopdf::Object::Reference(id) => push_stream(doc, *id, &mut out),
        lopdf::Object::Stream(stream) => out.push(decode_stream(stream)),
        lopdf::Object::Array(items) => {
            for item in items {
                if let lopdf::Object::Reference(id) = item {
                    push_stream(doc, *id, &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

fn push_stream(doc: &lopdf::Document, id: lopdf::ObjectId, out: &mut Vec<Vec<u8>>) {
    if let Ok(lopdf::Object::Stream(stream)) = doc.get_object(id) {
        out.push(decode_stream(stream));
    }
}

fn decode_stream(stream: &lopdf::Stream) -> Vec<u8> {
    stream.decompressed_content().unwrap_or_else(|_| stream.content.clone())
}

/// Sum of the areas of the image XObjects the content stream draws, in page
/// points. Each image is drawn as the unit square under the current transform,
/// so its area is the absolute value of that transform's determinant.
fn drawn_image_area(doc: &lopdf::Document, resources: &lopdf::Dictionary, stream: &[u8]) -> f64 {
    let mut ctm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut saved: Vec<[f64; 6]> = Vec::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut last_name = String::new();
    let mut area = 0.0;
    for token in tokenize(stream) {
        match token {
            Token::Number(value) => operands.push(value),
            Token::Name(name) => last_name = name,
            Token::Operator(op) => {
                match op.as_str() {
                    "q" => saved.push(ctm),
                    "Q" => {
                        if let Some(prev) = saved.pop() {
                            ctm = prev;
                        }
                    }
                    "cm" => {
                        if operands.len() >= 6 {
                            let start = operands.len() - 6;
                            let m = &operands[start..];
                            ctm = multiply([m[0], m[1], m[2], m[3], m[4], m[5]], ctm);
                        }
                    }
                    "Do" => {
                        if xobject_is_image(doc, resources, &last_name) {
                            area += (ctm[0] * ctm[3] - ctm[1] * ctm[2]).abs();
                        }
                    }
                    _ => {}
                }
                operands.clear();
            }
        }
    }
    area
}

fn multiply(applied: [f64; 6], current: [f64; 6]) -> [f64; 6] {
    [
        applied[0] * current[0] + applied[1] * current[2],
        applied[0] * current[1] + applied[1] * current[3],
        applied[2] * current[0] + applied[3] * current[2],
        applied[2] * current[1] + applied[3] * current[3],
        applied[4] * current[0] + applied[5] * current[2] + current[4],
        applied[4] * current[1] + applied[5] * current[3] + current[5],
    ]
}

fn xobject_is_image(doc: &lopdf::Document, resources: &lopdf::Dictionary, name: &str) -> bool {
    let Some(xobjects) = resolve_dict(doc, resources, b"XObject") else {
        return false;
    };
    let Ok(obj) = xobjects.get(name.as_bytes()) else {
        return false;
    };
    let dict = match obj {
        lopdf::Object::Reference(id) => doc.get_object(*id).ok().and_then(|inner| match inner {
            lopdf::Object::Stream(stream) => Some(stream.dict.clone()),
            lopdf::Object::Dictionary(inner) => Some(inner.clone()),
            _ => None,
        }),
        lopdf::Object::Stream(stream) => Some(stream.dict.clone()),
        lopdf::Object::Dictionary(inner) => Some(inner.clone()),
        _ => None,
    };
    dict.and_then(|inner| {
        inner
            .get(b"Subtype")
            .ok()
            .and_then(|value| value.as_name().ok())
            .map(|name| name == b"Image")
    })
    .unwrap_or(false)
}

enum Token {
    Number(f64),
    Name(String),
    Operator(String),
}

fn tokenize(bytes: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'%' {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'(' {
            let mut depth = 1;
            index += 1;
            while index < bytes.len() && depth > 0 {
                match bytes[index] {
                    b'\\' => index += 2,
                    b'(' => {
                        depth += 1;
                        index += 1;
                    }
                    b')' => {
                        depth -= 1;
                        index += 1;
                    }
                    _ => index += 1,
                }
            }
            continue;
        }
        if byte == b'<' {
            if bytes.get(index + 1) == Some(&b'<') {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'>' && bytes[index + 1] == b'>') {
                    index += 1;
                }
                index += 2;
                continue;
            }
            while index < bytes.len() && bytes[index] != b'>' {
                index += 1;
            }
            index += 1;
            continue;
        }
        if byte == b'/' {
            index += 1;
            let start = index;
            while index < bytes.len() && !is_delimiter(bytes[index]) {
                index += 1;
            }
            out.push(Token::Name(String::from_utf8_lossy(&bytes[start..index]).into_owned()));
            continue;
        }
        if matches!(byte, b'[' | b']') {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && !is_delimiter(bytes[index]) {
            index += 1;
        }
        let word = &bytes[start..index];
        if word.is_empty() {
            index += 1;
            continue;
        }
        match std::str::from_utf8(word).unwrap_or("").parse::<f64>() {
            Ok(value) => out.push(Token::Number(value)),
            Err(_) => out.push(Token::Operator(String::from_utf8_lossy(word).into_owned())),
        }
    }
    out
}

fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'%'
        )
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
        doc.save_to(&mut bytes)
            .expect("saving a freshly built test PDF must not fail");
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
        assert_eq!(
            page_texts.len(),
            2,
            "page_texts must be index-aligned with rows, one entry per page"
        );

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

    /// A one-page PDF whose only image is `Im0`, drawn by `content`.
    fn pdf_with_image(content: &str) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 1i64,
                "Height" => 1i64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8i64,
            },
            vec![0],
        ));
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Im0" => image_id },
        });
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Count" => 1i64,
                "Kids" => vec![Object::from(page_id)],
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes)
            .expect("saving a freshly built test PDF must not fail");
        bytes
    }

    #[test]
    fn a_logo_does_not_make_an_electronic_page_a_scan() {
        let bytes = pdf_with_image(
            "50 0 0 40 28 740 cm /Im0 Do BT /F1 12 Tf 72 700 Td (This electronic page carries a real text layer and a small logo.) Tj ET",
        );
        let (_, rows, _) = classify_pdf(&bytes);
        assert!(
            rows[0].image_coverage < 0.85,
            "a small logo covered {:.3} of the page",
            rows[0].image_coverage
        );
        assert_eq!(rows[0].kind, "digital");
    }

    #[test]
    fn a_full_page_image_is_a_scan() {
        let bytes = pdf_with_image("612 0 0 792 0 0 cm /Im0 Do");
        let (_, rows, _) = classify_pdf(&bytes);
        assert!(
            rows[0].image_coverage >= 0.85,
            "a full-page image covered only {:.3} of the page",
            rows[0].image_coverage
        );
        assert_eq!(rows[0].kind, "clean_scan");
    }
}
