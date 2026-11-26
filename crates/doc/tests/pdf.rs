use std::{collections::BTreeMap, fmt::Write as _, path::Path};

use doc::{BlockContent, DocumentError, ParserRegistry, SourceSpan};

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

        for r in 0..=rows.len() {
            let y = 842.0 - (top + r as f64 * row_height);
            let _ = writeln!(self.stream, "{x:.1} {y:.1} m {:.1} {y:.1} l S", x + total_w);
        }
        let mut cx = x;
        for &w in col_widths.iter().chain(std::iter::once(&0.0)) {
            let y_top = 842.0 - top;
            let y_bot = 842.0 - (top + total_h);
            let _ = writeln!(self.stream, "{cx:.1} {y_top:.1} m {cx:.1} {y_bot:.1} l S");
            cx += w;
        }
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
        page.text(
            50.0,
            30.0,
            9.0,
            false,
            "Учебная спецификация — Конфиденциально",
        );
        page.text(50.0, 100.0, 18.0, true, "Уведомление о расходах по карте");
        page.text(50.0, 140.0, 14.0, true, "Функциональные требования");
        page.text(50.0, 175.0, 10.0, false, "Сервис выполняет обра-");
        page.text(50.0, 189.0, 10.0, false, "ботка событий в реальном");
        page.text(50.0, 203.0, 10.0, false, "времени без задержек.");
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

    let spans_first: Vec<_> = doc_first.blocks.iter().map(|b| b.location).collect();
    let spans_second: Vec<_> = doc_second.blocks.iter().map(|b| b.location).collect();
    assert_eq!(spans_first, spans_second);

    assert_eq!(doc_first.title, "Уведомление о расходах по карте");

    let all_text = format!("{:?}", doc_first.blocks);
    assert!(!all_text.contains("Конфиденциально"));
    assert!(!all_text.contains("Стр. 1 из 2"));
    assert!(!all_text.contains("Стр. 2 из 2"));

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
    assert!(!doc_first.contains_quote(
        SourceSpan {
            start_line: row_lines[1],
            end_line: row_lines[1],
            page: Some(2),
        },
        "amount | integer | 1 000–1 000 000"
    ));

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
fn pdf_does_not_turn_multiline_bold_ending_into_heading_or_split_en_dash_wrap() {
    let mut builder = TestPdfBuilder::new();
    builder.add_page(|page| {
        page.text(50.0, 80.0, 16.0, true, "Спецификация");
        // Обычный текст с переносом на строку, начинающуюся с тире «– »
        page.text(50.0, 120.0, 10.0, false, "Порог расходов по карте");
        page.text(50.0, 134.0, 10.0, false, "– это лимит в тенге за месяц.");
        // Многострочный полужирный абзац с короткой последней строкой без точки
        page.text(
            50.0,
            175.0,
            10.0,
            true,
            "Важное примечание для всех сервисов интеграции,",
        );
        page.text(
            50.0,
            189.0,
            10.0,
            true,
            "которые обрабатывают события карт в контуре банка:",
        );
        page.text(50.0, 203.0, 10.0, true, "повторная доставка идемпотентна");
        // Продолжение обычного текста после отступа
        page.text(50.0, 240.0, 10.0, false, "Далее описаны форматы сообщений.");
    });

    let document = ParserRegistry::default()
        .parse(Path::new("typography.pdf"), &builder.build())
        .unwrap();

    // Единственный заголовок — «Спецификация» (короткая последняя строка полужирного абзаца не стала заголовком)
    let headings: Vec<&str> = document
        .blocks
        .iter()
        .filter_map(|b| match &b.content {
            BlockContent::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(headings, ["Спецификация"]);

    // Перенос со строкой «– это лимит...» остался одним абзацем, без ложного ListItem
    assert!(
        !document
            .blocks
            .iter()
            .any(|b| matches!(b.content, BlockContent::ListItem { .. }))
    );
    assert!(document.blocks.iter().any(|b| matches!(
        &b.content,
        BlockContent::Paragraph { text } if text == "Порог расходов по карте – это лимит в тенге за месяц."
    )));

    // Многострочный полужирный абзац собран целиком в один Paragraph
    assert!(document.blocks.iter().any(|b| matches!(
        &b.content,
        BlockContent::Paragraph { text }
            if text == "Важное примечание для всех сервисов интеграции, которые обрабатывают события карт в контуре банка: повторная доставка идемпотентна"
    )));
}

#[test]
fn pdf_recognizes_two_line_bold_subheading_as_single_heading() {
    let mut builder = TestPdfBuilder::new();
    builder.add_page(|page| {
        page.text(50.0, 80.0, 16.0, true, "Спецификация");
        // Двухстрочный полужирный подзаголовок обычного кегля (≤ 100 символов, без точки)
        page.text(50.0, 120.0, 10.0, true, "Требования к информационной");
        page.text(50.0, 134.0, 10.0, true, "безопасности мобильного канала");
        // Обычный текст после подзаголовка
        page.text(
            50.0,
            154.0,
            10.0,
            false,
            "Все запросы передаются по TLS 1.3.",
        );
        page.text(
            50.0,
            168.0,
            10.0,
            false,
            "Токены не пишутся в открытые журналы.",
        );
    });

    let document = ParserRegistry::default()
        .parse(Path::new("subheading.pdf"), &builder.build())
        .unwrap();

    let headings: Vec<&str> = document
        .blocks
        .iter()
        .filter_map(|b| match &b.content {
            BlockContent::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        headings,
        [
            "Спецификация",
            "Требования к информационной безопасности мобильного канала",
        ]
    );
}

#[test]
fn pdf_keeps_bold_middle_line_inside_paragraph_and_weights_bold_by_text_length() {
    let mut builder = TestPdfBuilder::new();
    builder.add_page(|page| {
        page.text(50.0, 80.0, 16.0, true, "Спецификация");
        // Абзац из трёх строк обычного кегля: средняя целиком жирная и без точки
        page.text(
            50.0,
            120.0,
            10.0,
            false,
            "Система обрабатывает запросы клиентов",
        );
        page.text(
            50.0,
            134.0,
            10.0,
            true,
            "строго в пределах установленного лимита",
        );
        page.text(50.0, 148.0, 10.0, false, "и фиксирует событие в аудите.");
        // Строка с тремя короткими жирными словами (3 оператора Tj) и длинным обычным хвостом (1 оператор Tj)
        page.text(50.0, 190.0, 10.0, true, "Код");
        page.text(72.0, 190.0, 10.0, true, "API");
        page.text(94.0, 190.0, 10.0, true, "200");
        page.text(
            116.0,
            190.0,
            10.0,
            false,
            "— ответ при успешном сохранении настроек без ошибок",
        );
    });

    let document = ParserRegistry::default()
        .parse(Path::new("bold_weights.pdf"), &builder.build())
        .unwrap();

    // Ни средняя жирная строка абзаца, ни строка с тремя короткими жирными словами не стали заголовками
    let headings: Vec<&str> = document
        .blocks
        .iter()
        .filter_map(|b| match &b.content {
            BlockContent::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(headings, ["Спецификация"]);

    assert!(document.blocks.iter().any(|b| matches!(
        &b.content,
        BlockContent::Paragraph { text }
            if text == "Система обрабатывает запросы клиентов строго в пределах установленного лимита и фиксирует событие в аудите."
    )));
    assert!(document.blocks.iter().any(|b| matches!(
        &b.content,
        BlockContent::Paragraph { text }
            if text.contains("Код") && text.contains("успешном сохранении настроек без ошибок")
    )));
}

#[test]
fn pdf_scan_without_text_layer_and_corrupted_pdf_return_typed_errors() {
    let registry = ParserRegistry::default();

    let mut scan = TestPdfBuilder::new();
    scan.add_page(|page| {
        page.stream.push_str("0 0 595 842 re f\n");
    });
    assert!(matches!(
        registry.parse(Path::new("scan.pdf"), &scan.build()),
        Err(DocumentError::EmptyPdfText)
    ));

    assert!(matches!(
        registry.parse(Path::new("broken.pdf"), b"not a pdf"),
        Err(DocumentError::InvalidPdf(_))
    ));
}
#[test]
fn pdf_reads_two_columns_in_section_order() {
    let doc = parse_sample_pdf("05-two-columns.pdf");
    let headings: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        headings,
        [
            "Сервис «Копилка»: требования",
            "1. Назначение",
            "2. Подключение",
            "3. Расчёт округления",
            "4. Списание в копилку",
            "5. Отключение",
            "6. Ограничения",
            "7. Отчётность",
            "8. Открытые вопросы",
        ]
    );
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text } if text.starts_with("Сервис «Копилка» позволяет")
            && text.contains("каждой покупки по карте"))));
}

