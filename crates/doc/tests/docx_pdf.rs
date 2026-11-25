use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{Cursor, Write as _},
    path::Path,
};

use doc::{BlockContent, DocumentError, ParserRegistry, SourceSpan};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn build_docx(document_xml: &str, styles_xml: Option<&str>) -> Vec<u8> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buffer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        if let Some(styles) = styles_xml {
            zip.start_file("word/styles.xml", options).unwrap();
            zip.write_all(styles.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

#[test]
fn docx_extracts_headings_paragraphs_lists_and_tables_in_order() {
    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="CustomTitle">
    <w:name w:val="Heading 1"/>
  </w:style>
  <w:style w:type="paragraph" w:styleId="SubSection">
    <w:name w:val="heading 2"/>
  </w:style>
</w:styles>"#;

    let document_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="CustomTitle"/></w:pPr>
      <w:r><w:t>Уведомление о расходах</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Краткое описание </w:t></w:r>
      <w:del><w:r><w:delText>удалённый текст</w:delText></w:r></w:del>
      <w:r><w:t>функционала.</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:pStyle w:val="SubSection"/></w:pPr>
      <w:r><w:t>Функциональные требования</w:t></w:r>
    </w:p>
    <w:p>
      <w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>
      <w:r><w:t>Ф-1. Порог задаётся от 1 000 до 1 000 000 ₸.</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Parameter</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Format</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc><w:p><w:r><w:t>amount</w:t></w:r></w:p></w:tc>
        <w:tc>
          <w:p><w:r><w:t>integer</w:t></w:r></w:p>
          <w:p><w:r><w:t>32 bit</w:t></w:r></w:p>
        </w:tc>
      </w:tr>
    </w:tbl>
    <w:p>
      <w:r><w:t>Первая строка</w:t><w:br/><w:t>Вторая строка</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

    let bytes = build_docx(document_xml, Some(styles_xml));
    let document = ParserRegistry::default()
        .parse(Path::new("spec.docx"), &bytes)
        .unwrap();

    assert_eq!(document.title, "Уведомление о расходах");
    assert_eq!(document.blocks.len(), 6);

    assert!(matches!(
        &document.blocks[0].content,
        BlockContent::Heading { level: 1, text } if text == "Уведомление о расходах"
    ));
    assert_eq!(
        document.blocks[0].location,
        SourceSpan {
            start_line: 1,
            end_line: 1,
            page: None
        }
    );

    assert!(matches!(
        &document.blocks[1].content,
        BlockContent::Paragraph { text } if text == "Краткое описание функционала."
    ));
    assert!(matches!(
        &document.blocks[2].content,
        BlockContent::Heading { level: 2, text } if text == "Функциональные требования"
    ));
    assert!(matches!(
        &document.blocks[3].content,
        BlockContent::ListItem { text } if text == "Ф-1. Порог задаётся от 1 000 до 1 000 000 ₸."
    ));
    assert!(matches!(
        &document.blocks[4].content,
        BlockContent::Table { rows, row_lines }
            if rows == &[["Parameter", "Format"], ["amount", "integer 32 bit"]]
                && row_lines == &[5, 6]
    ));
    assert_eq!(
        document.blocks[5].location,
        SourceSpan {
            start_line: 7,
            end_line: 8,
            page: None
        }
    );
    assert!(document.contains_quote(
        SourceSpan {
            start_line: 6,
            end_line: 6,
            page: None
        },
        "amount | integer 32 bit"
    ));
}

#[test]
fn docx_returns_typed_error_for_corrupted_archives_and_xml() {
    let registry = ParserRegistry::default();

    // Не ZIP
    assert!(matches!(
        registry.parse(Path::new("broken.docx"), b"not a zip archive"),
        Err(DocumentError::InvalidDocx(_))
    ));

    // Валидный ZIP без word/document.xml
    let mut empty_zip = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut empty_zip);
        zip.start_file("other.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hello").unwrap();
        zip.finish().unwrap();
    }
    assert!(matches!(
        registry.parse(Path::new("missing.docx"), &empty_zip.into_inner()),
        Err(DocumentError::InvalidDocx(_))
    ));

    // Сломанный XML внутри word/document.xml
    let bad_xml = build_docx("<w:document><w:body><w:p></w:document>", None);
    assert!(matches!(
        registry.parse(Path::new("bad_xml.docx"), &bad_xml),
        Err(DocumentError::InvalidDocx(_))
    ));
}

/// Минимальный генератор валидного PDF 1.4 для тестов с поддержкой Unicode (ToUnicode CMap),
/// нескольких страниц, разных размеров шрифтов, таблиц с сеткой и скана без текстового слоя.
struct TestPdfBuilder {
    pages: Vec<String>,
    char_to_gid: BTreeMap<char, u16>,
    next_gid: u16,
}

