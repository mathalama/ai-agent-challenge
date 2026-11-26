use std::{
    borrow::Cow,
    cmp::Reverse,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use lopdf::{Document as LoDocument, Object as LoObject, content::Content};
use pdfsink_rs::{
    BBox, Char, Page, PdfDocument, Point, Table, TableSettings, TableStrategy, TextLine,
    TextOptions, extract_text, extract_text_lines,
};

use unicode_script::{Script, UnicodeScript};

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
        for (index, mut page) in pdf.pages.into_iter().enumerate() {
            let page_draws = draw_styles.get(index).map_or(&[][..], Vec::as_slice);
            // Убираем наклонные водяные знаки до выделения строк и ячеек.
            page.chars.retain(|ch| !is_oblique(ch.matrix));
            pages_data.push(extract_page_data(&page, index + 1, page_draws)?);
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
    gutter: Option<f64>,
    tables: Vec<ExtractedTable>,
    lines: Vec<ExtractedLine>,
}

struct ExtractedTable {
    bbox: BBox,
    rows: Vec<Vec<String>>,
}

struct ExtractedLine {
    text: String,
    x0: f64,
    x1: f64,
    top: f64,
    bottom: f64,
    font_size: f64,
    is_bold: bool,
    is_mono: bool,
    has_drawn_bullet: bool,
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
    x: f64,
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
    let mut tables = extract_page_tables(page, &text_options)?;

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

    let mut raw_lines = extract_text_lines(&non_table_chars, &text_options, true, true);
    raw_lines.sort_by(|a, b| a.top.total_cmp(&b.top));
    let gutter = find_column_gutter(&raw_lines, page.bbox);
    tables.extend(extract_aligned_tables(
        &mut raw_lines,
        &text_options,
        page_draws,
        gutter,
    ));
    let raw_lines = if let Some(gutter) = gutter {
        raw_lines
            .into_iter()
            .flat_map(|line| {
                let chars = line.chars.as_deref().unwrap_or_default();
                let runs = horizontal_runs(chars, 18.0);
                if runs
                    .iter()
                    .any(|&(left, right)| left < gutter && right > gutter)
                    || line.x0 >= gutter
                    || line.x1 <= gutter
                {
                    return vec![line];
                }
                let (left, right): (Vec<_>, Vec<_>) = line
                    .chars
                    .unwrap_or_default()
                    .into_iter()
                    .partition(|ch| ch.x0 < gutter);
                let mut split = extract_text_lines(&left, &text_options, true, true);
                split.extend(extract_text_lines(&right, &text_options, true, true));
                split
            })
            .collect()
    } else {
        raw_lines
    };
    let mut lines = Vec::with_capacity(raw_lines.len());
    for line in raw_lines {
        let text = normalize_line_text(&line.text);
        if text.is_empty() {
            continue;
        }
        let chars = line.chars.as_deref().unwrap_or(&[]);
        let (font_size, is_bold, is_mono) = analyze_line_style(chars, line_bbox(&line), page_draws);
        lines.push(ExtractedLine {
            text,
            x0: line.x0,
            x1: line.x1,
            top: line.top,
            bottom: line.bottom,
            font_size,
            is_bold,
            is_mono,
            has_drawn_bullet: page.curves.iter().any(|curve| {
                let gap = line.x0 - curve.x1;
                (1.5..=5.0).contains(&curve.width)
                    && (curve.width - curve.height).abs() <= 1.0
                    && gap >= font_size * 0.4
                    && gap <= font_size * 1.4
                    && (line.top + font_size * 0.25..=line.bottom - font_size * 0.15)
                        .contains(&((curve.top + curve.bottom) / 2.0))
            }),
        });
    }
    lines.sort_by(|a, b| a.top.total_cmp(&b.top).then_with(|| a.x0.total_cmp(&b.x0)));

    Ok(PageData {
        page_number,
        height: page.height.max(1.0),
        gutter,
        tables,
        lines,
    })
}

fn is_oblique([a, b, ..]: [f64; 6]) -> bool {
    a.abs().min(b.abs()) > a.abs().max(b.abs()) * 0.1
}

fn horizontal_runs(chars: &[Char], gap: f64) -> Vec<(f64, f64)> {
    let mut spans: Vec<_> = chars
        .iter()
        .filter(|ch| !ch.text.trim().is_empty())
        .map(|ch| (ch.x0, ch.x1))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for (left, right) in spans {
        if let Some(last) = runs.last_mut()
            && left - last.1 < gap
        {
            last.1 = last.1.max(right);
        } else {
            runs.push((left, right));
        }
    }
    runs
}

fn find_column_gutter(lines: &[TextLine], bbox: BBox) -> Option<f64> {
    let runs: Vec<_> = lines
        .iter()
        .map(|line| horizontal_runs(line.chars.as_deref().unwrap_or_default(), 18.0))
        .collect();
    let width = bbox.width();
    // Широкие строки с обеих сторон подтверждают просвет. Редкие строки
    // поперёк просвета могут быть общими заголовками.
    (30_i32..=70)
        .filter_map(|percent| {
            let cut = bbox.x0 + width * f64::from(percent) / 100.0;
            let mut left = ColumnEvidence::default();
            let mut right = ColumnEvidence::default();
            let mut crossing = 0;
            for (line, row) in lines.iter().zip(&runs) {
                if row.iter().any(|&(x0, x1)| x0 < cut + 9.0 && x1 > cut - 9.0) {
                    crossing += 1;
                    continue;
                }
                if row
                    .iter()
                    .any(|&(x0, x1)| x1 <= cut - 9.0 && x1 - x0 >= width * 0.18)
                {
                    left.record(line.top);
                }
                if row
                    .iter()
                    .any(|&(x0, x1)| x0 >= cut + 9.0 && x1 - x0 >= width * 0.18)
                {
                    right.record(line.top);
                }
            }
            let shared_rows = left.count.min(right.count);
            if shared_rows < 2
                || crossing > (lines.len() / 12).max(1)
                || left.end.min(right.end) <= left.start.max(right.start)
            {
                return None;
            }
            Some((
                (
                    shared_rows,
                    Reverse(crossing),
                    Reverse((percent - 50).abs()),
                ),
                cut,
            ))
        })
        .max_by_key(|&(score, _)| score)
        .map(|(_, cut)| cut)
}

#[derive(Default)]
struct ColumnEvidence {
    count: usize,
    start: f64,
    end: f64,
}

impl ColumnEvidence {
    fn record(&mut self, top: f64) {
        if self.count == 0 {
            self.start = top;
        }
        self.end = top;
        self.count += 1;
    }
}

fn extract_aligned_tables(
    lines: &mut Vec<TextLine>,
    options: &TextOptions,
    page_draws: &[TextDrawStyle],
    gutter: Option<f64>,
) -> Vec<ExtractedTable> {
    let mut tables = Vec::new();
    let mut non_table_lines = Vec::with_capacity(lines.len());
    let mut remaining = std::mem::take(lines).into_iter();
    while let Some(header) = remaining.next() {
        if gutter.is_some_and(|x| header.x0 < x && header.x1 > x) {
            non_table_lines.push(header);
            continue;
        }
        let chars = header.chars.as_deref().unwrap_or_default();
        let (size, bold, mono) = analyze_line_style(chars, line_bbox(&header), page_draws);
        if !bold || mono {
            non_table_lines.push(header);
            continue;
        }
        let columns = horizontal_runs(chars, 12.0);
        if columns.len() < 2 {
            non_table_lines.push(header);
            continue;
        }

        let mut rows = vec![(header, columns)];
        while let Some(next) = remaining.as_slice().first() {
            let (prev, _) = rows.last().unwrap();
            let dy = next.top - prev.top;
            let cells = horizontal_runs(next.chars.as_deref().unwrap_or_default(), 12.0);
            if dy < size * 0.9
                || dy > size * 2.3
                || cells.len() != rows[0].1.len()
                || cells
                    .iter()
                    .zip(&rows[0].1)
                    .any(|(cell, col)| (cell.0 - col.0).abs() > 2.0)
            {
                break;
            }
            rows.push((remaining.next().unwrap(), cells));
        }
        // Шапка и хотя бы две выровненные строки защищают от ложных таблиц
        // из коротких подписей и строк обычного текста.
        if rows.len() < 3 {
            non_table_lines.extend(rows.into_iter().map(|(line, _)| line));
            continue;
        }
        let bbox = rows
            .iter()
            .map(|(line, _)| line_bbox(line))
            .reduce(|a, b| {
                BBox::new(
                    a.x0.min(b.x0),
                    a.top.min(b.top),
                    a.x1.max(b.x1),
                    a.bottom.max(b.bottom),
                )
            })
            .unwrap();
        let rows = rows
            .into_iter()
            .map(|(line, cells)| {
                let chars = line.chars.unwrap_or_default();
                cells
                    .into_iter()
                    .map(|(left, right)| {
                        let cell: Vec<_> = chars
                            .iter()
                            .filter(|ch| ch.x0 >= left && ch.x1 <= right)
                            .cloned()
                            .collect();
                        normalize_cell_text(&extract_text(&cell, options))
                    })
                    .collect()
            })
            .collect();
        tables.push(ExtractedTable { bbox, rows });
    }
    *lines = non_table_lines;
    tables
}

fn line_bbox(line: &TextLine) -> BBox {
    BBox::new(line.x0, line.top, line.x1, line.bottom)
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
    bbox: BBox,
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
        bbox.height().max(1.0)
    } else {
        sizes.sort_by(f64::total_cmp);
        sizes[sizes.len() / 2]
    };

    if known_font_alpha > 0 {
        return (
            font_size,
            bold_count * 2 > known_font_alpha,
            mono_count == known_font_alpha,
        );
    }

    // pdfsink-rs оставляет Char.fontname = "unknown", поэтому восстанавливаем
    // стиль (bold/mono) по длине текста операторов контента PDF на той же полосе.
    let mut draw_total = 0usize;
    let mut draw_bold = 0usize;
    let mut draw_mono = 0usize;
    for draw in page_draws {
        if draw.y_from_top >= bbox.top - 3.0
            && draw.y_from_top <= bbox.bottom + 3.0
            && draw.x >= bbox.x0 - 3.0
            && draw.x <= bbox.x1 + 3.0
        {
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
    let is_mono = draw_total > 0 && draw_mono == draw_total;
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
                        let combined = mul_matrix(ctm, tm);
                        if weight > 0 && !is_oblique(combined) {
                            draws.push(TextDrawStyle {
                                x: combined[4],
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
        for line in page.lines.iter().filter(|line| !line.is_mono) {
            let bucket = (line.font_size * 2.0).round() as i64;
            let weight = line.text.chars().count().max(1);
            *histogram.entry(bucket).or_default() += weight;
        }
    }
    histogram
        .into_iter()
        .max_by_key(|&(bucket, weight)| (weight, bucket))
        .map_or(11.0, |(bucket, _)| (bucket as f64) / 2.0)
}

fn compute_heading_tiers(pages: &[PageData], body_font_size: f64) -> Vec<f64> {
    let threshold = body_font_size * 1.15;
    let mut buckets: Vec<i64> = pages
        .iter()
        .flat_map(|page| page.lines.iter())
        .filter(|line| !line.is_mono && line.font_size >= threshold)
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
    if line.is_mono || line.has_drawn_bullet || strip_bullet(&line.text).is_some() {
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
        || first.has_drawn_bullet
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
            && !next.has_drawn_bullet
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

enum PageItem {
    Line(ExtractedLine),
    Table(ExtractedTable),
}

impl PageItem {
    fn position(&self) -> (f64, f64, f64) {
        match self {
            Self::Line(line) => (line.x0, line.x1, line.top),
            Self::Table(table) => (table.bbox.x0, table.bbox.x1, table.bbox.top),
        }
    }
}

fn build_page_blocks(
    page: PageData,
    body_font_size: f64,
    heading_tiers: &[f64],
    blocks: &mut Vec<Block>,
) {
    let mut page_items: Vec<_> = page
        .lines
        .into_iter()
        .map(PageItem::Line)
        .chain(page.tables.into_iter().map(PageItem::Table))
        .collect();
    let mut barriers = Vec::new();
    if let Some(gutter) = page.gutter {
        barriers.extend(page_items.iter().filter_map(|item| {
            let (x0, x1, top) = item.position();
            (x0 < gutter && x1 > gutter).then_some(top)
        }));
        barriers.sort_by(f64::total_cmp);
    }
    // Между общими заголовками и таблицами сначала читаем левую колонку.
    let region = |item: &PageItem| {
        let (x0, x1, top) = item.position();
        page.gutter.map_or((0, 0), |gutter| {
            let band = barriers.partition_point(|&y| y < top);
            let column = if x1 <= gutter {
                0
            } else if x0 >= gutter {
                1
            } else {
                2
            };
            (band, column)
        })
    };
    page_items.sort_by(|a, b| {
        region(a)
            .cmp(&region(b))
            .then_with(|| a.position().2.total_cmp(&b.position().2))
    });
    let mut items = Vec::new();
    let mut lines = Vec::new();
    let mut prev_region = (0, 0);
    for item in page_items {
        let next_region = region(&item);
        if next_region != prev_region || matches!(item, PageItem::Table(_)) {
            group_segment_lines(&lines, body_font_size, heading_tiers, &mut items);
            lines.clear();
        }
        match item {
            PageItem::Line(line) => lines.push(line),
            PageItem::Table(table) => items.push(BlockContent::Table {
                rows: table.rows,
                row_lines: Vec::new(),
            }),
        }
        prev_region = next_region;
    }
    group_segment_lines(&lines, body_font_size, heading_tiers, &mut items);

    let mut page_line = 1usize;
    for mut content in items {
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
    out: &mut Vec<BlockContent>,
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
            out.push(BlockContent::Heading {
                level,
                text: join_pdf_lines(&parts),
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
            out.push(BlockContent::Heading {
                level,
                text: join_pdf_lines(&parts),
            });
            index = end + 1;
            continue;
        }

        // 2. Блок кода (моноширинный шрифт)
        if line.is_mono {
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
            out.push(BlockContent::Code {
                language: None,
                text: code_lines.join("\n"),
            });
            continue;
        }

        // 3. Элемент списка
        if let Some(first_text) = list_item_text(lines, index, heading_tiers) {
            let mut parts = vec![first_text];
            let mut prev = line;
            index += 1;
            while let Some(candidate) = lines.get(index) {
                if candidate.is_mono
                    || list_item_text(lines, index, heading_tiers).is_some()
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
            out.push(BlockContent::ListItem {
                text: join_pdf_lines(&parts),
            });
            continue;
        }

        // 4. Абзац
        let mut parts = vec![line.text.as_str()];
        let mut prev = line;
        index += 1;
        while let Some(candidate) = lines.get(index) {
            if candidate.is_mono
                || list_item_text(lines, index, heading_tiers).is_some()
                || starts_heading(index)
            {
                break;
            }
            let gap = candidate.top - prev.bottom;
            let line_height = (prev.bottom - prev.top).max(prev.font_size);
            let new_paragraph_indent =
                candidate.x0 - prev.x0 > body_font_size * 1.5 && prev.text.ends_with('.');
            let paragraph_break = gap > line_height * 0.7 && prev.text.ends_with(['.', '!', '?']);
            if gap <= line_height * 0.85 && !new_paragraph_indent && !paragraph_break {
                parts.push(candidate.text.as_str());
                prev = candidate;
                index += 1;
            } else {
                break;
            }
        }
        out.push(BlockContent::Paragraph {
            text: join_pdf_lines(&parts),
        });
    }
}

fn list_item_text<'a>(
    lines: &'a [ExtractedLine],
    index: usize,
    heading_tiers: &[f64],
) -> Option<&'a str> {
    let line = &lines[index];
    if line.is_mono || tier_heading_level_for_line(line, heading_tiers).is_some() {
        return None;
    }
    if line.has_drawn_bullet {
        return Some(&line.text);
    }
    if let Some(text) = strip_bullet(&line.text).or_else(|| strip_ordered_marker(&line.text)) {
        return Some(text);
    }
    // Тире в начале переноса остаётся частью абзаца. Считаем его маркером
    // рядом с явно размеченным, выровненным пунктом списка.
    let rest = line
        .text
        .strip_prefix("– ")
        .or_else(|| line.text.strip_prefix("— "))?;
    let next = lines.get(index + 1);
    let prev = index.checked_sub(1).and_then(|i| lines.get(i));
    [prev, next]
        .into_iter()
        .flatten()
        .any(|neighbor| {
            (neighbor.x0 - line.x0).abs() <= 2.0
                && (neighbor.top - line.top).abs() <= line.font_size * 1.7
                && (neighbor.has_drawn_bullet
                    || strip_bullet(&neighbor.text).is_some()
                    || strip_ordered_marker(&neighbor.text).is_some())
        })
        .then_some(rest)
}

fn strip_ordered_marker(text: &str) -> Option<&str> {
    let (marker, rest) = text.split_once(' ')?;
    let prefix = marker.strip_suffix(['.', ')'])?;
    if !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()) {
        return Some(rest);
    }
    // «т. п.» на новой строке может быть продолжением предыдущего предложения.
    if marker.ends_with('.')
        && rest
            .split_whitespace()
            .next()
            .and_then(|word| word.strip_suffix('.'))
            .is_some_and(is_single_letter)
    {
        return None;
    }
    is_single_letter(prefix).then_some(rest)
}

fn is_single_letter(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(char::is_alphabetic) && chars.next().is_none()
}

fn strip_bullet(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let first = trimmed.chars().next()?;
    // Тире (`–`, `—`, `-`) не считаем маркером списка, чтобы не разрывать абзац при переносе строки с тире.
    if matches!(first, '•' | '●' | '○' | '▪' | '▫' | '■' | '*' | '✓') {
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
    normalize_schwa(text)
        .replace('\u{00A0}', " ")
        .replace('\u{00AD}', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_cell_text(text: &str) -> String {
    normalize_line_text(&dehyphenate_multiline(text))
}

fn normalize_schwa(text: &str) -> Cow<'_, str> {
    if !text.contains(['ə', 'Ə']) {
        return text.into();
    }

    let has_kazakh_letters = text.contains([
        'ғ', 'қ', 'ң', 'ө', 'ұ', 'ү', 'һ', 'і', 'Ғ', 'Қ', 'Ң', 'Ө', 'Ұ', 'Ү', 'Һ', 'І',
    ]);
    // В латинских словах и фонетических обозначениях сохраняем оригинальную букву.
    text.split_inclusive(|ch: char| !ch.is_alphabetic())
        .map(|word| {
            let cyrillic = word.chars().any(|ch| ch.script() == Script::Cyrillic);
            let isolated = matches!(
                word.trim_end_matches(|ch: char| !ch.is_alphabetic()),
                "ə" | "Ə"
            );
            if cyrillic || (isolated && has_kazakh_letters) {
                word.replace('ə', "ә").replace('Ə', "Ә").into()
            } else {
                Cow::Borrowed(word)
            }
        })
        .collect::<String>()
        .into()
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
            ("по-\nразному", "по-разному"),
            ("по-\nновому", "по-новому"),
            ("по-\nсвоему", "по-своему"),
            ("по-\nказахски", "по-казахски"),
            ("по-\nвторному", "повторному"),
            ("по-\nстоянному", "постоянному"),
            ("по-\nследнему", "последнему"),
            ("по-\nдробному", "подробному"),
            ("по-\nиски", "поиски"),
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
            x1: 100.0,
            top: 100.0,
            bottom: 112.0,
            font_size: 12.0,
            is_bold: true,
            is_mono: false,
            has_drawn_bullet: false,
        };
        assert_eq!(tier_heading_level_for_line(&line, &tiers), Some(6));

        let bold_body = ExtractedLine {
            text: "Подзаголовок".into(),
            x0: 50.0,
            x1: 100.0,
            top: 100.0,
            bottom: 110.0,
            font_size: 10.0,
            is_bold: true,
            is_mono: false,
            has_drawn_bullet: false,
        };
        assert_eq!(
            bold_subheading_run_end(&[bold_body], 0, 10.0, &tiers),
            Some((0, 6))
        );
    }
}
