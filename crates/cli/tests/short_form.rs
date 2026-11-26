use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/requirements.md")
}

fn rs(dir: &Path, args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(dir)
        .env("LLM_REQUESTS_PER_MINUTE", "60000")
        .args(args)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Запуск короткой формы `rs <INPUT> <REPORT> --provider mock --no-cache`.
fn short(dir: &Path, report: &Path) -> Output {
    rs(
        dir,
        &[
            sample().as_os_str(),
            report.as_os_str(),
            "--provider".as_ref(),
            "mock".as_ref(),
            "--no-cache".as_ref(),
        ],
    )
}

#[test]
fn short_form_writes_the_same_report_as_check() {
    let dir = tempfile::tempdir().unwrap();
    let short_path = dir.path().join("short.md");
    let long_path = dir.path().join("long.md");

    let result = short(dir.path(), &short_path);
    assert!(result.status.success(), "{}", stderr(&result));
    let result = rs(
        dir.path(),
        &[
            "check".as_ref(),
            sample().as_os_str(),
            "-o".as_ref(),
            long_path.as_os_str(),
            "--provider".as_ref(),
            "mock".as_ref(),
            "--no-cache".as_ref(),
        ],
    );
    assert!(result.status.success(), "{}", stderr(&result));

    let short_report = fs::read_to_string(short_path).unwrap();
    assert_eq!(short_report, fs::read_to_string(long_path).unwrap());
    assert!(short_report.contains("## 1. Паспорт прогона"));
}

#[test]
fn short_form_picks_json_by_extension_in_any_case() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["report.json", "report.JSON"] {
        let path = dir.path().join(name);
        let result = short(dir.path(), &path);
        assert!(result.status.success(), "{}", stderr(&result));
        let report: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(report["schema_version"], "1");
    }
}

#[test]
fn check_picks_format_by_output_extension_unless_format_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let input = sample();
    let check = |name: &str, format: Option<&str>| {
        let path = dir.path().join(name);
        let mut args: Vec<&std::ffi::OsStr> = vec![
            "check".as_ref(),
            input.as_os_str(),
            "-o".as_ref(),
            path.as_os_str(),
            "--provider".as_ref(),
            "mock".as_ref(),
            "--no-cache".as_ref(),
        ];
        if let Some(format) = format {
            args.extend::<[&std::ffi::OsStr; 2]>(["--format".as_ref(), format.as_ref()]);
        }
        let result = rs(dir.path(), &args);
        assert!(result.status.success(), "{}", stderr(&result));
        fs::read_to_string(path).unwrap()
    };

    let json: serde_json::Value = serde_json::from_str(&check("auto.json", None)).unwrap();
    assert_eq!(json["schema_version"], "1");
    assert!(check("forced.json", Some("md")).starts_with("# Отчёт о проверке требований"));
    assert!(check("auto.md", None).starts_with("# Отчёт о проверке требований"));
    let forced: serde_json::Value =
        serde_json::from_str(&check("forced.md", Some("json"))).unwrap();
    assert_eq!(forced["schema_version"], "1");
}

#[test]
fn short_form_writes_markdown_for_other_extensions() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["report.md", "report.txt", "report"] {
        let path = dir.path().join(name);
        let result = short(dir.path(), &path);
        assert!(result.status.success(), "{}", stderr(&result));
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("## 1. Паспорт прогона"),
            "{name}"
        );
    }
}

#[test]
fn single_argument_asks_for_the_report_path() {
    let dir = tempfile::tempdir().unwrap();
    let result = rs(dir.path(), &[sample().as_os_str()]);
    assert_eq!(result.status.code(), Some(2));
    assert!(stderr(&result).contains("укажите путь к отчёту"));
}

#[test]
fn single_missing_argument_is_reported_as_unknown_command() {
    let dir = tempfile::tempdir().unwrap();
    let result = rs(dir.path(), &["chek".as_ref()]);
    assert_eq!(result.status.code(), Some(2));
    let message = stderr(&result);
    assert!(message.contains("Неизвестная команда или нет такого файла — chek"));
    assert!(!message.contains("укажите путь к отчёту"));
}

#[test]
fn run_only_flags_need_a_document() {
    let dir = tempfile::tempdir().unwrap();
    let result = rs(dir.path(), &["--reasoning".as_ref()]);
    assert_eq!(result.status.code(), Some(2));
    assert!(stderr(&result).contains("работают при проверке документа"));
}

#[test]
fn no_arguments_still_target_chat_and_need_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    for args in [&[][..], &["--provider".as_ref(), "mock".as_ref()][..]] {
        let result = rs(dir.path(), args);
        assert_eq!(result.status.code(), Some(2));
        assert!(stderr(&result).contains("rs chat"));
    }
}

#[test]
fn subcommands_keep_working() {
    let dir = tempfile::tempdir().unwrap();
    for command in ["check", "ask", "eval", "chat", "serve", "config", "schema"] {
        let result = rs(dir.path(), &[command.as_ref(), "--help".as_ref()]);
        assert!(result.status.success(), "{command}: {}", stderr(&result));
    }
    let result = rs(dir.path(), &["check".as_ref()]);
    assert_eq!(result.status.code(), Some(2));
    assert!(stderr(&result).contains("Отсутствует обязательный аргумент"));
    let result = rs(dir.path(), &["schema".as_ref()]);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("\"title\": \"Report\""));
}
