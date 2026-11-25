use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event},
};
use zip::ZipArchive;

use crate::{
    Block, BlockContent, BlockId, Document, DocumentError, DocumentParser, Result, SourceSpan,
    has_extension,
};

const MAX_XML_PART_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_DOCUMENT_PART: &str = "word/document.xml";
const DEFAULT_STYLES_PART: &str = "word/styles.xml";

pub struct DocxParser;

impl DocumentParser for DocxParser {
    fn supports(&self, path: &Path) -> bool {
        has_extension(path, &["docx"])
    }

    fn parse(&self, bytes: &[u8]) -> Result<Document> {
        let mut archive = ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| DocumentError::InvalidDocx(format!("повреждён ZIP-архив: {error}")))?;

        let document_part = resolve_document_part(&mut archive)?;
        let styles_part = resolve_styles_part(&mut archive, &document_part)?;

        let styles = match read_zip_entry(&mut archive, &styles_part) {
            Ok(xml) => parse_styles(&xml)?,
            Err(ZipEntryError::Missing) if styles_part != DEFAULT_STYLES_PART => {
                match read_zip_entry(&mut archive, DEFAULT_STYLES_PART) {
                    Ok(xml) => parse_styles(&xml)?,
                    Err(ZipEntryError::Missing) => HashMap::new(),
                    Err(ZipEntryError::Other(message)) => {
                        return Err(DocumentError::InvalidDocx(message));
                    }
                }
            }
            Err(ZipEntryError::Missing) => HashMap::new(),
            Err(ZipEntryError::Other(message)) => return Err(DocumentError::InvalidDocx(message)),
        };

        let document_xml = match read_zip_entry(&mut archive, &document_part) {
            Ok(xml) => xml,
            Err(ZipEntryError::Missing) => {
                return Err(DocumentError::InvalidDocx(format!(
                    "в архиве отсутствует {document_part}"
                )));
            }
            Err(ZipEntryError::Other(message)) => return Err(DocumentError::InvalidDocx(message)),
        };

        let blocks = parse_document_xml(&document_xml, &styles)?;
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

enum ZipEntryError {
    Missing,
    Other(String),
}

fn read_zip_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> std::result::Result<String, ZipEntryError> {
    read_zip_entry_with_limit(archive, name, MAX_XML_PART_BYTES)
}

fn read_zip_entry_with_limit(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
    max_bytes: u64,
) -> std::result::Result<String, ZipEntryError> {
    let file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Err(ZipEntryError::Missing),
        Err(error) => {
            return Err(ZipEntryError::Other(format!(
                "не удалось открыть {name}: {error}"
            )));
        }
    };
    if file.size() > max_bytes {
        return Err(ZipEntryError::Other(format!(
            "часть «{name}» превышает допустимый размер {} МБ",
            max_bytes / (1024 * 1024)
        )));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| ZipEntryError::Other(format!("не удалось прочитать {name}: {error}")))?;
    if bytes.len() as u64 > max_bytes {
        return Err(ZipEntryError::Other(format!(
            "часть «{name}» превышает допустимый размер {} МБ",
            max_bytes / (1024 * 1024)
        )));
    }
    String::from_utf8(bytes).map_err(|error| {
        ZipEntryError::Other(format!("часть «{name}» не в кодировке UTF-8: {error}"))
    })
}