#[test]
fn pdf_keeps_inline_code_in_prose_and_preserves_code_blocks() {
    let doc = parse_sample_pdf("07-api-spec-code-blocks.pdf");
    let headings: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        headings,
        [
            "API сервиса переводов по номеру телефона",
            "1. Общие сведения",
            "2. Создание перевода",
            "Пример запроса",
            "Пример ответа",
            "3. Статусы перевода",
            "4. Ошибки",
        ]
    );
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text } if text.contains("Authorization: Bearer <token>"))));
    for status in ["PENDING", "SUCCESS", "FAILED"] {
        assert!(doc.blocks.iter().any(|block| matches!(&block.content,
            BlockContent::ListItem { text } if text.starts_with(status))));
    }
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Code { text, .. } if text.starts_with("POST /transfers HTTP/1.1")
            && text.contains("Idempotency-Key"))));
}

#[test]
fn pdf_discards_rotated_watermark_before_extracting_text_and_cells() {
    let doc = parse_sample_pdf("13-form-footnotes-watermark.pdf");
    let headings: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::Heading { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        headings,
        [
            "Изменение лимитов снятия наличных",
            "Лист согласования",
            "Замечания"
        ]
    );
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Table { rows, .. }
            if rows[0].get(2).is_some_and(|cell| cell == "Решение")
                && rows[2].get(2).is_some_and(|cell| cell == "☐ Согласовано ☑ С замечаниями"))));
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text } if text.contains("[Вставить ссылку на решение кредитного комитета]"))));
    assert!(
        doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text } if text.contains("Текущий лимит в сторонних банкоматах")))
    );
}

