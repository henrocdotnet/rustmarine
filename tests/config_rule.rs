use serde_json::{json, Value};
use std::{fs, path::Path, process::Command};

fn rule(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .current_dir(root)
        .env("RUSTMARINE_RUNTIME_DIR", root.join("missing-runtime"))
        .env_remove("RUSTMARINE_BACKEND_DIR")
        .env_remove("RUSTMARINE_ANALYZER_DIR")
        .args(["config", "rule"])
        .args(args)
        .output()
        .unwrap()
}

fn success(root: &Path, args: &[&str]) -> Value {
    let output = rule(root, args);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn config(path: &Path) -> toml::Value {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn reads_and_deletes_missing_overrides_without_creating_files() {
    let directory = tempfile::tempdir().unwrap();
    for extra in [vec![], vec!["--config", "missing/custom.toml"]] {
        let mut list = vec!["list"];
        list.extend_from_slice(&extra);
        assert_eq!(success(directory.path(), &list)["rules"], json!({}));
        for command in ["get", "delete"] {
            let mut args = vec![command, "go:S3776"];
            args.extend_from_slice(&extra);
            let report = success(directory.path(), &args);
            assert_eq!(report["configured"], false);
            assert!(report["rule"].is_null());
            assert!(Path::new(report["config_path"].as_str().unwrap()).is_absolute());
            if command == "delete" {
                assert_eq!(report["changed"], false);
            }
        }
    }
    let report = success(directory.path(), &["delete", "go:S3776", "threshold"]);
    assert_eq!(report["changed"], false);
    assert_eq!(
        success(directory.path(), &["list", "--language", "go"])["rules"],
        json!({})
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn list_and_get_inspect_saved_overrides_without_resolving_runtime_paths_or_writing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    let original = "# config\nbackend_dir = 'missing/runtime'\n\n[rules.\"typescript:S1135\"]\nactive = false\n\n[rules.\"go:S3776\"]\nactive = true\nparameters = { threshold = '20' }\n";
    fs::write(&path, original).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let report = success(directory.path(), &["list", "--config", "custom.toml"]);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["config_path"], json!(path.canonicalize().unwrap()));
    assert_eq!(
        report["rules"],
        json!({
            "go:S3776": {"active": true, "parameters": {"threshold": "20"}},
            "typescript:S1135": {"active": false, "parameters": {}},
        })
    );
    let report = success(
        directory.path(),
        &["get", "go:S3776", "--config", "custom.toml"],
    );
    assert_eq!(report["configured"], true);
    assert_eq!(
        report["rule"],
        json!({"active": true, "parameters": {"threshold": "20"}})
    );
    assert!(!report.as_object().unwrap().contains_key("changed"));
    let report = success(
        directory.path(),
        &["get", "java:S3776", "--config", "custom.toml"],
    );
    assert_eq!(report["configured"], false);
    assert!(report["rule"].is_null());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
}

#[test]
fn list_filters_by_exact_language_prefix_without_modifying_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    fs::create_dir_all(project.join(".rustmarine")).unwrap();
    let path = project.join(".rustmarine/config.toml");
    let original = "# language overrides\nbackend_dir = 'missing/runtime'\n\n[rules.\"go:S3776\"]\nactive = true\nparameters = { threshold = '20' }\n\n[rules.\"go:S1135\"]\nactive = false\n\n[rules.\"go-other:S3776\"]\nactive = true\n\n[rules.\"typescript:S1135\"]\nactive = false\n\n[rules.\"javascript:S1135\"]\nactive = true\n";
    fs::write(&path, original).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let all = success(
        directory.path(),
        &["list", "--root", project.to_str().unwrap()],
    );
    assert_eq!(all["rules"].as_object().unwrap().len(), 5);
    for (language, keys) in [
        ("go", vec!["go:S1135", "go:S3776"]),
        ("typescript", vec!["typescript:S1135"]),
        ("javascript", vec!["javascript:S1135"]),
        ("ruby", vec![]),
        ("Go", vec![]),
    ] {
        let report = success(
            directory.path(),
            &[
                "list",
                "--language",
                language,
                "--root",
                project.to_str().unwrap(),
            ],
        );
        assert_eq!(report["config_path"], all["config_path"]);
        let rules = report["rules"].as_object().unwrap();
        assert_eq!(rules.keys().map(String::as_str).collect::<Vec<_>>(), keys);
        for key in keys {
            assert_eq!(rules[key], all["rules"][key]);
        }
    }
    let report = success(
        directory.path(),
        &[
            "list",
            "--language",
            "typescript",
            "--config",
            "project/.rustmarine/config.toml",
        ],
    );
    assert_eq!(
        report["rules"],
        json!({"typescript:S1135": {"active": false, "parameters": {}}})
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert!(!directory.path().join(".rustmarine").exists());
}

#[test]
fn sets_activation_and_multiple_parameters_and_round_trips_arbitrary_strings() {
    let directory = tempfile::tempdir().unwrap();
    let assignments = [
        "max=7",
        "constructorMax=10",
        "float=1.5",
        "negative=-1",
        "boolean=true",
        "format=\"quoted\"\n[properties]\nvalue=escaped\\",
    ];
    let mut args = vec!["set", "java:S107", "--active", "false"];
    args.extend_from_slice(&assignments);
    let report = success(directory.path(), &args);
    assert_eq!(report["rule"]["active"], false);
    assert_eq!(report["rule"]["parameters"]["max"], "7");
    assert_eq!(report["rule"]["parameters"]["constructorMax"], "10");
    assert_eq!(report["rule"]["parameters"]["float"], "1.5");
    assert_eq!(report["rule"]["parameters"]["negative"], "-1");
    assert_eq!(report["rule"]["parameters"]["boolean"], "true");
    assert_eq!(
        report["rule"]["parameters"]["format"],
        "\"quoted\"\n[properties]\nvalue=escaped\\"
    );
    let report = success(directory.path(), &["set", "java:S107", "--active", "true"]);
    assert_eq!(report["changed"], true);
    assert_eq!(report["rule"]["active"], true);
    assert_eq!(report["rule"]["parameters"]["constructorMax"], "10");
    let path = directory.path().join(".rustmarine/config.toml");
    assert!(config(&path).get("properties").is_none());
    let text = fs::read_to_string(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        success(directory.path(), &["set", "java:S107", "--active", "true"])["changed"],
        false
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    let report = success(directory.path(), &["set", "go:S3776", "--active", "false"]);
    assert_eq!(report["rule"], json!({"active": false, "parameters": {}}));
}

#[test]
fn deletion_removes_whole_overrides_preserving_other_settings_in_normal_and_inline_tables() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    for original in [
        "# project\nbackend_dir = 'runtime'\n\n[rules.\"go:S3776\"]\nactive = false\n[rules.\"go:S3776\".parameters]\nthreshold = '20'\n\n[rules.\"java:S107\"]\nactive = true\nparameters = { max = '7', constructorMax = '10' }\n",
        "# project\nbackend_dir = 'runtime'\nrules = { 'go:S3776' = { active = false, parameters = { threshold = '20' } }, 'java:S107' = { active = true, parameters = { max = '7', constructorMax = '10' } } }\n",
    ] {
        fs::write(&path, original).unwrap();
        let report = success(directory.path(), &["delete", "go:S3776", "--config", "custom.toml"]);
        assert_eq!(report["changed"], true);
        assert_eq!(report["configured"], false);
        assert!(report["rule"].is_null());
        let after = config(&path);
        assert!(after["rules"].get("go:S3776").is_none());
        assert_eq!(after["rules"]["java:S107"]["parameters"]["constructorMax"].as_str(), Some("10"));
        assert_eq!(after["backend_dir"].as_str(), Some("runtime"));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# project\n"));
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(success(directory.path(), &["delete", "go:S3776", "--config", "custom.toml"])["changed"], false);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    }
}

#[test]
fn deletion_of_selected_parameters_preserves_activation_other_parameters_and_comments() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    for original in [
        "# project\n[rules.\"java:S107\"]\nactive = false # disabled\n[rules.\"java:S107\".parameters]\nmax = '7' # keep note\nconstructorMax = '10'\nextra = 'value'\n",
        "# project\nrules = { 'java:S107' = { active = false, parameters = { max = '7', constructorMax = '10', extra = 'value' } } }\n",
    ] {
        fs::write(&path, original).unwrap();
        let args = ["delete", "java:S107", "constructorMax", "extra", "absent", "--config", "custom.toml"];
        let report = success(directory.path(), &args);
        assert_eq!(report["changed"], true);
        assert_eq!(report["configured"], true);
        assert_eq!(report["rule"], json!({"active": false, "parameters": {"max": "7"}}));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# project\n"));
        if original.contains("# keep note") {
            assert!(text.contains("max = '7' # keep note"));
            assert!(text.contains("active = false # disabled"));
        }
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(success(directory.path(), &args)["changed"], false);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        let report = success(directory.path(), &["delete", "java:S107", "max", "--config", "custom.toml"]);
        assert_eq!(report["rule"], json!({"active": false, "parameters": {}}));
        assert_eq!(report["configured"], true);
    }
}

#[test]
fn set_requires_changes_and_rejects_duplicate_assignments_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["set", "go:S3776"],
        vec!["set", "go:S3776", "threshold=20", "threshold=25"],
    ] {
        let output = rule(directory.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(error["error"]["message"].is_string());
        assert!(!directory.path().join(".rustmarine").exists());
    }
}