fn resolve_document_part(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<String> {
    match read_zip_entry(archive, "_rels/.rels") {
        Ok(xml) => {
            if let Some(target) = find_relationship_target(&xml, "/officeDocument")? {
                return Ok(normalize_zip_target("", &target));
            }
            Ok(DEFAULT_DOCUMENT_PART.into())
        }
        Err(ZipEntryError::Missing) => Ok(DEFAULT_DOCUMENT_PART.into()),
        Err(ZipEntryError::Other(message)) => Err(DocumentError::InvalidDocx(message)),
    }
}

fn resolve_styles_part(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    document_part: &str,
) -> Result<String> {
    let (base_dir, rels_path) = part_rels_path(document_part);
    match read_zip_entry(archive, &rels_path) {
        Ok(xml) => {
            if let Some(target) = find_relationship_target(&xml, "/styles")? {
                return Ok(normalize_zip_target(base_dir, &target));
            }
            Ok(DEFAULT_STYLES_PART.into())
        }
        Err(ZipEntryError::Missing) => Ok(DEFAULT_STYLES_PART.into()),
        Err(ZipEntryError::Other(message)) => Err(DocumentError::InvalidDocx(message)),
    }
}

fn part_rels_path(part_path: &str) -> (&str, String) {
    match part_path.rsplit_once('/') {
        Some((dir, file)) => (dir, format!("{dir}/_rels/{file}.rels")),
        None => ("", format!("_rels/{part_path}.rels")),
    }
}

fn normalize_zip_target(base_dir: &str, target: &str) -> String {
    let normalized_target = target.replace('\\', "/");
    let combined = if let Some(stripped) = normalized_target.strip_prefix('/') {
        stripped.to_owned()
    } else if base_dir.is_empty() {
        normalized_target
    } else {
        format!("{base_dir}/{normalized_target}")
    };

    let mut segments: Vec<&str> = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

fn find_relationship_target(xml: &str, type_suffix: &str) -> Result<Option<String>> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref element)) | Ok(Event::Empty(ref element)) => {
                let qname = element.name();
                if local_name(qname.as_ref()) != "Relationship" {
                    continue;
                }
                let rel_type = attr_value(element, "Type")?;
                let target_mode = attr_value(element, "TargetMode")?;
                let is_external = target_mode
                    .as_deref()
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                if !is_external
                    && rel_type
                        .as_deref()
                        .is_some_and(|value| value.ends_with(type_suffix))
                    && let Some(target) = attr_value(element, "Target")?
                    && !target.trim().is_empty()
                {
                    return Ok(Some(target));
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(DocumentError::InvalidDocx(format!(
                    "ошибка XML в файле связей .rels: {error}"
                )));
            }
            _ => {}
        }
    }
    Ok(None)
}

#[derive(Debug, Default, Clone)]
struct StyleInfo {
    heading_level: Option<u8>,
    is_list: bool,
    based_on: Option<String>,
}

fn parse_styles(xml: &str) -> Result<HashMap<String, StyleInfo>> {
    let mut reader = Reader::from_str(xml);
    let mut styles: HashMap<String, StyleInfo> = HashMap::new();
    let mut current_id: Option<String> = None;
    let mut current = StyleInfo::default();
    let mut in_ppr = false;
    let mut ignored_depth = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if ignored_depth > 0 {
                    ignored_depth += 1;
                    continue;
                }
                if is_change_container(name) {
                    ignored_depth = 1;
                    continue;
                }
                match name {
                    "style" => {
                        current_id = attr_value(element, "styleId")?;
                        current = StyleInfo::default();
                        if let Some(id) = current_id.as_deref() {
                            current.heading_level = heading_level_from_name(id);
                            current.is_list = is_list_style_name(id);
                        }
                    }
                    "pPr" if current_id.is_some() => in_ppr = true,
                    "numPr" if in_ppr => current.is_list = true,
                    _ => {}
                }
            }
            Ok(Event::Empty(ref element)) if current_id.is_some() && ignored_depth == 0 => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                match name {
                    "name" => {
                        if let Some(val) = attr_value(element, "val")? {
                            if let Some(level) = heading_level_from_name(&val) {
                                current.heading_level = Some(level);
                            }
                            if is_list_style_name(&val) {
                                current.is_list = true;
                            }
                        }
                    }
                    "basedOn" => {
                        current.based_on = attr_value(element, "val")?;
                    }
                    "outlineLvl" if in_ppr => {
                        if let Some(level) = parse_outline_level(element)? {
                            current.heading_level = Some(level);
                        }
                    }
                    "numPr" if in_ppr => current.is_list = true,
                    _ => {}
                }
            }
            Ok(Event::End(ref element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if ignored_depth > 0 {
                    ignored_depth -= 1;
                    continue;
                }
                match name {
                    "pPr" => in_ppr = false,
                    "style" => {
                        if let Some(id) = current_id.take() {
                            styles.insert(id, std::mem::take(&mut current));
                        }
                        in_ppr = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(DocumentError::InvalidDocx(format!(
                    "ошибка XML в word/styles.xml: {error}"
                )));
            }
            _ => {}
        }
    }

    // Разрешаем наследование стилей (`basedOn`).
    let snapshot = styles.clone();
    for info in styles.values_mut() {
        let mut parent_id = info.based_on.clone();
        let mut depth = 0;
        while let Some(id) = parent_id {
            depth += 1;
            if depth > 16 {
                break;
            }
            let Some(parent) = snapshot.get(&id) else {
                break;
            };
            if info.heading_level.is_none() {
                info.heading_level = parent.heading_level;
            }
            info.is_list |= parent.is_list;
            parent_id = parent.based_on.clone();
        }
    }

    Ok(styles)
}

