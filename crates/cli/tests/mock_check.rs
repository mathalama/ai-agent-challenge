use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[test]
fn mock_produces_a_report_with_all_sections() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("reports/report.md");
    let trace_path = output_dir.path().join("reports/trace.json");

    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output_dir.path())
        .env("LLM_REQUESTS_PER_MINUTE", "60000")
        .arg("check")
        .arg(workspace.join("samples/requirements.md"))
        .arg("-o")
        .arg(&report_path)
        .args(["--provider", "mock", "--no-cache", "--trace"])
        .arg(&trace_path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let report = fs::read_to_string(report_path).unwrap();
    let mut remaining = report.as_str();
    for heading in [
        "## 1. Паспорт прогона",
        "## 2. Нарезка требований",
        "## 3. Проверка каждого требования",
        "## 4. Проверка набора требований",
        "## 5. Сводка",
        "## 6. Итог",
        "## 7. Уточняющие вопросы автору документа",
    ] {
        remaining = remaining
            .split_once(heading)
            .unwrap_or_else(|| panic!("Нет раздела {heading}"))
            .1;
    }
    assert!(report.contains("Уведомление о расходах по карте"));
    assert!(report.contains("R-001") && report.contains("Ф-1"));
    assert!(report.contains("> Ф-1. Держатель активной карты"));
    assert!(report.contains("**Не готово.**"));
    assert!(report.contains("[testing-section-"));
    assert!(!report.contains("можно начинать"));
    assert!(remaining.contains("Ф-3") && remaining.contains("Ф-4"));
    assert!(remaining.contains("Ф-2") && remaining.contains("Февральский счётчик"));
    assert!(remaining.contains("заблокирован"));
    assert_eq!(remaining.matches("](#finding-").count(), 3);
    for invalid in ["..", "?.", "&gt;", "&lt;", "&amp;"] {
        assert!(!report.contains(invalid), "В отчёте есть {invalid}");
    }

    let trace: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(trace_path).unwrap()).unwrap();
    let stages: Vec<_> = trace["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["stage"].as_str().unwrap())
        .collect();
    assert_eq!(
        stages,
        [
            "segment",
            "facts",
            "rules",
            "requirement_checks",
            "set_checks",
            "consistency",
            "ambiguity",
            "verify",
            "clarify"
        ]
    );
    assert_eq!(trace["llm_calls"][0]["stage"], "segment");
    assert!(trace["llm_calls"][0]["duration_ms"].is_number());
    assert!(trace["llm_calls"][0]["usage"]["input_tokens"].is_number());
    assert!(
        trace["llm_calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|call| call["stage"] == "clarify")
    );
}

#[test]
fn ask_rejects_invalid_choices_and_check_shows_the_answer_without_clearing_blockers() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rs"));
    command.current_dir(output.path());
    let input = workspace.join("samples/requirements.md");
    let mut ask = Command::new(env!("CARGO_BIN_EXE_rs"));
    ask.current_dir(output.path());
    let mut ask = ask
        .arg("ask")
        .arg(&input)
        .args(["--provider", "mock", "--no-cache"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ask.stdin.take().unwrap().write_all(b"3\n0\n1\n\n").unwrap();
    let result = ask.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let answers: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(output.path().join("reports/report.answers.json")).unwrap(),
    )
    .unwrap();
    let entries = answers.as_object().unwrap();
    assert_eq!(entries.len(), 1);
    let (id, answer) = entries.iter().next().unwrap();
    assert!(id.starts_with("Q-"));
    assert_eq!(answer, "По дате совершения покупки");
    assert_eq!(
        String::from_utf8_lossy(&result.stdout)
            .matches("Допустимые номера")
            .count(),
        2
    );

    let result = command
        .arg("check")
        .arg(&input)
        .args(["--provider", "mock", "--no-cache"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = fs::read_to_string(output.path().join("reports/report.md")).unwrap();
    let (findings, questions) = report.split_once("## 7.").unwrap();
    assert!(findings.contains(&format!("Ответ {id} — По дате совершения покупки")));
    assert!(findings.contains("**Не готово.**"));
    assert!(!questions.contains(id));
    assert!(questions.contains("заблокирован"));
}

#[test]
fn check_reports_saved_answers_that_no_longer_match_any_question() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = tempfile::tempdir().unwrap();
    let reports = output.path().join("reports");
    fs::create_dir(&reports).unwrap();
    let answers = reports.join("report.answers.json");
    let saved = r#"{"Q-old":"Ранее записанный ответ"}"#;
    fs::write(&answers, saved).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output.path())
        .arg("check")
        .arg(workspace.join("samples/requirements.md"))
        .args(["--provider", "mock", "--no-cache", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("Q-old"));
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(reports.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["schema_version"], "1");
    let input = workspace.join("samples/requirements.md");
    let document = doc::ParserRegistry::default()
        .parse(&input, &fs::read(&input).unwrap())
        .unwrap();
    assert_eq!(
        report["blocks"],
        serde_json::to_value(document.blocks).unwrap()
    );
    assert_eq!(
        report["unmatched_answers"]["Q-old"],
        "Ранее записанный ответ"
    );
    assert_eq!(fs::read_to_string(answers).unwrap(), saved);
}

#[test]
fn markdown_report_keeps_character_references_from_the_source_quote() {
    let output = tempfile::tempdir().unwrap();
    let input = output.path().join("requirements.md");
    fs::write(
        &input,
        "# Требования\n\nПоказать &lt; и не менять сумму > 10.\n",
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output.path())
        .arg("check")
        .arg(&input)
        .args(["--provider", "mock", "--no-cache"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = fs::read_to_string(output.path().join("reports/report.md")).unwrap();
    assert!(report.contains(r"> Показать \&lt; и не менять сумму > 10."));
}
