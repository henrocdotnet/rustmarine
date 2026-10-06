use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn cli(runtime: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustmarine"))
        .env("RUSTMARINE_RUNTIME_DIR", runtime)
        .env_remove("RUSTMARINE_BACKEND_DIR")
        .env_remove("RUSTMARINE_ANALYZER_DIR")
        .env_remove("JAVA_HOME")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn init_help_exposes_offline_and_repair() {
    let temp = tempfile::tempdir().unwrap();
    let output = cli(temp.path(), &["init", "--help"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("--offline") && text.contains("--repair") && text.contains("--runtime-dir")
    );
}

#[test]
fn offline_init_cannot_activate_a_partial_environment() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    let output = cli(&runtime, &["init", "--offline"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Offline cache is missing or corrupt"));
    let releases = fs::read_dir(runtime.join("releases")).unwrap().count();
    assert_eq!(
        releases, 0,
        "Incomplete staging directories must be removed"
    );
}

#[cfg(unix)]
fn fake_managed_runtime(runtime: &Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let manifest: Value = serde_json::from_str(include_str!("../runtime-manifest.json")).unwrap();
    let release = runtime.join("releases").join(format!(
        "{}-{}-{}",
        manifest["id"].as_str().unwrap(),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    for directory in ["backend/lib", "analyzers", "java/bin", "node/bin"] {
        fs::create_dir_all(release.join(directory)).unwrap();
    }
    let java = release.join("java/bin/java");
    fs::write(&java, "#!/bin/sh\necho 'openjdk version \"21.0.1\"' >&2\n").unwrap();
    fs::set_permissions(&java, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(release.join("node/bin/node"), "test").unwrap();
    fs::write(release.join("analyzers/test.jar"), "test").unwrap();
    fs::write(release.join("installation.json"), serde_json::to_vec(&serde_json::json!({
        "schema_version":1,
        "manifest_sha256":format!("{:x}", Sha256::digest(include_bytes!("../runtime-manifest.json"))),
        "java":"java/bin/java", "node":"node/bin/node", "files":{}
    })).unwrap()).unwrap();
    release
}

#[test]
#[cfg(unix)]
fn doctor_discovers_managed_runtime_without_external_settings() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    let release = fake_managed_runtime(&runtime);
    let output = cli(
        &runtime,
        &["doctor", "--root", temp.path().to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["java"],
        release.join("java/bin/java").to_str().unwrap()
    );
    assert_eq!(
        report["backend_dir"],
        release
            .canonicalize()
            .unwrap()
            .join("backend")
            .to_str()
            .unwrap()
    );
}

#[test]
#[cfg(unix)]
fn failed_offline_repair_preserves_previous_installation() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    let release = fake_managed_runtime(&runtime);
    let before = fs::read(release.join("installation.json")).unwrap();
    let output = cli(&runtime, &["init", "--repair", "--offline"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Offline cache is missing or corrupt"));
    assert_eq!(fs::read(release.join("installation.json")).unwrap(), before);
    assert!(release.join("java/bin/java").is_file());
    assert_eq!(fs::read_dir(runtime.join("releases")).unwrap().count(), 1);
}