#[derive(Default)]
struct ParagraphBuilder {
    text: String,
    style_id: Option<String>,
    outline_level: Option<u8>,
    has_num_pr: bool,
}

struct TableBuilder {
    rows: Vec<Vec<String>>,
    current_row: Option<Vec<String>>,
    current_row_grid_before: usize,
    current_row_grid_after: usize,
    in_trpr: bool,
    current_cell_paragraphs: Option<Vec<String>>,
    current_cell_grid_span: usize,
    in_tcpr: bool,
}

fn parse_document_xml(xml: &str, styles: &HashMap<String, StyleInfo>) -> Result<Vec<Block>> {
    let mut reader = Reader::from_str(xml);
    let mut blocks = Vec::new();
    let mut next_line = 1usize;

    let mut in_body = false;
    let mut ignored_depth = 0usize;
    let mut table_depth = 0usize;
    let mut table: Option<TableBuilder> = None;
    let mut paragraph: Option<ParagraphBuilder> = None;
    let mut in_ppr = false;
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if !in_body {
                    if name == "body" {
                        in_body = true;
                    }
                    continue;
                }
                if ignored_depth > 0 {
                    ignored_depth += 1;
                    continue;
                }
                if is_ignored_container(name) {
                    ignored_depth = 1;
                    continue;
                }

                match name {
                    "tbl" => {
                        table_depth += 1;
                        if table_depth == 1 {
                            table = Some(TableBuilder {
                                rows: Vec::new(),
                                current_row: None,
                                current_row_grid_before: 0,
                                current_row_grid_after: 0,
                                in_trpr: false,
                                current_cell_paragraphs: None,
                                current_cell_grid_span: 1,
                                in_tcpr: false,
                            });
                        }
                    }
                    "tr" if table_depth == 1 => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.current_row = Some(Vec::new());
                            tbl.current_row_grid_before = 0;
                            tbl.current_row_grid_after = 0;
                            tbl.in_trpr = false;
                        }
                    }
                    "trPr" if table_depth == 1 && paragraph.is_none() => {
                        if let Some(tbl) = table.as_mut()
                            && tbl.current_cell_paragraphs.is_none()
                        {
                            tbl.in_trpr = true;
                        }
                    }
                    "tc" if table_depth == 1 => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.current_cell_paragraphs = Some(Vec::new());
                            tbl.current_cell_grid_span = 1;
                            tbl.in_tcpr = false;
                        }
                    }
                    "tcPr" if table_depth == 1 && paragraph.is_none() => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.in_tcpr = true;
                        }
                    }
                    "p" if paragraph.is_none() => {
                        paragraph = Some(ParagraphBuilder::default());
                    }
                    "pPr" if paragraph.is_some() => {
                        in_ppr = true;
                    }
                    "numPr" if in_ppr => {
                        if let Some(p) = paragraph.as_mut() {
                            p.has_num_pr = true;
                        }
                    }
                    "t" if paragraph.is_some() => {
                        in_text = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref element)) => {
                if !in_body || ignored_depth > 0 {
                    continue;
                }
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if table_depth == 1
                    && paragraph.is_none()
                    && let Some(tbl) = table.as_mut()
                {
                    if tbl.in_trpr {
                        match name {
                            "gridBefore" => {
                                if let Some(count) = parse_grid_offset(element)? {
                                    tbl.current_row_grid_before = count;
                                }
                                continue;
                            }
                            "gridAfter" => {
                                if let Some(count) = parse_grid_offset(element)? {
                                    tbl.current_row_grid_after = count;
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                    if tbl.in_tcpr && name == "gridSpan" {
                        if let Some(span) = parse_grid_span(element)? {
                            tbl.current_cell_grid_span = span;
                        }
                        continue;
                    }
                }
                if in_ppr && let Some(p) = paragraph.as_mut() {
                    match name {
                        "pStyle" => {
                            p.style_id = attr_value(element, "val")?;
                        }
                        "outlineLvl" => {
                            p.outline_level = parse_outline_level(element)?;
                        }
                        "numPr" => {
                            p.has_num_pr = true;
                        }
                        _ => {}
                    }
                } else if let Some(p) = paragraph.as_mut() {
                    match name {
                        "br" | "cr" => p.text.push('\n'),
                        "tab" => p.text.push(' '),
                        "noBreakHyphen" => p.text.push('-'),
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(ref text)) => {
                if in_body
                    && ignored_depth == 0
                    && in_text
                    && let Some(p) = paragraph.as_mut()
                {
                    p.text.push_str(text.as_ref());
                }
            }
            Ok(Event::GeneralRef(ref entity)) => {
                if in_body
                    && ignored_depth == 0
                    && in_text
                    && let Some(p) = paragraph.as_mut()
                {
                    if let Some(ch) = entity.resolve_char_ref().map_err(|error| {
                        DocumentError::InvalidDocx(format!(
                            "некорректная символьная ссылка в word/document.xml: {error}"
                        ))
                    })? {
                        p.text.push(ch);
                    } else if let Some(resolved) =
                        quick_xml::escape::resolve_xml_entity(entity.as_ref())
                    {
                        p.text.push_str(resolved);
                    }
                }
            }
            Ok(Event::CData(ref text)) => {
                if in_body
                    && ignored_depth == 0
                    && in_text
                    && let Some(p) = paragraph.as_mut()
                {
                    p.text.push_str(text.as_ref());
                }
            }
            Ok(Event::End(ref element)) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if !in_body {
                    continue;
                }
                if ignored_depth > 0 {
                    ignored_depth -= 1;
                    continue;
                }
                match name {
                    "body" => in_body = false,
                    "t" => in_text = false,
                    "pPr" => in_ppr = false,
                    "trPr" if table_depth == 1 => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.in_trpr = false;
                        }
                    }
                    "tcPr" if table_depth == 1 => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.in_tcpr = false;
                        }
                    }
                    "p" => {
                        if let Some(p) = paragraph.take() {
                            if table_depth >= 1 {
                                let cleaned = normalize_cell_text(&p.text);
                                if !cleaned.is_empty()
                                    && let Some(tbl) = table.as_mut()
                                    && let Some(cell_paragraphs) =
                                        tbl.current_cell_paragraphs.as_mut()
                                {
                                    cell_paragraphs.push(cleaned);
                                }
                            } else {
                                push_paragraph_block(p, styles, &mut blocks, &mut next_line);
                            }
                        }
                        in_ppr = false;
                        in_text = false;
                    }
                    "tc" if table_depth == 1 => {
                        if let Some(tbl) = table.as_mut()
                            && let Some(cell_paragraphs) = tbl.current_cell_paragraphs.take()
                            && let Some(row) = tbl.current_row.as_mut()
                        {
                            row.push(cell_paragraphs.join(" "));
                            // Учитываем объединённые по горизонтали ячейки (w:gridSpan),
                            // чтобы следующие столбцы в строке не съезжали влево.
                            for _ in 1..tbl.current_cell_grid_span {
                                row.push(String::new());
                            }
                            tbl.current_cell_grid_span = 1;
                            tbl.in_tcpr = false;
                        }
                    }
                    "tr" if table_depth == 1 => {
                        if let Some(tbl) = table.as_mut()
                            && let Some(cells) = tbl.current_row.take()
                        {
                            let mut row = Vec::with_capacity(
                                tbl.current_row_grid_before
                                    + cells.len()
                                    + tbl.current_row_grid_after,
                            );
                            row.resize(tbl.current_row_grid_before, String::new());
                            row.extend(cells);
                            row.resize(row.len() + tbl.current_row_grid_after, String::new());
                            tbl.current_row_grid_before = 0;
                            tbl.current_row_grid_after = 0;
                            tbl.in_trpr = false;
                            if !row.is_empty() {
                                tbl.rows.push(row);
                            }
                        }
                    }
                    "tbl" => {
                        if table_depth == 1
                            && let Some(mut tbl) = table.take()
                            && !tbl.rows.is_empty()
                        {
                            let max_cols = tbl.rows.iter().map(Vec::len).max().unwrap_or(0);
                            for row in &mut tbl.rows {
                                row.resize(max_cols, String::new());
                            }
                            let start_line = next_line;
                            let mut row_lines = Vec::with_capacity(tbl.rows.len());
                            for _ in &tbl.rows {
                                row_lines.push(next_line);
                                next_line += 1;
                            }
                            let end_line = next_line - 1;
                            blocks.push(Block {
                                id: BlockId(blocks.len() + 1),
                                location: SourceSpan {
                                    start_line,
                                    end_line,
                                    page: None,
                                },
                                content: BlockContent::Table {
                                    rows: tbl.rows,
                                    row_lines,
                                },
                            });
                        }
                        table_depth = table_depth.saturating_sub(1);
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(DocumentError::InvalidDocx(format!(
                    "ошибка XML в word/document.xml: {error}"
                )));
            }
            _ => {}
        }
    }

    Ok(blocks)
}