#[test]
fn pdf_extracts_tables_with_horizontal_rules_or_aligned_cells() {
    let doc = parse_sample_pdf("03-borderless-tables.pdf");
    let tables: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::Table { rows, .. } => Some(rows),
            _ => None,
        })
        .collect();
    assert_eq!(tables.len(), 2);
    assert_eq!(tables[0].len(), 6);
    assert_eq!(
        tables[0][0],
        ["Тип перевода", "Комиссия", "Минимум", "Лимит в сутки"]
    );
    assert_eq!(
        tables[0][3],
        ["На карту другого банка РК", "0,95%", "200 ₸", "500 000 ₸"]
    );
    assert_eq!(tables[1].len(), 5);
    assert_eq!(tables[1][0], ["Параметр", "Значение"]);
    assert_eq!(
        tables[1][4],
        ["Возврат при ошибке", "В течение нескольких рабочих дней"]
    );
}

#[test]
fn pdf_separates_requirements_and_numbered_items() {
    let doc = parse_sample_pdf("01-classic-numbered-headings.pdf");
    for id in ["FR-6.", "FR-7."] {
        assert!(doc.blocks.iter().any(|block| matches!(&block.content,
            BlockContent::Paragraph { text } if text.starts_with(id)
                && !(text.contains("FR-6.") && text.contains("FR-7.")))));
    }
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::ListItem { text } if text == "Не более трёх срабатываний автоплатежа на один номер в сутки.")));
}

#[test]
fn pdf_extracts_nested_and_mixed_list_items() {
    let doc = parse_sample_pdf("06-bold-headings-nested-lists.pdf");
    let items: Vec<_> = doc
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::ListItem { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        &items[..7],
        [
            "Клиент прошёл биометрическую идентификацию.",
            "Биометрия не старше 12 месяцев.",
            "Документ, удостоверяющий личность, действителен.",
            "У клиента нет просроченной задолженности.",
            "Количество активных виртуальных карт — не более 5.",
            "Закрытые и заблокированные карты не учитываются.",
            "Карты в статусе «Выпускается» учитываются.",
        ]
    );
    assert!(items.contains(&"Срок действия — 3 года."));
    assert!(items.contains(&"Пополнение возможно с любой карты банка."));
    assert!(items.contains(&"Закрыть карту можно только при нулевом балансе."));
}

#[test]
fn pdf_corrects_schwa_in_kazakh_text() {
    let doc = parse_sample_pdf("08-mixed-languages-symbols.pdf");
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text }
            if text.contains("Әрбір операция журналға жазылады: ә, ғ, қ, ң, ө, ұ, ү, һ, і."))));
}

#[test]
fn pdf_keeps_compound_hyphen_in_landscape_table() {
    let doc = parse_sample_pdf("04-landscape-wide-table.pdf");
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Table { rows, .. } if rows[0].iter().any(|cell| cell == "Тест-кейс"))));
}

#[test]
fn pdf_joins_soft_wraps_and_keeps_compound_hyphens_in_narrow_column() {
    let doc = parse_sample_pdf("09-narrow-hyphenation.pdf");
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text } if text.contains("Высокопроизводительная антифрод-система")
            && text.contains("транзакционной активности держателей"))));
}

#[test]
fn pdf_keeps_abbreviations_at_the_start_of_wrapped_lines() {
    let path = Path::new("qr-payments.pdf");
    let doc = ParserRegistry::default()
        .parse(path, include_bytes!("../../../samples/qr-payments.pdf"))
        .unwrap();
    assert!(doc.blocks.iter().any(|block| matches!(&block.content,
        BlockContent::Paragraph { text } if text.starts_with("ER-3.") && text.ends_with("и т. п."))));
}

fn parse_sample_pdf(name: &str) -> doc::Document {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/pdf")
        .join(name);
    ParserRegistry::default()
        .parse(&path, &std::fs::read(&path).unwrap())
        .unwrap()
}
