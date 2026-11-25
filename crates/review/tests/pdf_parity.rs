use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use doc::{BlockContent, Document, ParserRegistry};
use llm::MockProvider;
use review::{CheckRegistry, Pipeline, RunContext, extract_table_requirements};

fn render_document_to_pdf(document: &Document) -> Vec<u8> {
    let mut builder = PdfGen::new();
    let mut cursor_y = 60.0f64;

    for block in &document.blocks {
        match &block.content {
            BlockContent::Heading { level, text } => {
                let size = match level {
                    1 => 18.0,
                    2 => 15.0,
                    3 => 13.0,
                    _ => 11.5,
                };
                if cursor_y + size + 20.0 > 780.0 {
                    builder.new_page();
                    cursor_y = 60.0;
                }
                cursor_y += 10.0;
                builder.text(50.0, cursor_y, size, true, text);
                cursor_y += size + 10.0;
            }
            BlockContent::Paragraph { text } => {
                for line in text.lines() {
                    if cursor_y + 18.0 > 780.0 {
                        builder.new_page();
                        cursor_y = 60.0;
                    }
                    builder.text(50.0, cursor_y, 9.5, false, line);
                    cursor_y += 13.0;
                }
                cursor_y += 10.0;
            }
            BlockContent::ListItem { text } => {
                let bullet_line = format!("• {text}");
                if cursor_y + 20.0 > 780.0 {
                    builder.new_page();
                    cursor_y = 60.0;
                }
                builder.text(55.0, cursor_y, 9.5, false, &bullet_line);
                cursor_y += 22.0;
            }
            BlockContent::Code { text, .. } => {
                for line in text.lines() {
                    if cursor_y + 16.0 > 780.0 {
                        builder.new_page();
                        cursor_y = 60.0;
                    }
                    builder.text(55.0, cursor_y, 9.0, false, line);
                    cursor_y += 12.0;
                }
                cursor_y += 10.0;
            }
            BlockContent::Table { rows, .. } => {
                let row_h = 20.0;
                let table_h = row_h * rows.len() as f64;
                if cursor_y + table_h + 20.0 > 780.0 {
                    builder.new_page();
                    cursor_y = 60.0;
                }
                let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                let col_w = 500.0 / cols as f64;
                builder.table(48.0, cursor_y, cols, col_w, row_h, rows);
                cursor_y += table_h + 16.0;
            }
        }
    }

    builder.build()
}

struct PdfGen {
    pages: Vec<String>,
    char_to_gid: BTreeMap<char, u16>,
    next_gid: u16,
}

impl PdfGen {
    fn new() -> Self {
        Self {
            pages: vec![String::new()],
            char_to_gid: BTreeMap::new(),
            next_gid: 1,
        }
    }

    fn new_page(&mut self) {
        self.pages.push(String::new());
    }

    fn encode_hex(&mut self, text: &str) -> String {
        let mut hex = String::with_capacity(text.len() * 4);
        for ch in text.chars() {
            let gid = *self.char_to_gid.entry(ch).or_insert_with(|| {
                let id = self.next_gid;
                self.next_gid += 1;
                id
            });
            let _ = write!(hex, "{gid:04X}");
        }
        hex
    }

    fn text(&mut self, x: f64, y_from_top: f64, size: f64, bold: bool, content: &str) {
        let pdf_y = 842.0 - y_from_top;
        let font = if bold { "F2" } else { "F1" };
        let hex = self.encode_hex(content);
        let stream = self.pages.last_mut().unwrap();
        let _ = writeln!(
            stream,
            "BT /{font} {size:.1} Tf 1 0 0 1 {x:.1} {pdf_y:.1} Tm <{hex}> Tj ET"
        );
    }

    fn table(
        &mut self,
        x: f64,
        top: f64,
        cols: usize,
        col_w: f64,
        row_h: f64,
        rows: &[Vec<String>],
    ) {
        let total_w = col_w * cols as f64;
        let total_h = row_h * rows.len() as f64;
        let stream = self.pages.last_mut().unwrap();
        let _ = writeln!(stream, "0.5 w");
        for r in 0..=rows.len() {
            let y = 842.0 - (top + r as f64 * row_h);
            let _ = writeln!(stream, "{x:.1} {y:.1} m {:.1} {y:.1} l S", x + total_w);
        }
        for c in 0..=cols {
            let cx = x + c as f64 * col_w;
            let y_top = 842.0 - top;
            let y_bot = 842.0 - (top + total_h);
            let _ = writeln!(stream, "{cx:.1} {y_top:.1} m {cx:.1} {y_bot:.1} l S");
        }

        for (r, row) in rows.iter().enumerate() {
            let text_y = top + r as f64 * row_h + row_h * 0.65;
            for (c, cell) in row.iter().enumerate() {
                if !cell.trim().is_empty() {
                    let char_count = cell.chars().count().max(1) as f64;
                    let font_size = ((col_w - 6.0) / (char_count * 0.52)).clamp(2.5, 8.0);
                    self.text(x + c as f64 * col_w + 3.0, text_y, font_size, r == 0, cell);
                }
            }
        }
    }