fn push_paragraph_block(
    paragraph: ParagraphBuilder,
    styles: &HashMap<String, StyleInfo>,
    blocks: &mut Vec<Block>,
    next_line: &mut usize,
) {
    let text = normalize_paragraph_text(&paragraph.text);
    if text.is_empty() {
        return;
    }

    let style_info = paragraph.style_id.as_deref().and_then(|id| styles.get(id));

    let heading_level = paragraph
        .outline_level
        .or_else(|| style_info.and_then(|info| info.heading_level))
        .or_else(|| {
            paragraph
                .style_id
                .as_deref()
                .and_then(heading_level_from_name)
        });

    let is_list = heading_level.is_none()
        && (paragraph.has_num_pr
            || style_info.is_some_and(|info| info.is_list)
            || paragraph
                .style_id
                .as_deref()
                .is_some_and(is_list_style_name));

    let (content, line_count) = if let Some(level) = heading_level {
        let single_line = collapse_spaces(&text);
        (
            BlockContent::Heading {
                level,
                text: single_line,
            },
            1,
        )
    } else if is_list {
        let cleaned = strip_leading_bullet(&text);
        let count = cleaned.lines().count().max(1);
        (BlockContent::ListItem { text: cleaned }, count)
    } else if let Some(stripped) = strip_explicit_bullet(&text) {
        let count = stripped.lines().count().max(1);
        (BlockContent::ListItem { text: stripped }, count)
    } else {
        let count = text.lines().count().max(1);
        (BlockContent::Paragraph { text }, count)
    };

    let start_line = *next_line;
    let end_line = start_line + line_count - 1;
    *next_line = end_line + 1;

    blocks.push(Block {
        id: BlockId(blocks.len() + 1),
        location: SourceSpan {
            start_line,
            end_line,
            page: None,
        },
        content,
    });
}

