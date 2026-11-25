use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use lopdf::{Document as LoDocument, Object as LoObject, content::Content};
use pdfsink_rs::{
    BBox, Char, Page, PdfDocument, Point, Table, TableSettings, TableStrategy, TextOptions,
    extract_text_lines,
};

use crate::{
    Block, BlockContent, BlockId, Document, DocumentError, DocumentParser, Result, SourceSpan,
    dehyphenate_multiline, has_extension,
};

pub struct PdfParser;

impl DocumentParser for PdfParser {
    fn supports(&self, path: &Path) -> bool {
        has_extension(path, &["pdf"])
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let pdf = PdfDocument::from_bytes(bytes)
            .map_err(|error| DocumentError::InvalidPdf(error.to_string()))?;
        if pdf.pages.is_empty() {
            return Err(DocumentError::InvalidPdf(
                "документ не содержит страниц".into(),
            ));
        }

        let has_any_text = pdf
            .pages
            .iter()
            .any(|page| page.chars.iter().any(|ch| !ch.text.trim().is_empty()));
        if !has_any_text {
            return Err(DocumentError::EmptyPdfText);
        }

        let page_heights: Vec<f64> = pdf.pages.iter().map(|page| page.height.max(1.0)).collect();
        let draw_styles = extract_pdf_text_draw_styles(bytes, &page_heights);

        let mut pages_data = Vec::with_capacity(pdf.pages.len());
        for (index, page) in pdf.pages.iter().enumerate() {
            let page_draws = draw_styles.get(index).map_or(&[][..], Vec::as_slice);
            pages_data.push(extract_page_data(page, index + 1, page_draws)?);
        }

        strip_headers_footers_and_page_numbers(&mut pages_data);

        let body_font_size = compute_body_font_size(&pages_data);
        let heading_tiers = compute_heading_tiers(&pages_data, body_font_size);

        let mut blocks = Vec::new();
        for page in pages_data {
            build_page_blocks(page, body_font_size, &heading_tiers, &mut blocks);
        }

        if blocks.is_empty() {
            return Err(DocumentError::EmptyPdfText);
        }

        let title = blocks
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Heading { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        Ok(Document {
            title,
            source_path: PathBuf::new(),
            blocks,
        })
    }
}

struct PageData {
    page_number: usize,
    height: f64,
    tables: Vec<ExtractedTable>,
    lines: Vec<ExtractedLine>,
}

struct ExtractedTable {
    bbox: BBox,
    rows: Vec<Vec<String>>,
}

#[derive(Clone)]
struct ExtractedLine {
    text: String,
    x0: f64,
    top: f64,
    bottom: f64,
    font_size: f64,
    is_bold: bool,
    is_mono: bool,
}

fn page_text_options(page: &Page) -> TextOptions {
    TextOptions {
        layout_bbox: Some(page.bbox),
        layout_width: Some(page.width),
        layout_height: Some(page.height),
        ..Default::default()
    }
}

#[derive(Clone, Copy, Default)]
struct FontStyle {
    is_bold: bool,
    is_mono: bool,
    is_composite: bool,
}

#[derive(Clone, Copy)]
struct TextDrawStyle {
    y_from_top: f64,
    style: FontStyle,
    weight: usize,
}

fn extract_page_data(
    page: &Page,
    page_number: usize,
    page_draws: &[TextDrawStyle],
) -> Result<PageData> {
    let text_options = page_text_options(page);
    let tables = extract_page_tables(page, &text_options)?;

    let non_table_chars: Vec<Char> = page
        .chars
        .iter()
        .filter(|ch| {
            let center = Point::new((ch.x0 + ch.x1) / 2.0, (ch.top + ch.bottom) / 2.0);
            !tables
                .iter()
                .any(|table| table.bbox.expand(1.5, 1.5).contains_point(center))
        })
        .cloned()
        .collect();

    let raw_lines = extract_text_lines(&non_table_chars, &text_options, true, true);
    let mut lines = Vec::with_capacity(raw_lines.len());
    for line in raw_lines {
        let text = normalize_line_text(&line.text);
        if text.is_empty() {
            continue;
        }
        let chars = line.chars.as_deref().unwrap_or(&[]);
        let (font_size, is_bold, is_mono) =
            analyze_line_style(chars, line.top, line.bottom, page_draws);
        lines.push(ExtractedLine {
            text,
            x0: line.x0,
            top: line.top,
            bottom: line.bottom,
            font_size,
            is_bold,
            is_mono,
        });
    }
    lines.sort_by(|a, b| a.top.total_cmp(&b.top).then_with(|| a.x0.total_cmp(&b.x0)));

    Ok(PageData {
        page_number,
        height: page.height.max(1.0),
        tables,
        lines,
    })
}

fn extract_page_tables(page: &Page, text_options: &TextOptions) -> Result<Vec<ExtractedTable>> {
    let settings = TableSettings {
        text_options: text_options.clone(),
        ..Default::default()
    };
    let mut raw_tables = page
        .find_tables(settings.clone())
        .map_err(|error| DocumentError::InvalidPdf(error.to_string()))?;

    let mut used_text_fallback = false;
    if raw_tables.is_empty() && has_horizontal_table_rules(page) {
        let mut fallback = settings;
        fallback.vertical_strategy = TableStrategy::Text;
        fallback.horizontal_strategy = TableStrategy::Lines;
        fallback.min_words_vertical = 2;
        if let Ok(found) = page.find_tables(fallback) {
            raw_tables = found;
            used_text_fallback = true;
        }
    }

    let mut tables = Vec::new();
    for table in raw_tables {
        if let Some(extracted) = clean_table(page, &table, text_options, used_text_fallback) {
            tables.push(extracted);
        }
    }
    tables.sort_by(|a, b| {
        a.bbox
            .top
            .total_cmp(&b.bbox.top)
            .then_with(|| a.bbox.x0.total_cmp(&b.bbox.x0))
    });
    Ok(tables)
}

fn has_horizontal_table_rules(page: &Page) -> bool {
    let horizontal_lines = page
        .lines
        .iter()
        .filter(|line| line.width >= 40.0 && line.height <= 3.0)
        .count();
    let horizontal_rects = page
        .rects
        .iter()
        .filter(|rect| rect.width >= 40.0 && rect.height <= 3.0)
        .count();
    horizontal_lines + horizontal_rects >= 2
}

fn clean_table(
    page: &Page,
    table: &Table,
    text_options: &TextOptions,
    strict_validation: bool,
) -> Option<ExtractedTable> {
    let raw_rows = table.extract(page, text_options);
    let mut rows: Vec<Vec<String>> = raw_rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.map_or_else(String::new, |text| normalize_cell_text(&text)))
                .collect::<Vec<_>>()
        })
        .collect();

    if rows.len() < 2 {
        return None;
    }
    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count < 2 {
        return None;
    }
    for row in &mut rows {
        row.resize(col_count, String::new());
    }

    let total_cells = rows.len() * col_count;
    let non_empty_cells = rows
        .iter()
        .flatten()
        .filter(|cell| !cell.is_empty())
        .count();
    if non_empty_cells == 0 {
        return None;
    }
    if strict_validation && non_empty_cells * 2 < total_cells {
        return None;
    }

    Some(ExtractedTable {
        bbox: table.bbox,
        rows,
    })
}

