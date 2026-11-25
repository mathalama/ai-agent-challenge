use std::{fs, path::Path, process::Command};

#[test]
fn eval_command_reports_metrics_for_each_golden_and_is_reproducible_with_cache() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run_dir = tempfile::tempdir().unwrap();
    let eval_dir = workspace.join("eval");

    let run_once = || {
        let output = Command::new(env!("CARGO_BIN_EXE_rs"))
            .current_dir(run_dir.path())
            .args(["eval", "--golden"])
            .arg(&eval_dir)
            .args(["--provider", "mock"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };

    let first = run_once();
    let second = run_once();
    assert_eq!(first, second);
    assert!(run_dir.path().join(".cache/llm").is_dir());

    assert!(first.contains("Уведомление о расходах по карте"));
    assert!(first.contains("Онлайн-заявка на потребительский кредит"));
    assert!(first.contains("recall:"));
    assert!(first.contains("precision:"));
    assert!(first.contains("Пропущенные дефекты"));
    assert!(first.contains("D-01"));
    assert!(first.contains("C-01"));
    // Встроенное правило testing-section должно стабильно находить пустой раздел тестирования в обоих документах
    assert!(first.contains("Найдено дефектов: 1/14"));
    assert!(first.contains("Найдено дефектов: 1/13"));
}

#[test]
fn eval_command_reports_clear_error_on_invalid_golden_file() {
    let temp = tempfile::tempdir().unwrap();
    let invalid = temp.path().join("broken.golden.json");
    fs::write(&invalid, r#"{"document":"missing.md","defects":[]}"#).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_rs"))
        .args(["eval", "--golden"])
        .arg(&invalid)
        .args(["--provider", "mock"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Ошибка:"));
    assert!(stderr.contains("не найден"));
}
