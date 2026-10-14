//! Owns a versioned, verified Sonar runtime without depending on any IDE installation.
use crate::{config, engine};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

const RECEIPT: &str = "installation.json";
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    id: String,
    backend_version: String,
    artifacts: Vec<Artifact>,
    java: BTreeMap<String, Artifact>,
    node: BTreeMap<String, Artifact>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    name: String,
    version: String,
    url: String,
    sha256: String,
    kind: ArchiveKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ArchiveKind {
    Jar,
    Zip,
    TarGz,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    manifest_sha256: String,
    java: PathBuf,
    node: PathBuf,
    files: BTreeMap<PathBuf, String>,
}

#[derive(Debug, Serialize)]
pub struct Installation {
    pub runtime_dir: PathBuf,
    pub release_dir: PathBuf,
    pub release: String,
    pub backend_version: String,
    pub java_version: String,
    pub node_version: String,
    pub analyzer_versions: BTreeMap<String, String>,
    pub backend_dir: PathBuf,
    pub analyzer_dir: PathBuf,
    pub java: PathBuf,
    pub node: PathBuf,
}

fn manifest() -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(include_str!("../runtime-manifest.json"))?;
    if manifest.schema_version != 1 {
        bail!("Unsupported runtime manifest schema");
    }
    safe_relative(Path::new(&manifest.id))?;
    for artifact in manifest
        .artifacts
        .iter()
        .chain(manifest.java.values())
        .chain(manifest.node.values())
    {
        let url = url::Url::parse(&artifact.url)?;
        if url.scheme() != "https" || url.host_str().is_none() {
            bail!("Runtime downloads require HTTPS");
        }
        if artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
            bail!("Invalid SHA-256 in runtime manifest");
        }
        artifact.filename()?;
    }
    Ok(manifest)
}

fn manifest_digest() -> String {
    format!(
        "{:x}",
        Sha256::digest(include_bytes!("../runtime-manifest.json"))
    )
}

fn platform() -> Result<String> {
    if cfg!(target_env = "musl") {
        bail!("The managed runtime requires glibc Linux; musl/Alpine is not supported by this catalog");
    }
    let key = format!("{}-{}", env::consts::OS, env::consts::ARCH);
    if !manifest()?.java.contains_key(&key) {
        bail!("No managed runtime for {key}; configure external runtime paths instead");
    }
    Ok(key)
}

pub fn root(explicit: Option<&Path>) -> Result<PathBuf> {
    let path = explicit
        .map(Path::to_owned)
        .or_else(|| env::var_os("RUSTMARINE_RUNTIME_DIR").map(PathBuf::from))
        .or_else(|| dirs::data_local_dir().map(|p| p.join("rustmarine")))
        .context("Cannot determine runtime directory; pass --runtime-dir")?;
    Ok(if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    })
}

fn release_dir(root: &Path, manifest: &Manifest) -> Result<PathBuf> {
    Ok(root
        .join("releases")
        .join(format!("{}-{}", manifest.id, platform()?)))
}

fn safe_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("Unsafe archive or installation path: {}", path.display());
    }
    // Windows path syntax is rejected on Unix as well, so archives are portable.
    let foreign_separator = cfg!(not(windows)) && path.to_string_lossy().contains('\\');
    if foreign_separator || path.to_string_lossy().contains(':') {
        bail!("Unsafe archive path: {}", path.display());
    }
    Ok(())
}

impl Artifact {
    fn filename(&self) -> Result<String> {
        let url = url::Url::parse(&self.url)?;
        let filename = url
            .path_segments()
            .and_then(|mut p| p.next_back())
            .context("Artifact URL has no filename")?;
        safe_relative(Path::new(filename))?;
        if Path::new(filename).components().count() != 1 {
            bail!("Invalid artifact filename");
        }
        Ok(filename.to_owned())
    }
}

fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn download(
    client: &reqwest::blocking::Client,
    artifact: &Artifact,
    cache: &Path,
    offline: bool,
) -> Result<PathBuf> {
    let path = cache.join(format!("{}-{}", artifact.sha256, artifact.filename()?));
    if path.is_file() && hash(&path)? == artifact.sha256 {
        return Ok(path);
    }
    if offline {
        bail!(
            "Offline cache is missing or corrupt: {} {}. Run rustmarine init online first.",
            artifact.name,
            artifact.version
        );
    }
    eprintln!("Downloading {} {}", artifact.name, artifact.version);
    let response = client
        .get(&artifact.url)
        .send()
        .with_context(|| format!("Cannot download {}", artifact.url))?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD)
    {
        bail!("Artifact exceeds download size limit");
    }
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    let count = io::copy(&mut response.take(MAX_DOWNLOAD + 1), &mut temporary)?;
    if count > MAX_DOWNLOAD {
        bail!("Artifact exceeds download size limit");
    }
    temporary.flush()?;
    if hash(temporary.path())? != artifact.sha256 {
        bail!(
            "SHA-256 verification failed for {} {}",
            artifact.name,
            artifact.version
        );
    }
    temporary.as_file().sync_all()?;
    // Remove only an invalid cache entry, never a verified download.
    if path.exists() {
        fs::remove_file(&path)?;
    }
    temporary.persist(&path).map_err(|e| e.error)?;
    Ok(path)
}

fn unpack_zip(archive: &Path, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive)?)?;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let path = PathBuf::from(entry.name());
        safe_relative(&path)?;
        let mode = entry.unix_mode().unwrap_or(0);
        if mode & 0o170000 == 0o120000 {
            bail!("ZIP symlinks are not supported: {}", path.display());
        }
        total = total
            .checked_add(entry.size())
            .context("Archive size overflow")?;
        if total > MAX_UNPACKED {
            bail!("Archive exceeds extraction size limit");
        }
        let output = destination.join(&path);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        fs::create_dir_all(output.parent().context("Archive path has no parent")?)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        io::copy(&mut entry, &mut file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &output,
                fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
            )?;
        }
    }
    Ok(())
}

fn safe_link(path: &Path, target: &Path, hardlink: bool) -> Result<()> {
    if target.is_absolute() {
        bail!("Archive link points outside extraction directory");
    }
    let mut depth = if hardlink {
        0
    } else {
        path.parent().map_or(0, |p| {
            p.components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count()
        })
    };
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => bail!("Archive link points outside extraction directory"),
        }
    }
    Ok(())
}

fn unpack_tar(archive: &Path, destination: &Path) -> Result<()> {
    let reader = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut archive = tar::Archive::new(reader);
    let mut total = 0u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        safe_relative(&path)?;
        let kind = entry.header().entry_type();
        total = total
            .checked_add(entry.size())
            .context("Archive size overflow")?;
        if total > MAX_UNPACKED {
            bail!("Archive exceeds extraction size limit");
        }
        if kind.is_symlink() || kind.is_hard_link() {
            let target = entry.link_name()?.context("Archive link has no target")?;
            safe_link(&path, &target, kind.is_hard_link())?;
        } else if !kind.is_file() && !kind.is_dir() {
            bail!("Unsupported special archive entry: {}", path.display());
        }
        if !entry.unpack_in(destination)? {
            bail!("Archive entry escapes extraction directory");
        }
    }
    Ok(())
}

fn unpack(archive: &Path, destination: &Path, kind: ArchiveKind) -> Result<()> {
    fs::create_dir_all(destination)?;
    match kind {
        ArchiveKind::Zip => unpack_zip(archive, destination),
        ArchiveKind::TarGz => unpack_tar(archive, destination),
        ArchiveKind::Jar => bail!("A JAR must be copied as an analyzer"),
    }
}

fn file_inventory(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut result = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &path, result)?;
            } else if kind.is_file() {
                result.insert(path.strip_prefix(root)?.to_owned(), hash(&path)?);
            } else if kind.is_symlink() && !path.canonicalize()?.starts_with(root.canonicalize()?) {
                bail!("Installed symlink escapes runtime");
            }
        }
        Ok(())
    }
    walk(root, root, &mut result)?;
    Ok(result)
}

fn find_executable(directory: &Path, name: &str) -> Result<PathBuf> {
    let mut found = Vec::new();
    fn walk(dir: &Path, name: &str, found: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), name, found)?;
            } else if entry.file_name() == name {
                found.push(entry.path());
            }
        }
        Ok(())
    }
    walk(directory, name, &mut found)?;
    if found.len() != 1 {
        bail!("Expected one {name} executable, found {}", found.len());
    }
    Ok(found.remove(0))
}