fn analyze_line_style(
    chars: &[Char],
    line_top: f64,
    line_bottom: f64,
    page_draws: &[TextDrawStyle],
) -> (f64, bool, bool) {
    let mut sizes = Vec::new();
    let mut known_font_alpha = 0usize;
    let mut bold_count = 0usize;
    let mut mono_count = 0usize;

    for ch in chars {
        if ch.text.trim().is_empty() {
            continue;
        }
        if ch.size.is_finite() && ch.size > 0.0 {
            sizes.push(ch.size);
        }
        if ch.text.chars().any(char::is_alphanumeric) && ch.fontname != "unknown" {
            known_font_alpha += 1;
            let lower = ch.fontname.to_lowercase();
            if is_bold_fontname(&lower) {
                bold_count += 1;
            }
            if is_mono_fontname(&lower) {
                mono_count += 1;
            }
        }
    }

    let font_size = if sizes.is_empty() {
        (line_bottom - line_top).max(1.0)
    } else {
        sizes.sort_by(f64::total_cmp);
        sizes[sizes.len() / 2]
    };

    if known_font_alpha > 0 {
        return (
            font_size,
            bold_count * 2 > known_font_alpha,
            mono_count * 2 > known_font_alpha,
        );
    }

    // pdfsink-rs оставляет Char.fontname = "unknown", поэтому восстанавливаем
    // стиль (bold/mono) по длине текста операторов контента PDF на той же полосе.
    let mut draw_total = 0usize;
    let mut draw_bold = 0usize;
    let mut draw_mono = 0usize;
    for draw in page_draws {
        if draw.y_from_top >= line_top - 3.0 && draw.y_from_top <= line_bottom + 3.0 {
            draw_total += draw.weight;
            if draw.style.is_bold {
                draw_bold += draw.weight;
            }
            if draw.style.is_mono {
                draw_mono += draw.weight;
            }
        }
    }
    let is_bold = draw_total > 0 && draw_bold * 2 > draw_total;
    let is_mono = draw_total > 0 && draw_mono * 2 > draw_total;
    (font_size, is_bold, is_mono)
}

