use std::{
    io::{Cursor, Write as _},
    path::Path,
};

use doc::{BlockContent, DocumentError, ParserRegistry, SourceSpan};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn build_docx_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buffer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for &(name, data) in entries {
            zip.start_file(name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    buffer.into_inner()
}

fn build_docx(document_xml: &str, styles_xml: Option<&str>) -> Vec<u8> {
    let mut entries: Vec<(&str, &[u8])> = vec![("word/document.xml", document_xml.as_bytes())];
    if let Some(styles) = styles_xml {
        entries.push(("word/styles.xml", styles.as_bytes()));
    }
    build_docx_entries(&entries)
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
fn docx_handles_track_changes_simple_fields_grid_span_and_leading_dashes() {
    let document_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr>
        <w:pStyle w:val="Heading2"/>
        <w:pPrChange w:id="1" w:author="Editor">
          <w:pPr>
            <w:pStyle w:val="Normal"/>
          </w:pPr>
        </w:pPrChange>
      </w:pPr>
      <w:r><w:t>Новый заголовок после правки</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>См. раздел </w:t></w:r>
      <w:fldSimple w:instr=" REF _Ref123 \h ">
        <w:r><w:t>2.1 «Лимиты»</w:t></w:r>
      </w:fldSimple>
      <w:r><w:t> для деталей.</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>– это обычное пояснение с тире в начале строки.</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Поле</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Тип</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Обязательность</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Описание</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:tc>
          <w:tcPr><w:gridSpan w:val="2"/></w:tcPr>
          <w:p><w:r><w:t>amount (integer)</w:t></w:r></w:p>
        </w:tc>
        <w:tc><w:p><w:r><w:t>Да</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Порог в тенге</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

    let bytes = build_docx(document_xml, None);
    let document = ParserRegistry::default()
        .parse(Path::new("edge_cases.docx"), &bytes)
        .unwrap();

    assert_eq!(document.blocks.len(), 4);

    assert!(matches!(
        &document.blocks[0].content,
        BlockContent::Heading { level: 2, text } if text == "Новый заголовок после правки"
    ));

    assert!(matches!(
        &document.blocks[1].content,
        BlockContent::Paragraph { text } if text == "См. раздел 2.1 «Лимиты» для деталей."
    ));
    assert!(document.contains_quote(
        document.blocks[1].location,
        "См. раздел 2.1 «Лимиты» для деталей."
    ));

    assert!(matches!(
        &document.blocks[2].content,
        BlockContent::Paragraph { text } if text == "– это обычное пояснение с тире в начале строки."
    ));

    assert!(matches!(
        &document.blocks[3].content,
        BlockContent::Table { rows, .. }
            if rows == &[
                ["Поле", "Тип", "Обязательность", "Описание"],
                ["amount (integer)", "", "Да", "Порог в тенге"],
            ]
    ));
}

#[test]
fn docx_resolves_custom_document_and_styles_paths_via_rels() {
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="/word/document2.xml"/>
</Relationships>"#;

    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles2.xml"/>
</Relationships>"#;

    let styles2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="AltHeading">
    <w:name w:val="Heading 1"/>
  </w:style>
</w:styles>"#;

    let document2 = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="AltHeading"/></w:pPr>
      <w:r><w:t>Заголовок из document2</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Основной текст документа.</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

    let bytes = build_docx_entries(&[
        ("_rels/.rels", root_rels.as_bytes()),
        ("word/_rels/document2.xml.rels", doc_rels.as_bytes()),
        ("word/styles2.xml", styles2.as_bytes()),
        ("word/document2.xml", document2.as_bytes()),
    ]);

    let document = ParserRegistry::default()
        .parse(Path::new("custom_rels.docx"), &bytes)
        .unwrap();

    assert_eq!(document.title, "Заголовок из document2");
    assert_eq!(document.blocks.len(), 2);
    assert!(matches!(
        &document.blocks[0].content,
        BlockContent::Heading { level: 1, text } if text == "Заголовок из document2"
    ));
    assert!(matches!(
        &document.blocks[1].content,
        BlockContent::Paragraph { text } if text == "Основной текст документа."
    ));
}

#[test]
fn docx_skips_move_from_handles_grid_before_after_and_ignores_style_ppr_change() {
    let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="BodyFixed">
    <w:name w:val="Body Fixed"/>
    <w:pPr>
      <w:pPrChange w:id="1" w:author="Reviewer">
        <w:pPr>
          <w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr>
        </w:pPr>
      </w:pPrChange>
    </w:pPr>
  </w:style>
</w:styles>"#;

    let document_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:pPr><w:pStyle w:val="BodyFixed"/></w:pPr>
      <w:r><w:t>Обычный абзац после правки стиля.</w:t></w:r>
    </w:p>
    <w:p>
      <w:moveFrom w:id="2" w:author="Editor">
        <w:r><w:t>Перенесённый фрагмент</w:t></w:r>
      </w:moveFrom>
      <w:r><w:t>Начало. </w:t></w:r>
      <w:moveTo w:id="3" w:author="Editor">
        <w:r><w:t>Перенесённый фрагмент</w:t></w:r>
      </w:moveTo>
    </w:p>
    <w:tbl>
      <w:tr>
        <w:tc><w:p><w:r><w:t>Колонка 1</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Колонка 2</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Колонка 3</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr><w:gridBefore w:val="1"/></w:trPr>
        <w:tc><w:p><w:r><w:t>Сдвинуто во 2-ю</w:t></w:r></w:p></w:tc>
      </w:tr>
      <w:tr>
        <w:trPr><w:gridAfter w:val="1"/></w:trPr>
        <w:tc><w:p><w:r><w:t>Значение 1</w:t></w:r></w:p></w:tc>
        <w:tc><w:p><w:r><w:t>Значение 2</w:t></w:r></w:p></w:tc>
      </w:tr>
    </w:tbl>
  </w:body>
</w:document>"#;

    let bytes = build_docx(document_xml, Some(styles_xml));
    let document = ParserRegistry::default()
        .parse(Path::new("revisions_and_grid.docx"), &bytes)
        .unwrap();

    assert_eq!(document.blocks.len(), 3);

    // 1. Стиль с <w:pPrChange><w:pPr><w:numPr/></w:pPr></w:pPrChange> не делает абзац списком
    assert!(matches!(
        &document.blocks[0].content,
        BlockContent::Paragraph { text } if text == "Обычный абзац после правки стиля."
    ));

    // 2. Текст из пары moveFrom/moveTo встречается ровно один раз
    let BlockContent::Paragraph { text } = &document.blocks[1].content else {
        panic!("Ожидался абзац с moveTo");
    };
    assert_eq!(text, "Начало. Перенесённый фрагмент");
    assert_eq!(text.matches("Перенесённый фрагмент").count(), 1);

    // 3. gridBefore=1, gridAfter=1 и дополнение строк до максимальной ширины таблицы
    assert!(matches!(
        &document.blocks[2].content,
        BlockContent::Table { rows, .. }
            if rows == &[
                ["Колонка 1", "Колонка 2", "Колонка 3"],
                ["", "Сдвинуто во 2-ю", ""],
                ["Значение 1", "Значение 2", ""],
            ]
    ));
}

#[test]
fn docx_rejects_oversized_decompressed_part_and_corrupted_archives() {
    let registry = ParserRegistry::default();

    assert!(matches!(
        registry.parse(Path::new("broken.docx"), b"not a zip archive"),
        Err(DocumentError::InvalidDocx(_))
    ));

    let empty_zip = build_docx_entries(&[("other.txt", b"hello")]);
    assert!(matches!(
        registry.parse(Path::new("missing.docx"), &empty_zip),
        Err(DocumentError::InvalidDocx(_))
    ));

    let bad_xml = build_docx("<w:document><w:body><w:p></w:document>", None);
    assert!(matches!(
        registry.parse(Path::new("bad_xml.docx"), &bad_xml),
        Err(DocumentError::InvalidDocx(_))
    ));

    // Распакованный word/document.xml больше 64 МБ отклоняется с понятной ошибкой
    let mut huge_xml = Vec::with_capacity(65 * 1024 * 1024);
    huge_xml.extend_from_slice(
        br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    huge_xml.resize(65 * 1024 * 1024, b' ');
    huge_xml.extend_from_slice(b"</w:body></w:document>");
    let oversized = build_docx_entries(&[("word/document.xml", &huge_xml)]);
    let err = registry
        .parse(Path::new("oversized.docx"), &oversized)
        .unwrap_err();
    assert!(
        matches!(&err, DocumentError::InvalidDocx(message) if message.contains("64 МБ")),
        "Неожиданная ошибка: {err}"
    );
}

#[test]
fn docx_rejects_empty_document() {
    let registry = ParserRegistry::default();
    let empty = build_docx(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>   </w:t></w:r></w:p></w:body></w:document>"#,
        None,
    );
    assert!(matches!(
        registry.parse(Path::new("empty.docx"), &empty),
        Err(DocumentError::EmptyDocument)
    ));
}
