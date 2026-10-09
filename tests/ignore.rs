use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn ignore(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .current_dir(root)
        .env("RUSTMARINE_RUNTIME_DIR", root.join("missing-runtime"))
        .env_remove("RUSTMARINE_BACKEND_DIR")
        .env_remove("RUSTMARINE_ANALYZER_DIR")
        .arg("ignore")
        .args(args)
        .output()
        .unwrap()
}

fn success(root: &Path, args: &[&str]) -> Value {
    let output = ignore(root, args);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn creates_project_configuration_without_a_runtime_and_adds_multiple_rules() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    fs::create_dir(&project).unwrap();
    let report = success(
        directory.path(),
        &["typescript:S1135", "--root", project.to_str().unwrap()],
    );
    let path = project.join(".rustmarine/config.toml");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(
        report["config_path"],
        serde_json::json!(path.canonicalize().unwrap())
    );
    assert_eq!(report["rule_key"], "typescript:S1135");
    assert_eq!(report["active"], false);
    assert_eq!(report["changed"], true);
    success(&project, &["typescript:S1134"]);
    let config: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for key in ["typescript:S1135", "typescript:S1134"] {
        assert_eq!(config["rules"][key]["active"].as_bool(), Some(false));
    }
    assert!(!project.join(".rustmarine/runs").exists());
    assert!(!directory.path().join(".rustmarine").exists());
}

#[test]
fn preserves_comments_parameters_and_other_settings_and_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join(".rustmarine")).unwrap();
    let path = directory.path().join(".rustmarine/config.toml");
    let original = "# Project settings\nbackend_dir = '../runtime/sloop'\n\n[file_classification]\ntest_patterns = ['checks/**']\n\n[properties]\n\"sonar.python.version\" = '3.12'\n\n[rules.\"typescript:S1135\"]\n# Preserve this note\nactive = true # team preference\nparameters = { format = 'example' }\n\n[rules.\"java:S100\"]\nactive = true\n";
    fs::write(&path, original).unwrap();
    assert_eq!(
        success(directory.path(), &["typescript:S1135"])["changed"],
        true
    );
    let expected = original.replace(
        "active = true # team preference",
        "active = false # team preference",
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        success(directory.path(), &["typescript:S1135"])["changed"],
        false
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
}

#[test]
fn edits_inline_rules_and_adds_new_rules_without_dropping_existing_ones() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join(".rustmarine")).unwrap();
    let path = directory.path().join(".rustmarine/config.toml");
    fs::write(&path, "# inline rules\nrules = { 'java:S100' = { active = true, parameters = { format = 'x' } } }\n").unwrap();
    success(directory.path(), &["java:S100"]);
    success(directory.path(), &["typescript:S1135"]);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# inline rules\n"));
    let config: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        config["rules"]["java:S100"]["active"].as_bool(),
        Some(false)
    );
    assert_eq!(
        config["rules"]["java:S100"]["parameters"]["format"].as_str(),
        Some("x")
    );
    assert_eq!(
        config["rules"]["typescript:S1135"]["active"].as_bool(),
        Some(false)
    );
}

#[test]
fn explicit_config_paths_are_relative_to_the_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    fs::create_dir(&project).unwrap();
    let report = success(
        directory.path(),
        &[
            "typescript:S1135",
            "--root",
            project.to_str().unwrap(),
            "--config",
            "configs/custom.toml",
        ],
    );
    let path = directory.path().join("configs/custom.toml");
    assert_eq!(
        report["config_path"],
        serde_json::json!(path.canonicalize().unwrap())
    );
    assert!(!project.join(".rustmarine").exists());
    assert_eq!(
        success(
            directory.path(),
            &["typescript:S1135", "--config", path.to_str().unwrap()]
        )["changed"],
        false
    );
}

#[test]
fn rejects_invalid_configuration_without_changing_it() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join(".rustmarine")).unwrap();
    let path = directory.path().join(".rustmarine/config.toml");
    for original in [
        "[",
        "rules = 42\n",
        "typo = true\n",
        "[rules.\"typescript:S1135\"]\nactive = 'yes'\n",
    ] {
        fs::write(&path, original).unwrap();
        let output = ignore(directory.path(), &["typescript:S1135"]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid configuration"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }
}

#[test]
fn rejects_invalid_rule_keys_before_creating_files() {
    let directory = tempfile::tempdir().unwrap();
    for key in [
        "TODO",
        "typescript:",
        ":S1135",
        "typescript:S1135:extra",
        "typescript:S1135\n[properties]",
        "",
    ] {
        let output = ignore(directory.path(), &[key]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Expected a rule key"));
        assert!(!directory.path().join(".rustmarine").exists());
    }
}

#[test]
fn reports_inaccessible_config_and_invalid_roots_without_creating_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("directory.toml");
    fs::create_dir(&path).unwrap();
    for args in [
        vec!["typescript:S1135", "--config", path.to_str().unwrap()],
        vec!["typescript:S1135", "--root", "missing"],
        vec!["typescript:S1135", "--root", "file"],
    ] {
        fs::write(directory.path().join("file"), "source").unwrap();
        let output = ignore(directory.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["error"]["message"].is_string());
    }
    assert!(!directory.path().join(".rustmarine").exists());
}

#[cfg(unix)]
#[test]
fn preserves_symlinks_and_existing_file_permissions() {
    use std::os::unix::{fs::symlink, fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("shared.toml");
    fs::write(&target, "# shared settings\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    fs::create_dir(directory.path().join(".rustmarine")).unwrap();
    let link = directory.path().join(".rustmarine/config.toml");
    symlink(&target, &link).unwrap();
    let report = success(directory.path(), &["typescript:S1135"]);
    assert_eq!(
        report["config_path"],
        serde_json::json!(target.canonicalize().unwrap())
    );
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(fs::read_to_string(&target)
        .unwrap()
        .contains("active = false"));
}

#[cfg(unix)]
#[test]
fn leaves_dangling_config_symlinks_untouched() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join(".rustmarine")).unwrap();
    let link = directory.path().join(".rustmarine/config.toml");
    symlink(directory.path().join("missing.toml"), &link).unwrap();
    let output = ignore(directory.path(), &["typescript:S1135"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!directory.path().join("missing.toml").exists());
}