    fn build(self) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let page_count = self.pages.len();
        let first_page_obj = 8usize;
        let kids: String = (0..page_count)
            .map(|idx| format!("{} 0 R", first_page_obj + idx * 2))
            .collect::<Vec<_>>()
            .join(" ");

        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        objects.push(format!(
            "<< /Type /Pages /Kids [ {kids} ] /Count {page_count} >>"
        ));
        objects.push(
            "<< /Type /Font /Subtype /Type0 /BaseFont /TestSans /Encoding /Identity-H /DescendantFonts [4 0 R] /ToUnicode 5 0 R >>"
                .into(),
        );
        objects.push(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /TestSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor << /Type /FontDescriptor /FontName /TestSans /Flags 32 /FontBBox [0 -200 1000 800] /Ascent 800 /Descent -200 /CapHeight 700 /ItalicAngle 0 /StemV 80 >> /DW 500 >>"
                .into(),
        );

        let cmap = build_cmap(&self.char_to_gid);
        objects.push(format!(
            "<< /Length {} >>\nstream\n{cmap}\nendstream",
            cmap.len()
        ));

        objects.push(
            "<< /Type /Font /Subtype /Type0 /BaseFont /TestSans-Bold /Encoding /Identity-H /DescendantFonts [7 0 R] /ToUnicode 5 0 R >>"
                .into(),
        );
        objects.push(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /TestSans-Bold /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor << /Type /FontDescriptor /FontName /TestSans-Bold /Flags 32 /FontBBox [0 -200 1000 800] /Ascent 800 /Descent -200 /CapHeight 700 /ItalicAngle 0 /StemV 120 >> /DW 500 >>"
                .into(),
        );

        for (idx, content) in self.pages.into_iter().enumerate() {
            let content_obj = first_page_obj + idx * 2 + 1;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R /F2 6 0 R >> >> /Contents {content_obj} 0 R >>"
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (idx, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", idx + 1).as_bytes());
        }
        let xref_offset = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }
}

fn build_cmap(char_to_gid: &BTreeMap<char, u16>) -> String {
    let mut entries: Vec<(u16, char)> = char_to_gid.iter().map(|(&ch, &gid)| (gid, ch)).collect();
    entries.sort_by_key(|&(gid, _)| gid);
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n\
         <0000> <FFFF>\n\
         endcodespacerange\n",
    );
    for chunk in entries.chunks(100) {
        let _ = writeln!(out, "{} beginbfchar", chunk.len());
        for &(gid, ch) in chunk {
            let mut utf16 = [0u16; 2];
            let encoded = ch.encode_utf16(&mut utf16);
            let mut dst = String::new();
            for unit in encoded {
                let _ = write!(dst, "{unit:04X}");
            }
            let _ = writeln!(out, "<{gid:04X}> <{dst}>");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str(
        "endcmap\n\
         CMapName currentdict /CMap defineresource pop\n\
         end\n\
         end",
    );
    out
}

#[tokio::test]
async fn requirements_pdf_matches_markdown_requirements_and_verify_rate() {
    let md_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/requirements.md");
    let md_bytes = fs::read(&md_path).unwrap();
    let registry = ParserRegistry::default();
    let md_doc = registry.parse(&md_path, &md_bytes).unwrap();

    let pdf_bytes = render_document_to_pdf(&md_doc);
    let pdf_doc = registry
        .parse(Path::new("requirements.pdf"), &pdf_bytes)
        .unwrap();

    let md_tables = md_doc
        .blocks
        .iter()
        .filter(|b| matches!(b.content, BlockContent::Table { .. }))
        .count();
    let pdf_tables = pdf_doc
        .blocks
        .iter()
        .filter(|b| matches!(b.content, BlockContent::Table { .. }))
        .count();
    assert_eq!(pdf_tables, md_tables);

    let md_table_reqs = extract_table_requirements(&md_doc);
    let pdf_table_reqs = extract_table_requirements(&pdf_doc);
    let normalize_list = |reqs: &[review::Requirement]| {
        reqs.iter()
            .map(|r| (r.part.clone(), r.section.clone(), r.text.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        normalize_list(&pdf_table_reqs),
        normalize_list(&md_table_reqs)
    );

    let checks = CheckRegistry::with_builtin_rules();
    let pipeline = Pipeline::default();

    let mut md_ctx = RunContext::new(md_doc, &MockProvider, &checks);
    pipeline.run(&mut md_ctx).await.unwrap();

    let mut pdf_ctx = RunContext::new(pdf_doc, &MockProvider, &checks);
    pipeline.run(&mut pdf_ctx).await.unwrap();

    assert_eq!(pdf_ctx.requirements.len(), md_ctx.requirements.len());
    assert!(!pdf_ctx.findings.is_empty());

    let verify_rate = |findings: &[review::Finding]| {
        let verified = findings.iter().filter(|f| f.verified).count();
        verified as f64 / findings.len().max(1) as f64
    };
    assert!(
        verify_rate(&pdf_ctx.findings) >= verify_rate(&md_ctx.findings),
        "Доля подтверждённых цитат в PDF ниже Markdown"
    );
    assert!(pdf_ctx.findings.iter().all(|f| f.verified));
}