fn installation(
    root: PathBuf,
    release: PathBuf,
    receipt: &Receipt,
    manifest: &Manifest,
) -> Result<Installation> {
    safe_relative(&receipt.java)?;
    safe_relative(&receipt.node)?;
    Ok(Installation {
        runtime_dir: root,
        release: manifest.id.clone(),
        backend_version: manifest.backend_version.clone(),
        java_version: manifest.java[&platform()?].version.clone(),
        node_version: manifest.node[&platform()?].version.clone(),
        analyzer_versions: manifest
            .artifacts
            .iter()
            .filter(|a| matches!(a.kind, ArchiveKind::Jar))
            .map(|a| (a.name.clone(), a.version.clone()))
            .collect(),
        backend_dir: release.join("backend"),
        analyzer_dir: release.join("analyzers"),
        java: release.join(&receipt.java),
        node: release.join(&receipt.node),
        release_dir: release,
    })
}

pub fn load(explicit: Option<&Path>) -> Result<Option<Installation>> {
    let root = root(explicit)?;
    let manifest = manifest()?;
    let release = release_dir(&root, &manifest)?;
    if !release.exists() {
        return Ok(None);
    }
    let receipt: Receipt = serde_json::from_slice(
        &fs::read(release.join(RECEIPT))
            .context("Managed runtime is incomplete; run rustmarine init --repair")?,
    )?;
    if receipt.schema_version != 1 || receipt.manifest_sha256 != manifest_digest() {
        bail!("Managed runtime catalog mismatch; run rustmarine init --repair");
    }
    for path in receipt.files.keys() {
        safe_relative(path)?;
        if !release.join(path).is_file() {
            bail!(
                "Managed runtime file is missing: {}. Run rustmarine init --repair",
                path.display()
            );
        }
    }
    let runtime = installation(root, release, &receipt, &manifest)?;
    if !runtime.java.is_file()
        || !runtime.node.is_file()
        || !runtime.backend_dir.join("lib").is_dir()
        || !runtime.analyzer_dir.is_dir()
    {
        bail!("Managed runtime is incomplete; run rustmarine init --repair");
    }
    Ok(Some(runtime))
}

fn smoke(runtime: &Installation) -> Result<()> {
    for (path, flag, expected) in [
        (&runtime.java, "-version", runtime.java_version.clone()),
        (
            &runtime.node,
            "--version",
            format!("v{}", runtime.node_version),
        ),
    ] {
        let output = Command::new(path)
            .arg(flag)
            .output()
            .with_context(|| format!("Cannot execute {}", path.display()))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() || !text.contains(&expected) {
            bail!(
                "Managed runtime version check failed for {}: {text}",
                path.display()
            );
        }
    }
    let project = tempfile::tempdir()?;
    let root = project.path().canonicalize()?;
    let fixtures = [
        (
            "Check.java",
            "class Check { boolean compare(String a, String b) { return a == b; } }\n",
            "java:",
        ),
        (
            "check.py",
            "def bad():\n    if True:\n        return 1\n    return 0\n",
            "python:",
        ),
        (
            "check.js",
            "function compare(value) { return value == value; }\n",
            "javascript:",
        ),
    ];
    for (name, source, _) in fixtures {
        fs::write(root.join(name), source)?;
    }
    let files = fixtures
        .iter()
        .map(|(name, _, _)| root.join(name))
        .collect::<Vec<_>>();
    let options = engine::EngineOptions {
        backend_dir: runtime.backend_dir.clone(),
        plugins: config::plugins(Some(&runtime.analyzer_dir), &[])?,
        java: runtime.java.clone(),
        node: Some(runtime.node.clone()),
        timeout: Duration::from_secs(180),
        properties: BTreeMap::new(),
        rules: BTreeMap::new(),
        verbose: false,
        log: None,
    };
    let classifier = crate::classification::FileClassifier::new(&Default::default())?;
    let report = engine::analyze(&options, &root, &files, &classifier)
        .context("Managed runtime analysis self-test failed")?;
    if !report.failed_files.is_empty() || !report.skipped_files.is_empty() {
        bail!(
            "Runtime self-test failed or excluded source files: {:?} {:?}",
            report.failed_files,
            report.skipped_files
        );
    }
    for (_, _, prefix) in fixtures {
        if !report.findings.iter().any(|issue| {
            issue
                .get("ruleKey")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|key| key.starts_with(prefix))
        }) {
            bail!(
                "Runtime self-test produced no {prefix} findings; installation was not activated"
            );
        }
    }
    Ok(())
}

