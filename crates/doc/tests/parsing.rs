use std::path::Path;

use doc::{BlockContent, BlockId, DocumentError, ParserRegistry, SourceSpan};

#[test]
fn markdown_keeps_source_lines_and_structures() {
    let source = concat!(
        "# Заголовок\r\n\r\n",
        "Две\r\nстроки.\r\n\r\n",
        "- Первый\r\n  - Вложенный\r\n\r\n",
        "| Колонка | Значение |\r\n| --- | --- |\r\n| **Имя** | `x` |\r\n\r\n",
        "```json\r\n{}\r\n```\r\n",
    );
    let registry = ParserRegistry::default();
    let path = Path::new("example.MD");
    let document = registry.parse(path, source.as_bytes()).unwrap();
    assert_eq!(document.title, "Заголовок");
    assert_eq!(document.source_path, path);
    let locations: Vec<_> = document
        .blocks
        .iter()
        .map(|block| (block.id, block.location.start_line, block.location.end_line))
        .collect();
    assert_eq!(
        locations,
        [
            (BlockId(1), 1, 1),
            (BlockId(2), 3, 4),
            (BlockId(3), 6, 7),
            (BlockId(4), 7, 7),
            (BlockId(5), 9, 11),
            (BlockId(6), 13, 15),
        ]
    );

    assert!(
        matches!(&document.blocks[2].content, BlockContent::ListItem { text } if text == "Первый")
    );
    assert!(
        matches!(&document.blocks[4].content, BlockContent::Table { rows }
        if rows == &[["Колонка", "Значение"], ["Имя", "x"]])
    );
    assert!(
        matches!(&document.blocks[5].content, BlockContent::Code { language: Some(language), text }
        if language == "json" && text == "{}\n")
    );
}

#[test]
fn plain_text_preserves_paragraph_lines_and_reports_unsupported_formats() {
    let registry = ParserRegistry::default();
    let document = registry
        .parse(Path::new("plain.txt"), b"\r\nOne\r\nTwo\r\n\r\nThree")
        .unwrap();
    assert_eq!(document.title, "plain");
    assert_eq!(document.blocks.len(), 2);
    assert_eq!(
        document.blocks[0].location,
        SourceSpan {
            start_line: 2,
            end_line: 3,
            page: None
        }
    );
    assert!(
        matches!(&document.blocks[0].content, BlockContent::Paragraph { text } if text == "One\nTwo")
    );

    for format in ["pdf", "docx"] {
        let name = format!("example.{format}");
        assert!(matches!(
            registry.parse(Path::new(&name), b""),
            Err(DocumentError::Unsupported(_))
        ));
    }
}