#[test]
fn all_operations_reject_malformed_configuration_without_changing_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    let original = "[rules.\"go:S3776\"]\nactive = true\nparameters = { threshold = 15 }\n";
    fs::write(&path, original).unwrap();
    for args in [
        vec!["list"],
        vec!["get", "go:S3776"],
        vec!["set", "go:S3776", "threshold=20"],
        vec!["delete", "go:S3776"],
    ] {
        let mut args = args;
        args.extend_from_slice(&["--config", "custom.toml"]);
        let output = rule(directory.path(), &args);
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
fn creates_configuration_in_the_target_project_without_a_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    fs::create_dir(&project).unwrap();
    let report = success(
        directory.path(),
        &[
            "set",
            "go:S3776",
            "threshold=20",
            "--root",
            project.to_str().unwrap(),
        ],
    );
    let path = project.join(".rustmarine/config.toml");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["config_path"], json!(path.canonicalize().unwrap()));
    assert_eq!(report["rule_key"], "go:S3776");
    assert_eq!(report["rule"]["parameters"]["threshold"], "20");
    assert_eq!(report["rule"]["active"], true);
    assert_eq!(report["configured"], true);
    assert_eq!(report["changed"], true);
    let config = config(&path);
    assert_eq!(config["rules"]["go:S3776"]["active"].as_bool(), Some(true));
    assert_eq!(
        config["rules"]["go:S3776"]["parameters"]["threshold"].as_str(),
        Some("20")
    );
    assert!(!directory.path().join(".rustmarine").exists());
    assert!(!project.join(".rustmarine/runs").exists());
}

