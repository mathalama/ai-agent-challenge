use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

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

        let mut pages_data = Vec::with_capacity(pdf.pages.len());
        for (index, page) in pdf.pages.iter().enumerate() {
            pages_data.push(extract_page_data(page, index + 1)?);
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

fn extract_page_data(page: &Page, page_number: usize) -> Result<PageData> {
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
        let (font_size, is_bold, is_mono) = analyze_line_style(chars, line.bottom - line.top);
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

fn analyze_line_style(chars: &[Char], fallback_height: f64) -> (f64, bool, bool) {
    let mut sizes = Vec::new();
    let mut alpha_total = 0usize;
    let mut bold_count = 0usize;
    let mut mono_count = 0usize;

    for ch in chars {
        if ch.text.trim().is_empty() {
            continue;
        }
        if ch.size.is_finite() && ch.size > 0.0 {
            sizes.push(ch.size);
        }
        if ch.text.chars().any(char::is_alphanumeric) {
            alpha_total += 1;
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
        fallback_height.max(1.0)
    } else {
        sizes.sort_by(f64::total_cmp);
        sizes[sizes.len() / 2]
    };

    let is_bold = alpha_total > 0 && bold_count * 2 > alpha_total;
    let is_mono = alpha_total > 0 && mono_count * 2 > alpha_total;
    (font_size, is_bold, is_mono)
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

fn heading_level_for_line(
    line: &ExtractedLine,
    body_font_size: f64,
    heading_tiers: &[f64],
    next_line: Option<&ExtractedLine>,
) -> Option<u8> {
    if line.is_mono || strip_bullet(&line.text).is_some() {
        return None;
    }
    let rounded = (line.font_size * 2.0).round() / 2.0;
    if let Some(index) = heading_tiers
        .iter()
        .position(|&tier| (tier - rounded).abs() < 0.01)
    {
        return Some((index as u8 + 1).min(6));
    }

    // Короткая отдельная полужирная строка перед обычным текстом может быть подзаголовком.
    let char_count = line.text.chars().count();
    let looks_like_sentence = line.text.ends_with('.') && char_count > 45;
    let followed_by_different_style = next_line
        .is_none_or(|next| !next.is_bold || (next.top - line.bottom) > line.font_size * 0.65);
    if line.is_bold
        && line.font_size >= body_font_size * 0.98
        && char_count <= 100
        && !looks_like_sentence
        && followed_by_different_style
    {
        let level = (heading_tiers.len() as u8 + 1).clamp(1, 6);
        return Some(level);
    }

    None
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
    let mut index = 0usize;
    while index < lines.len() {
        let line = &lines[index];
        let next = lines.get(index + 1);

        // 1. Заголовок
        if let Some(level) = heading_level_for_line(line, body_font_size, heading_tiers, next) {
            let start_top = line.top;
            let mut parts = vec![line.text.as_str()];
            let mut prev = line;
            index += 1;
            while let Some(candidate) = lines.get(index) {
                let gap = candidate.top - prev.bottom;
                let line_height = (prev.bottom - prev.top).max(prev.font_size);
                let same_level = heading_level_for_line(
                    candidate,
                    body_font_size,
                    heading_tiers,
                    lines.get(index + 1),
                ) == Some(level);
                if same_level && gap <= line_height * 0.85 {
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
                    || heading_level_for_line(
                        candidate,
                        body_font_size,
                        heading_tiers,
                        lines.get(index + 1),
                    )
                    .is_some()
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
            if candidate.is_mono
                || strip_bullet(&candidate.text).is_some()
                || heading_level_for_line(
                    candidate,
                    body_font_size,
                    heading_tiers,
                    lines.get(index + 1),
                )
                .is_some()
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
    if matches!(first, '•' | '●' | '○' | '▪' | '▫' | '■' | '*' | '-' | '–') {
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