pub fn init(explicit: Option<&Path>, offline: bool, repair: bool) -> Result<Installation> {
    let root = root(explicit)?;
    fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".install.lock"))?;
    lock.lock_exclusive()
        .context("Cannot lock runtime installation")?;
    let manifest = manifest()?;
    let platform = platform()?;
    let destination = release_dir(&root, &manifest)?;
    if !repair {
        if let Some(runtime) = load(Some(&root))? {
            let receipt: Receipt = serde_json::from_slice(&fs::read(destination.join(RECEIPT))?)?;
            for (path, expected) in receipt.files {
                if hash(&destination.join(&path))? != expected {
                    bail!(
                        "Managed runtime file is corrupt: {}. Run rustmarine init --repair",
                        path.display()
                    );
                }
            }
            eprintln!("Checking installed runtime {}", manifest.id);
            smoke(&runtime)?;
            return Ok(runtime);
        }
    }
    let releases = root.join("releases");
    let cache = root.join("downloads");
    fs::create_dir_all(&releases)?;
    fs::create_dir_all(&cache)?;
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&releases)?;
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("rustmarine/", env!("CARGO_PKG_VERSION")))
        .build()?;
    for artifact in &manifest.artifacts {
        let archive = download(&client, artifact, &cache, offline)?;
        match artifact.kind {
            ArchiveKind::Jar => {
                let dir = staging.path().join("analyzers");
                fs::create_dir_all(&dir)?;
                fs::copy(archive, dir.join(artifact.filename()?))?;
            }
            _ => unpack(&archive, &staging.path().join("backend"), artifact.kind)?,
        }
    }
    for (name, artifact) in [
        ("java", &manifest.java[&platform]),
        ("node", &manifest.node[&platform]),
    ] {
        let archive = download(&client, artifact, &cache, offline)?;
        unpack(&archive, &staging.path().join(name), artifact.kind)?;
    }
    let java = find_executable(
        &staging.path().join("java"),
        if cfg!(windows) { "java.exe" } else { "java" },
    )?;
    let node = find_executable(
        &staging.path().join("node"),
        if cfg!(windows) { "node.exe" } else { "node" },
    )?;
    let receipt = Receipt {
        schema_version: 1,
        manifest_sha256: manifest_digest(),
        java: java.strip_prefix(staging.path())?.to_owned(),
        node: node.strip_prefix(staging.path())?.to_owned(),
        files: BTreeMap::new(),
    };
    let candidate = installation(root.clone(), staging.path().to_owned(), &receipt, &manifest)?;
    eprintln!("Verifying Java, Python, and JavaScript analysis");
    smoke(&candidate)?;
    let receipt = Receipt {
        files: file_inventory(staging.path())?,
        ..receipt
    };
    fs::write(
        staging.path().join(RECEIPT),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    // Only verified installations become discoverable. Preserve the old runtime if activation fails.
    let backup = tempfile::Builder::new()
        .prefix(".previous-")
        .tempdir_in(&releases)?;
    let old = backup.path().join("runtime");
    if destination.exists() {
        fs::rename(&destination, &old)?;
    }
    if let Err(error) = fs::rename(staging.path(), &destination) {
        if old.exists() {
            if let Err(restore) = fs::rename(&old, &destination) {
                let preserved = backup.keep();
                bail!("Cannot activate runtime ({error}) or restore it ({restore}); previous installation preserved at {}", preserved.join("runtime").display());
            }
        }
        return Err(error).context("Cannot activate runtime");
    }
    match load(Some(&root)).and_then(|runtime| runtime.context("Runtime activation failed")) {
        Ok(runtime) => Ok(runtime),
        Err(error) => {
            // Undo publication before allowing the previous installation to be removed.
            if let Err(restore) = fs::rename(&destination, staging.path()).and_then(|_| {
                if old.exists() {
                    fs::rename(&old, &destination)
                } else {
                    Ok(())
                }
            }) {
                let preserved = backup.keep();
                bail!("Runtime activation validation failed ({error}); rollback failed ({restore}). Previous installation, if present, preserved at {}", preserved.join("runtime").display());
            }
            Err(error)
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn cached_artifact(bytes: &[u8]) -> Artifact {
        Artifact {
            name: "test".into(),
            version: "1".into(),
            url: "https://example.com/test.jar".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            kind: ArchiveKind::Jar,
        }
    }

    #[test]
    fn catalog_is_pinned_and_covers_supported_platforms() {
        let catalog = manifest().unwrap();
        assert_eq!(catalog.artifacts.len(), 12);
        assert_eq!(catalog.java.len(), 6);
        assert_eq!(catalog.node.len(), 6);
        assert!(catalog
            .artifacts
            .iter()
            .all(|a| a.url.starts_with("https://repo.maven.apache.org/maven2/")));
        for key in catalog.java.keys() {
            assert!(catalog.node.contains_key(key));
        }
    }

    #[test]
    fn offline_cache_requires_matching_digest() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"verified artifact";
        let artifact = cached_artifact(bytes);
        let client = reqwest::blocking::Client::new();
        assert!(download(&client, &artifact, dir.path(), true).is_err());
        let path = dir.path().join(format!("{}-test.jar", artifact.sha256));
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            download(&client, &artifact, dir.path(), true).unwrap(),
            path
        );
        fs::write(&path, "tampered").unwrap();
        assert!(download(&client, &artifact, dir.path(), true).is_err());
    }

    #[test]
    fn rejects_archive_and_receipt_paths_that_escape() {
        for path in [
            "../outside",
            "/absolute",
            "ok/../../outside",
            "C:\\outside",
            "..\\outside",
            "",
        ] {
            assert!(safe_relative(Path::new(path)).is_err(), "{path}");
        }
        assert!(safe_relative(Path::new("lib/backend.jar")).is_ok());
        assert!(safe_link(Path::new("jre/lib/link"), Path::new("../../outside"), false).is_ok());
        assert!(safe_link(
            Path::new("jre/lib/link"),
            Path::new("../../../outside"),
            false
        )
        .is_err());
        assert!(safe_link(Path::new("link"), Path::new("/outside"), false).is_err());
    }

    fn zip_file(path: &Path, name: &str) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"data").unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn zip_traversal_is_rejected_and_normal_files_extract() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive.zip");
        let target = dir.path().join("destination");
        fs::create_dir(&target).unwrap();
        zip_file(&archive, "../escape");
        assert!(unpack_zip(&archive, &target).is_err());
        assert!(!dir.path().join("escape").exists());
        zip_file(&archive, "lib/a.jar");
        unpack_zip(&archive, &target).unwrap();
        assert_eq!(fs::read(target.join("lib/a.jar")).unwrap(), b"data");
    }

    #[test]
    fn tar_preserves_executable_permissions_and_rejects_escaping_links() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive.tar.gz");
        let mut gzip = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        {
            let mut tar = tar::Builder::new(&mut gzip);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "bin/java", Cursor::new(b"data"))
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o777);
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("../../../escape").unwrap();
            header.set_cksum();
            tar.append_data(&mut header, "bin/escape", io::empty())
                .unwrap();
            tar.finish().unwrap();
        }
        gzip.finish().unwrap();
        let target = dir.path().join("destination");
        fs::create_dir(&target).unwrap();
        assert!(unpack_tar(&archive, &target).is_err());
        assert_eq!(fs::read(target.join("bin/java")).unwrap(), b"data");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(target.join("bin/java"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn incomplete_installation_is_not_discovered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(Some(dir.path())).unwrap().is_none());
        let release = release_dir(dir.path(), &manifest().unwrap()).unwrap();
        fs::create_dir_all(&release).unwrap();
        assert!(load(Some(dir.path()))
            .unwrap_err()
            .to_string()
            .contains("incomplete"));
    }
}