#[test]
fn updates_parameter_tables_preserving_comments_disabled_state_and_other_settings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    let original = "# Team configuration\nbackend_dir = '../runtime'\n\n[file_classification]\ntest_patterns = ['checks/**']\n\n[rules.\"go:S3776\"]\nactive = false # deliberately disabled\n[rules.\"go:S3776\".parameters]\n# Complexity policy\nthreshold = '15' # keep note\nother = 'unchanged'\n\n[rules.\"java:S100\"]\nactive = true\nparameters = { format = 'pattern' }\n";
    fs::write(&path, original).unwrap();
    let report = success(
        directory.path(),
        &["set", "go:S3776", "threshold=20", "--config", "custom.toml"],
    );
    assert_eq!(report["rule"]["active"], false);
    assert_eq!(report["changed"], true);
    let expected = original.replace("threshold = '15'", "threshold = \"20\"");
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let report = success(
        directory.path(),
        &["set", "go:S3776", "threshold=20", "--config", "custom.toml"],
    );
    assert_eq!(report["changed"], false);
    assert_eq!(report["rule"]["active"], false);
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
}

#[test]
fn edits_inline_parameters_and_adds_multiple_case_sensitive_string_parameters() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    fs::write(&path, "# inline config\nrules = { 'go:S3776' = { active = true, parameters = { threshold = '15', other = 'keep' } } }\n").unwrap();
    success(
        directory.path(),
        &["set", "go:S3776", "threshold=20", "--config", "custom.toml"],
    );
    success(
        directory.path(),
        &[
            "set",
            "java:S3776",
            "Threshold=25",
            "--config",
            "custom.toml",
        ],
    );
    success(
        directory.path(),
        &[
            "set",
            "go:S3776",
            "maximum=0",
            "format=^name=.*$",
            "ignoreStrings=application/json,text/plain",
            "empty=",
            "--config",
            "custom.toml",
        ],
    );
    let config = config(&path);
    let go = &config["rules"]["go:S3776"]["parameters"];
    assert_eq!(go["threshold"].as_str(), Some("20"));
    assert_eq!(go["other"].as_str(), Some("keep"));
    assert_eq!(go["maximum"].as_str(), Some("0"));
    assert_eq!(go["format"].as_str(), Some("^name=.*$"));
    assert_eq!(
        go["ignoreStrings"].as_str(),
        Some("application/json,text/plain")
    );
    assert_eq!(go["empty"].as_str(), Some(""));
    let java = &config["rules"]["java:S3776"]["parameters"];
    assert_eq!(java["Threshold"].as_str(), Some("25"));
    assert!(java.get("threshold").is_none());
    assert!(fs::read_to_string(&path)
        .unwrap()
        .starts_with("# inline config\n"));
}