impl TestPdfBuilder {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            char_to_gid: BTreeMap::new(),
            next_gid: 1,
        }
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

    fn add_page(&mut self, build: impl FnOnce(&mut TestPageBuilder<'_>)) {
        let mut page = TestPageBuilder {
            builder: self,
            stream: String::new(),
        };
        build(&mut page);
        let stream = page.stream;
        self.pages.push(stream);
    }

    fn build(self) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        // 1: Catalog
        // 2: Pages
        // 3: Type0 Unicode Regular font (F1)
        // 4: CIDFont (Regular)
        // 5: ToUnicode CMap
        // 6: Type0 Unicode Bold font (F2)
        // 7: CIDFont (Bold)
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

        let cmap = build_tounicode_cmap(&self.char_to_gid);
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
            let _ = write!(pdf_writer(&mut pdf), "{} 0 obj\n{body}\nendobj\n", idx + 1);
        }
        let xref_offset = pdf.len();
        let _ = write!(
            pdf_writer(&mut pdf),
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        );
        for offset in offsets {
            let _ = writeln!(pdf_writer(&mut pdf), "{offset:010} 00000 n ");
        }
        let _ = write!(
            pdf_writer(&mut pdf),
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        );
        pdf
    }
}

struct ByteWriter<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

fn pdf_writer(buf: &mut Vec<u8>) -> ByteWriter<'_> {
    ByteWriter(buf)
}

fn build_tounicode_cmap(char_to_gid: &BTreeMap<char, u16>) -> String {
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

struct TestPageBuilder<'a> {
    builder: &'a mut TestPdfBuilder,
    stream: String,
}

impl TestPageBuilder<'_> {
    fn text(&mut self, x: f64, y_from_top: f64, size: f64, bold: bool, content: &str) {
        let pdf_y = 842.0 - y_from_top;
        let font = if bold { "F2" } else { "F1" };
        let hex = self.builder.encode_hex(content);
        let _ = writeln!(
            self.stream,
            "BT /{font} {size:.1} Tf 1 0 0 1 {x:.1} {pdf_y:.1} Tm <{hex}> Tj ET"
        );
    }

    fn table(&mut self, x: f64, top: f64, col_widths: &[f64], row_height: f64, rows: &[&[&str]]) {
        let total_w: f64 = col_widths.iter().sum();
        let total_h = row_height * rows.len() as f64;
        let _ = writeln!(self.stream, "0.5 w");

        // Горизонтальные линии
        for r in 0..=rows.len() {
            let y = 842.0 - (top + r as f64 * row_height);
            let _ = writeln!(self.stream, "{x:.1} {y:.1} m {:.1} {y:.1} l S", x + total_w);
        }
        // Вертикальные линии
        let mut cx = x;
        for &w in col_widths.iter().chain(std::iter::once(&0.0)) {
            let y_top = 842.0 - top;
            let y_bot = 842.0 - (top + total_h);
            let _ = writeln!(self.stream, "{cx:.1} {y_top:.1} m {cx:.1} {y_bot:.1} l S");
            cx += w;
        }
        // Текст ячеек
        for (r, row) in rows.iter().enumerate() {
            let mut cx = x;
            let text_y = top + r as f64 * row_height + row_height * 0.65;
            for (c, cell) in row.iter().enumerate() {
                if !cell.is_empty() {
                    self.text(cx + 4.0, text_y, 9.0, r == 0, cell);
                }
                cx += col_widths[c];
            }
        }
    }
}