fn extract_pdf_text_draw_styles(bytes: &[u8], page_heights: &[f64]) -> Vec<Vec<TextDrawStyle>> {
    let Ok(lo_doc) = LoDocument::load_mem(bytes) else {
        return Vec::new();
    };
    let pages = lo_doc.get_pages();
    let mut result = Vec::with_capacity(pages.len());

    for (index, &page_id) in pages.values().enumerate() {
        let page_height = page_heights.get(index).copied().unwrap_or(842.0);
        let mut font_styles: HashMap<Vec<u8>, FontStyle> = HashMap::new();
        if let Ok(fonts) = lo_doc.get_page_fonts(page_id) {
            for (name, dict) in fonts {
                font_styles.insert(name, inspect_font_dict(&lo_doc, dict));
            }
        }

        let mut draws = Vec::new();
        if let Ok(raw_content) = lo_doc.get_page_content(page_id)
            && let Ok(content) = Content::decode(&raw_content)
        {
            let mut ctm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            let mut style = FontStyle::default();
            let mut stack: Vec<([f64; 6], FontStyle)> = Vec::new();
            let mut tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            let mut tlm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            let mut leading = 0.0f64;

            for op in &content.operations {
                match op.operator.as_str() {
                    "q" => stack.push((ctm, style)),
                    "Q" => {
                        if let Some((saved_ctm, saved_style)) = stack.pop() {
                            ctm = saved_ctm;
                            style = saved_style;
                        }
                    }
                    "cm" if op.operands.len() == 6 => {
                        if let Some(m) = operands_matrix(&op.operands) {
                            ctm = mul_matrix(ctm, m);
                        }
                    }
                    "BT" => {
                        tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                        tlm = tm;
                    }
                    "Tf" if !op.operands.is_empty() => {
                        if let Ok(name) = op.operands[0].as_name() {
                            style = font_styles.get(name).copied().unwrap_or_default();
                        }
                    }
                    "TL" if !op.operands.is_empty() => {
                        if let Some(val) = operand_f64(&op.operands[0]) {
                            leading = val;
                        }
                    }
                    "Tm" if op.operands.len() == 6 => {
                        if let Some(m) = operands_matrix(&op.operands) {
                            tm = m;
                            tlm = m;
                        }
                    }
                    "Td" if op.operands.len() == 2 => {
                        if let (Some(tx), Some(ty)) =
                            (operand_f64(&op.operands[0]), operand_f64(&op.operands[1]))
                        {
                            tlm = mul_matrix(tlm, [1.0, 0.0, 0.0, 1.0, tx, ty]);
                            tm = tlm;
                        }
                    }
                    "TD" if op.operands.len() == 2 => {
                        if let (Some(tx), Some(ty)) =
                            (operand_f64(&op.operands[0]), operand_f64(&op.operands[1]))
                        {
                            leading = -ty;
                            tlm = mul_matrix(tlm, [1.0, 0.0, 0.0, 1.0, tx, ty]);
                            tm = tlm;
                        }
                    }
                    "T*" => {
                        tlm = mul_matrix(tlm, [1.0, 0.0, 0.0, 1.0, 0.0, -leading]);
                        tm = tlm;
                    }
                    "Tj" | "TJ" | "'" | "\"" => {
                        if matches!(op.operator.as_str(), "'" | "\"") {
                            tlm = mul_matrix(tlm, [1.0, 0.0, 0.0, 1.0, 0.0, -leading]);
                            tm = tlm;
                        }
                        let weight = draw_text_weight(op, style.is_composite);
                        if weight > 0 {
                            let combined = mul_matrix(ctm, tm);
                            draws.push(TextDrawStyle {
                                y_from_top: page_height - combined[5],
                                style,
                                weight,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        result.push(draws);
    }

    result
}

fn draw_text_weight(op: &lopdf::content::Operation, is_composite: bool) -> usize {
    match op.operator.as_str() {
        "Tj" | "'" => op
            .operands
            .first()
            .map_or(0, |operand| operand_text_weight(operand, is_composite)),
        "\"" => op
            .operands
            .get(2)
            .map_or(0, |operand| operand_text_weight(operand, is_composite)),
        "TJ" => op
            .operands
            .first()
            .and_then(|operand| operand.as_array().ok())
            .map_or(0, |items| {
                items
                    .iter()
                    .map(|item| operand_text_weight(item, is_composite))
                    .sum()
            }),
        _ => 0,
    }
}

fn operand_text_weight(operand: &LoObject, is_composite: bool) -> usize {
    let LoObject::String(bytes, _) = operand else {
        return 0;
    };
    if bytes.is_empty() {
        return 0;
    }
    if is_composite {
        (bytes.len() / 2).max(1)
    } else {
        bytes.len()
    }
}

fn apply_font_name(name: &[u8], style: &mut FontStyle) {
    let lower = String::from_utf8_lossy(name).to_lowercase();
    style.is_bold |= is_bold_fontname(&lower);
    style.is_mono |= is_mono_fontname(&lower);
}

fn inspect_font_dict(doc: &LoDocument, dict: &lopdf::Dictionary) -> FontStyle {
    let mut style = FontStyle {
        is_composite: dict
            .get(b"Subtype")
            .and_then(LoObject::as_name)
            .is_ok_and(|subtype| subtype == b"Type0"),
        ..FontStyle::default()
    };
    if let Ok(base_font) = dict.get(b"BaseFont").and_then(LoObject::as_name) {
        apply_font_name(base_font, &mut style);
    }
    inspect_font_descriptor(doc, dict, &mut style);

    if let Ok(descendants) = dict.get(b"DescendantFonts") {
        let resolved = resolve_object(doc, descendants);
        if let Ok(array) = resolved.as_array() {
            for item in array {
                if let Ok(cid_dict) = resolve_object(doc, item).as_dict() {
                    if let Ok(base_font) = cid_dict.get(b"BaseFont").and_then(LoObject::as_name) {
                        apply_font_name(base_font, &mut style);
                    }
                    inspect_font_descriptor(doc, cid_dict, &mut style);
                }
            }
        }
    }
    style
}

fn inspect_font_descriptor(doc: &LoDocument, dict: &lopdf::Dictionary, style: &mut FontStyle) {
    let Ok(desc_obj) = dict.get(b"FontDescriptor") else {
        return;
    };
    let Ok(desc) = resolve_object(doc, desc_obj).as_dict() else {
        return;
    };
    if let Ok(font_name) = desc.get(b"FontName").and_then(LoObject::as_name) {
        let lower = String::from_utf8_lossy(font_name).to_lowercase();
        style.is_bold |= is_bold_fontname(&lower);
        style.is_mono |= is_mono_fontname(&lower);
    }
    if let Ok(weight) = desc
        .get(b"FontWeight")
        .and_then(|obj| obj.as_i64().or_else(|_| obj.as_f32().map(|v| v as i64)))
        && weight >= 600
    {
        style.is_bold = true;
    }
    if let Ok(flags) = desc.get(b"Flags").and_then(LoObject::as_i64) {
        // ISO 32000-1 Table 123: bit 1 (value 1) = FixedPitch, bit 19 (1 << 18) = ForceBold.
        if (flags & 1) != 0 {
            style.is_mono = true;
        }
        if (flags & (1 << 18)) != 0 {
            style.is_bold = true;
        }
    }
}

fn resolve_object<'a>(doc: &'a LoDocument, obj: &'a LoObject) -> &'a LoObject {
    doc.dereference(obj).map_or(obj, |(_, resolved)| resolved)
}

fn operand_f64(obj: &LoObject) -> Option<f64> {
    match obj {
        LoObject::Real(value) => Some(f64::from(*value)),
        LoObject::Integer(value) => Some(*value as f64),
        _ => None,
    }
}

fn operands_matrix(operands: &[LoObject]) -> Option<[f64; 6]> {
    Some([
        operand_f64(&operands[0])?,
        operand_f64(&operands[1])?,
        operand_f64(&operands[2])?,
        operand_f64(&operands[3])?,
        operand_f64(&operands[4])?,
        operand_f64(&operands[5])?,
    ])
}

fn mul_matrix(left: [f64; 6], right: [f64; 6]) -> [f64; 6] {
    [
        left[0] * right[0] + left[2] * right[1],
        left[1] * right[0] + left[3] * right[1],
        left[0] * right[2] + left[2] * right[3],
        left[1] * right[2] + left[3] * right[3],
        left[0] * right[4] + left[2] * right[5] + left[4],
        left[1] * right[4] + left[3] * right[5] + left[5],
    ]
}

fn is_bold_fontname(lower: &str) -> bool {
    lower.contains("bold")
        || lower.contains("black")
        || lower.contains("heavy")
        || lower.contains("demi")
        || lower.contains("semibold")
        || lower.ends_with("-bd")
        || lower.ends_with("_bd")
        || lower.ends_with(",bold")
}

fn is_mono_fontname(lower: &str) -> bool {
    lower.contains("mono")
        || lower.contains("courier")
        || lower.contains("consolas")
        || lower.contains("menlo")
        || lower.contains("fixed")
}

fn strip_headers_footers_and_page_numbers(pages: &mut [PageData]) {
    let page_count = pages.len();
    let mut top_counts: HashMap<String, usize> = HashMap::new();
    let mut bottom_counts: HashMap<String, usize> = HashMap::new();

    if page_count >= 2 {
        for page in pages.iter() {
            let mut seen_top = HashSet::new();
            let mut seen_bottom = HashSet::new();
            for line in &page.lines {
                if is_top_margin(line, page.height) {
                    let sig = running_chrome_signature(&line.text);
                    if !sig.is_empty() && seen_top.insert(sig.clone()) {
                        *top_counts.entry(sig).or_default() += 1;
                    }
                }
                if is_bottom_margin(line, page.height) {
                    let sig = running_chrome_signature(&line.text);
                    if !sig.is_empty() && seen_bottom.insert(sig.clone()) {
                        *bottom_counts.entry(sig).or_default() += 1;
                    }
                }
            }
        }
    }

    let threshold = page_count.div_ceil(2).max(2);

    for page in pages.iter_mut() {
        let height = page.height;
        page.lines.retain(|line| {
            let in_top = is_top_margin(line, height);
            let in_bottom = is_bottom_margin(line, height);
            if !in_top && !in_bottom {
                return true;
            }
            if is_page_number(&line.text) {
                return false;
            }
            if page_count >= 2 {
                let sig = running_chrome_signature(&line.text);
                if in_top
                    && top_counts
                        .get(&sig)
                        .is_some_and(|&count| count >= threshold)
                {
                    return false;
                }
                if in_bottom
                    && bottom_counts
                        .get(&sig)
                        .is_some_and(|&count| count >= threshold)
                {
                    return false;
                }
            }
            true
        });
    }
}

fn is_top_margin(line: &ExtractedLine, page_height: f64) -> bool {
    line.top <= page_height * 0.10
}

fn is_bottom_margin(line: &ExtractedLine, page_height: f64) -> bool {
    line.bottom >= page_height * 0.90
}

fn running_chrome_signature(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_digit = false;
    for ch in text.trim().to_lowercase().chars() {
        if ch.is_ascii_digit() {
            if !last_was_digit {
                out.push('#');
                last_was_digit = true;
            }
        } else if ch.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            last_was_digit = false;
        } else {
            out.push(ch);
            last_was_digit = false;
        }
    }
    out.trim().to_owned()
}

fn is_page_number(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 32 {
        return false;
    }
    let stripped = trimmed
        .trim_matches(|ch: char| matches!(ch, '-' | '–' | '—' | '[' | ']' | '(' | ')' | ' '))
        .trim();
    if !stripped.is_empty() && stripped.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if let Some((left, right)) = stripped.split_once('/') {
        let left = left.trim();
        let right = right.trim();
        if !left.is_empty()
            && !right.is_empty()
            && left.bytes().all(|b| b.is_ascii_digit())
            && right.bytes().all(|b| b.is_ascii_digit())
        {
            return true;
        }
    }
    let lower = stripped.to_lowercase();
    for prefix in ["стр.", "стр ", "страница ", "page ", "p. "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            let valid = match tokens.as_slice() {
                [num] => num.bytes().all(|b| b.is_ascii_digit()),
                [num, sep, total] => {
                    num.bytes().all(|b| b.is_ascii_digit())
                        && matches!(*sep, "из" | "of" | "/")
                        && total.bytes().all(|b| b.is_ascii_digit())
                }
                _ => false,
            };
            if valid {
                return true;
            }
        }
    }
    false
}

fn compute_body_font_size(pages: &[PageData]) -> f64 {
    let mut histogram: HashMap<i64, usize> = HashMap::new();
    for page in pages {
        for line in &page.lines {
            let bucket = (line.font_size * 2.0).round() as i64;
            let weight = line.text.chars().count().max(1);
            *histogram.entry(bucket).or_default() += weight;
        }
    }
    histogram
        .into_iter()
        .max_by_key(|&(_, weight)| weight)
        .map_or(11.0, |(bucket, _)| (bucket as f64) / 2.0)
}

fn compute_heading_tiers(pages: &[PageData], body_font_size: f64) -> Vec<f64> {
    let threshold = body_font_size * 1.15;
    let mut buckets: Vec<i64> = pages
        .iter()
        .flat_map(|page| page.lines.iter())
        .filter(|line| line.font_size >= threshold)
        .map(|line| (line.font_size * 2.0).round() as i64)
        .collect();
    buckets.sort_unstable();
    buckets.dedup();
    buckets.reverse();
    buckets.into_iter().map(|b| (b as f64) / 2.0).collect()
}

fn is_tight_bold_continuation(upper: &ExtractedLine, lower: &ExtractedLine) -> bool {
    if !upper.is_bold || !lower.is_bold || (upper.font_size - lower.font_size).abs() >= 0.5 {
        return false;
    }
    let line_height = (upper.bottom - upper.top).max(upper.font_size);
    let gap = lower.top - upper.bottom;
    gap <= line_height * 0.85
}

fn tier_heading_level_for_line(line: &ExtractedLine, heading_tiers: &[f64]) -> Option<u8> {
    if line.is_mono || strip_bullet(&line.text).is_some() {
        return None;
    }
    let rounded = (line.font_size * 2.0).round() / 2.0;
    heading_tiers
        .iter()
        .position(|&tier| (tier - rounded).abs() < 0.01)
        .map(|index| index.saturating_add(1).min(6) as u8)
}

fn bold_subheading_run_end(
    lines: &[ExtractedLine],
    start: usize,
    body_font_size: f64,
    heading_tiers: &[f64],
) -> Option<(usize, u8)> {
    let first = &lines[start];
    if !first.is_bold
        || first.is_mono
        || strip_bullet(&first.text).is_some()
        || first.font_size < body_font_size * 0.98
        || tier_heading_level_for_line(first, heading_tiers).is_some()
    {
        return None;
    }
    // Жирный подзаголовок обычного кегля засчитывается, только если это начало сегмента,
    // перед группой есть увеличенный отступ (> 0.85 высоты строки) или предыдущая строка
    // завершена знаком `. : ; ! ?`, и строка не продолжает предыдущую bold-группу.
    if start > 0 {
        let prev = &lines[start - 1];
        if is_tight_bold_continuation(prev, first) {
            return None;
        }
        let prev_height = (prev.bottom - prev.top).max(prev.font_size);
        let gap_from_prev = first.top - prev.bottom;
        let has_break_before =
            gap_from_prev > prev_height * 0.85 || prev.text.ends_with(['.', ':', ';', '!', '?']);
        if !has_break_before {
            return None;
        }
    }

    let mut end = start;
    while end + 1 < lines.len() {
        let next = &lines[end + 1];
        if !next.is_mono
            && strip_bullet(&next.text).is_none()
            && tier_heading_level_for_line(next, heading_tiers).is_none()
            && is_tight_bold_continuation(&lines[end], next)
        {
            end += 1;
        } else {
            break;
        }
    }

    // Группа из 1–2 плотно идущих bold-строк одного размера (≤ 100 символов, не оканчивается
    // знаком конца предложения) — один подзаголовок; группа из 3+ строк остаётся абзацем.
    if end - start + 1 > 2 {
        return None;
    }

    let parts: Vec<&str> = lines[start..=end]
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    let joined = join_pdf_lines(&parts);
    if joined.chars().count() > 100 || joined.ends_with(['.', '!', '?', ';']) {
        return None;
    }

    let level = heading_tiers.len().saturating_add(1).clamp(1, 6) as u8;
    Some((end, level))
}

enum PositionedContent {
    Block { top: f64, content: BlockContent },
}

fn build_page_blocks(
    page: PageData,
    body_font_size: f64,
    heading_tiers: &[f64],
    blocks: &mut Vec<Block>,
) {
    let mut items: Vec<PositionedContent> = Vec::new();
    let table_ranges: Vec<(f64, f64)> = page
        .tables
        .iter()
        .map(|table| (table.bbox.top, table.bbox.bottom))
        .collect();

    for table in page.tables {
        items.push(PositionedContent::Block {
            top: table.bbox.top,
            content: BlockContent::Table {
                rows: table.rows,
                row_lines: Vec::new(),
            },
        });
    }

    // Разделяем текстовые строки по границам таблиц, чтобы абзацы не склеивались через таблицу.
    let mut segment_start = 0usize;
    let mut segments: Vec<&[ExtractedLine]> = Vec::new();
    for (index, line) in page.lines.iter().enumerate() {
        if index > segment_start {
            let prev = &page.lines[index - 1];
            let separated_by_table = table_ranges
                .iter()
                .any(|&(t_top, t_bottom)| prev.bottom <= t_top + 2.0 && line.top >= t_bottom - 2.0);
            if separated_by_table {
                segments.push(&page.lines[segment_start..index]);
                segment_start = index;
            }
        }
    }
    if segment_start < page.lines.len() {
        segments.push(&page.lines[segment_start..]);
    }

    for segment in segments {
        group_segment_lines(segment, body_font_size, heading_tiers, &mut items);
    }

    items.sort_by(|a, b| match (a, b) {
        (PositionedContent::Block { top: a, .. }, PositionedContent::Block { top: b, .. }) => {
            a.total_cmp(b)
        }
    });

    let mut page_line = 1usize;
    for PositionedContent::Block { mut content, .. } in items {
        let (start_line, end_line) = match &mut content {
            BlockContent::Table { rows, row_lines } => {
                let start = page_line;
                row_lines.clear();
                row_lines.reserve(rows.len());
                for _ in rows.iter() {
                    row_lines.push(page_line);
                    page_line += 1;
                }
                (start, page_line - 1)
            }
            BlockContent::Heading { text, .. }
            | BlockContent::Paragraph { text }
            | BlockContent::ListItem { text }
            | BlockContent::Code { text, .. } => {
                let count = text.lines().count().max(1);
                let start = page_line;
                let end = start + count - 1;
                page_line = end + 1;
                (start, end)
            }
        };

        blocks.push(Block {
            id: BlockId(blocks.len() + 1),
            location: SourceSpan {
                start_line,
                end_line,
                page: Some(page.page_number),
            },
            content,
        });
    }
}

fn group_segment_lines(
    lines: &[ExtractedLine],
    body_font_size: f64,
    heading_tiers: &[f64],
    out: &mut Vec<PositionedContent>,
) {
    let starts_heading = |idx: usize| {
        tier_heading_level_for_line(&lines[idx], heading_tiers).is_some()
            || bold_subheading_run_end(lines, idx, body_font_size, heading_tiers).is_some()
    };

    let mut index = 0usize;
    while index < lines.len() {
        let line = &lines[index];

        // 1a. Заголовок увеличенного кегля
        if let Some(level) = tier_heading_level_for_line(line, heading_tiers) {
            let start_top = line.top;
            let mut parts = vec![line.text.as_str()];
            let mut prev = line;
            index += 1;
            while let Some(candidate) = lines.get(index) {
                let gap = candidate.top - prev.bottom;
                let line_height = (prev.bottom - prev.top).max(prev.font_size);
                if tier_heading_level_for_line(candidate, heading_tiers) == Some(level)
                    && gap <= line_height * 0.85
                {
                    parts.push(candidate.text.as_str());
                    prev = candidate;
                    index += 1;
                } else {
                    break;
                }
            }
            out.push(PositionedContent::Block {
                top: start_top,
                content: BlockContent::Heading {
                    level,
                    text: join_pdf_lines(&parts),
                },
            });
            continue;
        }

        // 1b. Полужирный подзаголовок обычного кегля (1–2 плотно идущих строки)
        if let Some((end, level)) =
            bold_subheading_run_end(lines, index, body_font_size, heading_tiers)
        {
            let parts: Vec<&str> = lines[index..=end]
                .iter()
                .map(|item| item.text.as_str())
                .collect();
            out.push(PositionedContent::Block {
                top: line.top,
                content: BlockContent::Heading {
                    level,
                    text: join_pdf_lines(&parts),
                },
            });
            index = end + 1;
            continue;
        }

        // 2. Блок кода (моноширинный шрифт)
        if line.is_mono {
            let start_top = line.top;
            let mut code_lines = vec![line.text.as_str()];
            let mut prev = line;
            index += 1;
            while let Some(candidate) = lines.get(index) {
                let gap = candidate.top - prev.bottom;
                let line_height = (prev.bottom - prev.top).max(prev.font_size);
                if candidate.is_mono && gap <= line_height * 1.2 {
                    code_lines.push(candidate.text.as_str());
                    prev = candidate;
                    index += 1;
                } else {
                    break;
                }
            }
            out.push(PositionedContent::Block {
                top: start_top,
                content: BlockContent::Code {
                    language: None,
                    text: code_lines.join("\n"),
                },
            });
            continue;
        }

        // 3. Элемент списка
        if let Some(first_text) = strip_bullet(&line.text) {
            let start_top = line.top;
            let mut parts = vec![first_text];
            let mut prev = line;
            index += 1;
            while let Some(candidate) = lines.get(index) {
                if candidate.is_mono
                    || strip_bullet(&candidate.text).is_some()
                    || starts_heading(index)
                {
                    break;
                }
                let gap = candidate.top - prev.bottom;
                let line_height = (prev.bottom - prev.top).max(prev.font_size);
                if gap <= line_height * 0.85 {
                    parts.push(candidate.text.as_str());
                    prev = candidate;
                    index += 1;
                } else {
                    break;
                }
            }
            out.push(PositionedContent::Block {
                top: start_top,
                content: BlockContent::ListItem {
                    text: join_pdf_lines(&parts),
                },
            });
            continue;
        }

        // 4. Абзац
        let start_top = line.top;
        let mut parts = vec![line.text.as_str()];
        let mut prev = line;
        index += 1;
        while let Some(candidate) = lines.get(index) {
            if candidate.is_mono || strip_bullet(&candidate.text).is_some() || starts_heading(index)
            {
                break;
            }
            let gap = candidate.top - prev.bottom;
            let line_height = (prev.bottom - prev.top).max(prev.font_size);
            let new_paragraph_indent =
                candidate.x0 - prev.x0 > body_font_size * 1.5 && prev.text.ends_with('.');
            if gap <= line_height * 0.85 && !new_paragraph_indent {
                parts.push(candidate.text.as_str());
                prev = candidate;
                index += 1;
            } else {
                break;
            }
        }
        out.push(PositionedContent::Block {
            top: start_top,
            content: BlockContent::Paragraph {
                text: join_pdf_lines(&parts),
            },
        });
    }
}

fn strip_bullet(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let first = trimmed.chars().next()?;
    // Тире (`–`, `—`, `-`) не считаем маркером списка, чтобы не разрывать абзац при переносе строки с тире.
    if matches!(first, '•' | '●' | '○' | '▪' | '▫' | '■' | '*') {
        let after = &trimmed[first.len_utf8()..];
        if after.chars().next().is_some_and(char::is_whitespace) {
            let rest = after.trim_start();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

fn normalize_line_text(text: &str) -> String {
    text.replace('\u{00A0}', " ")
        .replace('\u{00AD}', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_cell_text(text: &str) -> String {
    dehyphenate_multiline(text)
        .replace('\u{00A0}', " ")
        .replace('\u{00AD}', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn join_pdf_lines(lines: &[&str]) -> String {
    normalize_cell_text(&lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_wrapped_words_and_preserves_compound_hyphens() {
        for (wrapped, expected) in [
            ("QR-\nкода", "QR-кода"),
            ("SMS-\nкода", "SMS-кода"),
            ("про-\nверка", "проверка"),
            ("API-\nшлюз", "API-шлюз"),
            ("FR-\n12", "FR-12"),
            ("онлайн-\nплатёж", "онлайн-платёж"),
            ("из-\nза", "из-за"),
            ("из-\nпод", "из-под"),
            ("кто-\nто", "кто-то"),
            ("Какого-\nто.", "Какого-то."),
            ("что-\nнибудь", "что-нибудь"),
            ("по-\nпрежнему", "по-прежнему"),
            ("по-\nдругому", "по-другому"),
            ("по-\nрусски", "по-русски"),
            ("по-\nнемецки", "по-немецки"),
            ("по-\nволчьи", "по-волчьи"),
            ("по-\nтом", "потом"),
            ("по-\nтому", "потому"),
            ("по-\nэтому", "поэтому"),
            ("по-\nчему", "почему"),
            ("провер-\nка", "проверка"),
            ("Интернет-\nбанк", "Интернет-банк"),
            ("x-\nлуч", "x-луч"),
            ("3-\nдневный", "3-дневный"),
            ("про\u{00ad}\nверка", "проверка"),
        ] {
            assert_eq!(normalize_cell_text(wrapped), expected);
            assert_eq!(crate::normalize_quote_dehyphenated(wrapped), expected);
        }
    }

    #[test]
    fn parses_wrapped_compound_words_in_qr_sample() {
        let document = PdfParser
            .parse(include_bytes!("../../../samples/qr-payments.pdf"))
            .unwrap();
        for (anchor, quote) in [
            ("Минимальная сумма платежа", "QR-кода"),
            ("Порог запроса", "SMS-кода"),
            ("FR-12.", "QR-платежи"),
        ] {
            assert!(
                document.blocks.iter().any(|block| match &block.content {
                    BlockContent::Table { rows, .. } => rows.iter().any(|row| {
                        let text = row.join(" ");
                        text.contains(anchor) && text.contains(quote)
                    }),
                    BlockContent::Paragraph { text } | BlockContent::ListItem { text } =>
                        text.contains(anchor) && text.contains(quote),
                    _ => false,
                }),
                "{anchor}: {quote}"
            );
        }
        for (wrapped, joined) in [("SMS-\nкода", "SMS-кода"), ("QR-\nплатежи", "QR-платежи")]
        {
            assert!(
                document.blocks.iter().any(|block| {
                    document.contains_quote(block.location, wrapped)
                        && document.contains_quote(block.location, joined)
                }),
                "{wrapped}"
            );
        }
    }

    #[test]
    fn heading_level_saturates_without_u8_overflow_for_many_font_tiers() {
        let tiers: Vec<f64> = (0..300).rev().map(|i| 12.0 + i as f64 * 0.5).collect();
        let line = ExtractedLine {
            text: "Заголовок".into(),
            x0: 50.0,
            top: 100.0,
            bottom: 112.0,
            font_size: 12.0,
            is_bold: true,
            is_mono: false,
        };
        assert_eq!(tier_heading_level_for_line(&line, &tiers), Some(6));

        let bold_body = ExtractedLine {
            text: "Подзаголовок".into(),
            x0: 50.0,
            top: 100.0,
            bottom: 110.0,
            font_size: 10.0,
            is_bold: true,
            is_mono: false,
        };
        assert_eq!(
            bold_subheading_run_end(&[bold_body], 0, 10.0, &tiers),
            Some((0, 6))
        );
    }
}