#[test]
fn adds_parameters_to_existing_rules_and_preserves_ignore_preferences() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .current_dir(directory.path())
        .args(["ignore", "go:S3776"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report = success(directory.path(), &["set", "go:S3776", "threshold=20"]);
    assert_eq!(report["rule"]["active"], false);
    let output = Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .current_dir(directory.path())
        .args(["ignore", "go:S3776"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let config = config(&directory.path().join(".rustmarine/config.toml"));
    assert_eq!(config["rules"]["go:S3776"]["active"].as_bool(), Some(false));
    assert_eq!(
        config["rules"]["go:S3776"]["parameters"]["threshold"].as_str(),
        Some("20")
    );
}

#[test]
fn explicit_config_is_relative_to_the_working_directory_and_creates_parent_directories() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    fs::create_dir(&project).unwrap();
    success(
        directory.path(),
        &[
            "set",
            "go:S3776",
            "threshold=20",
            "--root",
            project.to_str().unwrap(),
            "--config",
            "settings/custom.toml",
        ],
    );
    let path = directory.path().join("settings/custom.toml");
    assert!(path.is_file());
    assert!(!project.join(".rustmarine").exists());
    assert_eq!(
        success(
            directory.path(),
            &[
                "set",
                "go:S3776",
                "threshold=20",
                "--config",
                path.to_str().unwrap()
            ]
        )["changed"],
        false
    );
}

#[test]
fn malformed_configuration_is_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    for original in [
        "[",
        "unknown_setting = true\n",
        "rules = 42\n",
        "[rules.\"go:S3776\"]\nactive = true\nparameters = { threshold = 15 }\n",
    ] {
        fs::write(&path, original).unwrap();
        let output = rule(
            directory.path(),
            &["set", "go:S3776", "threshold=20", "--config", "custom.toml"],
        );
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
fn invalid_assignments_and_names_fail_before_touching_configuration() {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["set", "go:S3776", "20"],
        vec!["set", "go:S3776", "=20"],
        vec!["set", "S3776", "threshold=20"],
        vec!["set", "go:S3776", "threshold\nactive=20"],
        vec!["set", "go:S3776", "param:extra=20"],
        vec!["set", "go:S3776", "--active", "yes"],
        vec!["get", "go:S3776:extra"],
        vec!["delete", "go:S3776", ""],
        vec!["delete", "go:S3776", "threshold=20"],
        vec!["list", "--language", "go:S3776"],
        vec!["list", "--language", ""],
    ] {
        let output = rule(directory.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(!output.stderr.is_empty());
        assert!(!directory.path().join(".rustmarine").exists());
    }
}

#[test]
fn invalid_roots_and_unreadable_configs_return_json_errors() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("directory.toml")).unwrap();
    fs::write(directory.path().join("file"), "source").unwrap();
    for args in [
        vec!["set", "go:S3776", "threshold=20", "--root", "missing"],
        vec!["get", "go:S3776", "--root", "file"],
        vec!["list", "--config", "directory.toml"],
    ] {
        let output = rule(directory.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(error["error"]["message"].is_string());
    }
    assert!(!directory.path().join(".rustmarine").exists());
}

#[cfg(unix)]
#[test]
fn edits_symlink_targets_preserving_permissions_and_rejects_dangling_links() {
    use std::os::unix::{fs::symlink, fs::PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("shared.toml");
    fs::write(&target, "# shared\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let link = directory.path().join("link.toml");
    symlink(&target, &link).unwrap();
    let report = success(
        directory.path(),
        &["set", "go:S3776", "threshold=20", "--config", "link.toml"],
    );
    assert_eq!(report["config_path"], json!(target.canonicalize().unwrap()));
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        config(&target)["rules"]["go:S3776"]["parameters"]["threshold"].as_str(),
        Some("20")
    );
    let dangling = directory.path().join("dangling.toml");
    symlink(directory.path().join("missing.toml"), &dangling).unwrap();
    let output = rule(
        directory.path(),
        &["delete", "go:S3776", "--config", "dangling.toml"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!directory.path().join("missing.toml").exists());
}
