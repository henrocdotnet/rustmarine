//! Run explicitly with a compatible, locally supplied Sonar distribution.
use serde_json::Value;
use std::{env, fs, process::Command};

#[test]
#[ignore = "requires RUSTMARINE_TEST_BACKEND_DIR, RUSTMARINE_TEST_ANALYZER_DIR and RUSTMARINE_TEST_JAVA"]
fn real_engine_reports_findings_and_honors_configuration() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Bad.java"),
        r#"class Bad {
  public boolean compare(String left, String right) {
    return left == right;
  }
}
"#,
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_rustmarine"));
    command
        .args(["scan", "--root"])
        .arg(project.path())
        .args(["Bad.java", "--backend-dir"])
        .arg(env::var_os("RUSTMARINE_TEST_BACKEND_DIR").expect("RUSTMARINE_TEST_BACKEND_DIR"))
        .arg("--analyzer-dir")
        .arg(env::var_os("RUSTMARINE_TEST_ANALYZER_DIR").expect("RUSTMARINE_TEST_ANALYZER_DIR"))
        .arg("--java")
        .arg(env::var_os("RUSTMARINE_TEST_JAVA").expect("RUSTMARINE_TEST_JAVA"))
        .args(["--timeout", "90"]);
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["failed_files"].as_array().unwrap().is_empty());
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["rule_key"] == "java:S4973"),
        "{report}"
    );

    // Exit threshold changes must preserve findings for agents.
    command.args(["--fail-on", "none"]);
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["rule_key"] == "java:S4973"));

    fs::write(
        project.path().join(".rustmarine.toml"),
        "[rules.\"java:S4973\"]\nactive = false\n",
    )
    .unwrap();
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["failed_files"].as_array().unwrap().is_empty());
    assert!(
        !report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["rule_key"] == "java:S4973"),
        "Rule deactivation failed: {report}"
    );
}
