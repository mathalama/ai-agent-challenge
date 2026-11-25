use std::process::Command;

#[test]
fn report_schema_is_current_and_validates_fixtures() {
    let output_dir = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_rs"))
        .current_dir(output_dir.path())
        .arg("schema")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let generated: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let published: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/api/report.schema.json")).unwrap();
    assert_eq!(
        generated, published,
        "Обновите схему командой cargo run -p cli -- schema -o docs/api/report.schema.json"
    );

    let validator = jsonschema::validator_for(&published).unwrap();
    for (name, fixture) in [
        (
            "report.json",
            include_str!("../../../docs/api/fixtures/report.json"),
        ),
        (
            "report.gemini.json",
            include_str!("../../../docs/api/fixtures/report.gemini.json"),
        ),
    ] {
        let report: serde_json::Value = serde_json::from_str(fixture).unwrap();
        if let Err(error) = validator.validate(&report) {
            panic!("{name}: {error}");
        }
    }
}
