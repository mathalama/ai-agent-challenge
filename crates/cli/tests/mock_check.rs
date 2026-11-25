use std::{fs, path::Path, process::Command};

#[test]
fn mock_produces_a_report_with_all_sections() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output_dir = tempfile::tempdir().unwrap();
    let report_path = output_dir.path().join("reports/report.md");
    let trace_path = output_dir.path().join("reports/trace.json");

    let result = Command::new(env!("CARGO_BIN_EXE_tz-agent"))
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
        "## 7. Уточняющие вопросы автору ТЗ",
    ] {
        remaining = remaining
            .split_once(heading)
            .unwrap_or_else(|| panic!("Нет раздела {heading}"))
            .1;
    }
    assert!(report.contains("Уведомление о расходах по карте"));
    assert!(report.contains("R-001") && report.contains("Ф-1"));
    assert!(report.contains("Нужно завершить проверки"));

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
}