#[test]
fn pdf_multipage_coordinates_headers_footers_tables_and_hyphenation() {
    let mut builder = TestPdfBuilder::new();
    builder.add_page(|page| {
        // Колонтитул и номер страницы (должны быть удалены)
        page.text(
            50.0,
            30.0,
            9.0,
            false,
            "Учебная спецификация — Конфиденциально",
        );
        page.text(50.0, 100.0, 18.0, true, "Уведомление о расходах по карте");
        page.text(50.0, 140.0, 14.0, true, "Функциональные требования");
        // Перенос слова и разрыв строки внутри абзаца
        page.text(50.0, 175.0, 10.0, false, "Сервис выполняет обра-");
        page.text(50.0, 189.0, 10.0, false, "ботка событий в реальном");
        page.text(50.0, 203.0, 10.0, false, "времени без задержек.");
        // Таблица с сеткой
        page.table(
            50.0,
            240.0,
            &[150.0, 120.0, 180.0],
            24.0,
            &[
                &["Parameter", "Format", "Logic"],
                &["amount", "integer", "1 000–1 000 000"],
                &["enabled", "integer", "0 или 1"],
            ],
        );
        page.text(280.0, 815.0, 9.0, false, "Стр. 1 из 2");
    });
    builder.add_page(|page| {
        page.text(
            50.0,
            30.0,
            9.0,
            false,
            "Учебная спецификация — Конфиденциально",
        );
        page.text(50.0, 100.0, 14.0, true, "Список правил");
        page.text(
            50.0,
            135.0,
            10.0,
            false,
            "• Ф-1. Держатель активной карты KZT",
        );
        page.text(60.0, 149.0, 10.0, false, "задаёт порог от 1 000 ₸.");
        page.text(280.0, 815.0, 9.0, false, "Стр. 2 из 2");
    });

    let pdf_bytes = builder.build();
    let registry = ParserRegistry::default();
    let doc_first = registry.parse(Path::new("spec.pdf"), &pdf_bytes).unwrap();
    let doc_second = registry.parse(Path::new("spec.pdf"), &pdf_bytes).unwrap();

    // Координаты стабильны между запусками
    let spans_first: Vec<_> = doc_first.blocks.iter().map(|b| b.location).collect();
    let spans_second: Vec<_> = doc_second.blocks.iter().map(|b| b.location).collect();
    assert_eq!(spans_first, spans_second);

    assert_eq!(doc_first.title, "Уведомление о расходах по карте");

    // Колонтитулы и номера страниц удалены
    let all_text = format!("{:?}", doc_first.blocks);
    assert!(!all_text.contains("Конфиденциально"));
    assert!(!all_text.contains("Стр. 1 из 2"));
    assert!(!all_text.contains("Стр. 2 из 2"));

    // Перенос слова и разрыв строки склеены и подтверждаются через contains_quote
    let para_block = doc_first
        .blocks
        .iter()
        .find(|b| matches!(&b.content, BlockContent::Paragraph { text } if text.contains("обработка")))
        .expect("Должен быть абзац со склеенным переносом слова");
    assert_eq!(para_block.location.page, Some(1));
    assert!(doc_first.contains_quote(
        para_block.location,
        "Сервис выполняет обработка событий в реальном времени без задержек."
    ));
    assert!(doc_first.contains_quote(
        para_block.location,
        "обра-\nботка событий\nв реальном времени"
    ));

    // Таблица попала как BlockContent::Table, а не склеенный текст
    let table_block = doc_first
        .blocks
        .iter()
        .find(|b| matches!(b.content, BlockContent::Table { .. }))
        .expect("Таблица должна извлекаться как BlockContent::Table");
    assert_eq!(table_block.location.page, Some(1));
    let BlockContent::Table { rows, row_lines } = &table_block.content else {
        unreachable!();
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], ["Parameter", "Format", "Logic"]);
    assert_eq!(rows[1], ["amount", "integer", "1 000–1 000 000"]);
    assert!(doc_first.contains_quote(
        SourceSpan {
            start_line: row_lines[1],
            end_line: row_lines[1],
            page: Some(1),
        },
        "amount | integer | 1 000–1 000 000"
    ));
    // Неправильная страница не подтверждает цитату
    assert!(!doc_first.contains_quote(
        SourceSpan {
            start_line: row_lines[1],
            end_line: row_lines[1],
            page: Some(2),
        },
        "amount | integer | 1 000–1 000 000"
    ));

    // Вторая страница содержит элемент списка с page = Some(2)
    let list_block = doc_first
        .blocks
        .iter()
        .find(|b| matches!(&b.content, BlockContent::ListItem { text } if text.starts_with("Ф-1.")))
        .expect("На странице 2 должен быть элемент списка Ф-1");
    assert_eq!(list_block.location.page, Some(2));
    assert!(doc_first.contains_quote(
        list_block.location,
        "Ф-1. Держатель активной карты KZT задаёт порог от 1 000 ₸."
    ));
}

#[test]
fn pdf_scan_without_text_layer_and_corrupted_pdf_return_typed_errors() {
    let registry = ParserRegistry::default();

    // PDF-скан: страница с графическим прямоугольником, но без текста
    let mut scan = TestPdfBuilder::new();
    scan.add_page(|page| {
        page.stream.push_str("0 0 595 842 re f\n");
    });
    assert!(matches!(
        registry.parse(Path::new("scan.pdf"), &scan.build()),
        Err(DocumentError::EmptyPdfText)
    ));

    // Повреждённый PDF
    assert!(matches!(
        registry.parse(Path::new("broken.pdf"), b"not a pdf"),
        Err(DocumentError::InvalidPdf(_))
    ));
}