fn is_change_container(name: &str) -> bool {
    matches!(
        name,
        "pPrChange"
            | "rPrChange"
            | "tblPrChange"
            | "trPrChange"
            | "tcPrChange"
            | "sectPrChange"
            | "tblGridChange"
            | "numberingChange"
    )
}

fn is_ignored_container(name: &str) -> bool {
    is_change_container(name)
        || matches!(
            name,
            "del" | "moveFrom" | "instrText" | "fallback" | "Fallback" | "txbxContent"
        )
}

fn local_name(raw: &str) -> &str {
    raw.rsplit(':').next().unwrap_or(raw)
}

fn attr_value(element: &BytesStart<'_>, expected_local: &str) -> Result<Option<String>> {
    for attr in element.attributes() {
        let attr = attr.map_err(|error| {
            DocumentError::InvalidDocx(format!("некорректный атрибут XML: {error}"))
        })?;
        if local_name(attr.key.as_ref()) == expected_local {
            let value = attr
                .normalized_value(XmlVersion::default())
                .map_err(|error| {
                    DocumentError::InvalidDocx(format!(
                        "не удалось декодировать атрибут XML: {error}"
                    ))
                })?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn parse_outline_level(element: &BytesStart<'_>) -> Result<Option<u8>> {
    let Some(val) = attr_value(element, "val")? else {
        return Ok(None);
    };
    let Ok(parsed) = val.trim().parse::<u8>() else {
        return Ok(None);
    };
    // В OOXML outlineLvl 0..=8 соответствует уровням 1..=9, 9 означает обычный текст.
    if parsed >= 9 {
        return Ok(None);
    }
    Ok(Some((parsed + 1).min(6)))
}

fn parse_grid_span(element: &BytesStart<'_>) -> Result<Option<usize>> {
    let Some(val) = attr_value(element, "val")? else {
        return Ok(None);
    };
    let Ok(parsed) = val.trim().parse::<usize>() else {
        return Ok(None);
    };
    Ok((parsed >= 1).then(|| parsed.min(64)))
}

fn parse_grid_offset(element: &BytesStart<'_>) -> Result<Option<usize>> {
    let Some(val) = attr_value(element, "val")? else {
        return Ok(None);
    };
    let Ok(parsed) = val.trim().parse::<usize>() else {
        return Ok(None);
    };
    Ok(Some(parsed.min(64)))
}

fn heading_level_from_name(name: &str) -> Option<u8> {
    let lower = name.trim().to_lowercase();
    if matches!(lower.as_str(), "title" | "название") {
        return Some(1);
    }
    if matches!(lower.as_str(), "subtitle" | "подзаголовок") {
        return Some(2);
    }
    for prefix in ["heading", "заголовок"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let digits = rest.trim_start_matches([' ', '_', '-']);
            if let Ok(level) = digits.parse::<u8>()
                && (1..=6).contains(&level)
            {
                return Some(level);
            }
        }
    }
    None
}

fn is_list_style_name(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    lower.contains("list") || lower.contains("список")
}

fn normalize_paragraph_text(text: &str) -> String {
    let lines: Vec<String> = text
        .replace('\u{00A0}', " ")
        .replace('\u{00AD}', "")
        .lines()
        .map(collapse_spaces)
        .filter(|line| !line.is_empty())
        .collect();
    lines.join("\n")
}

fn normalize_cell_text(text: &str) -> String {
    collapse_spaces(&text.replace('\u{00A0}', " ").replace('\u{00AD}', ""))
}

fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_leading_bullet(text: &str) -> String {
    strip_explicit_bullet(text).unwrap_or_else(|| text.to_owned())
}

fn strip_explicit_bullet(text: &str) -> Option<String> {
    let first = text.chars().next()?;
    // Тире (`–`, `—`, `-`) не считаем маркером списка без w:numPr, чтобы не отрывать текст с тире.
    if !matches!(first, '•' | '●' | '○' | '▪' | '▫' | '■' | '*') {
        return None;
    }
    let after = &text[first.len_utf8()..];
    if after.chars().next().is_some_and(char::is_whitespace) {
        let trimmed = after.trim_start();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}
