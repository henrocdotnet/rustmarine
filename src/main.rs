mod config;
mod engine;
mod output;
mod rpc;
mod runtime;
mod selection;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use config::Config;
use output::{OutputFormat, ScanReport};
use selection::{SelectedFiles, Selection};
use std::{
    collections::BTreeMap,
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "rustmarine",
    version,
    about = "Scan local source changes with Sonar analyzers",
    long_about = "Scan local source changes with Sonar analyzers. Run init once to provision the analysis environment. Findings exit 1; operational errors exit 2. Source analysis runs locally."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Provision and verify an independent SLOOP, analyzer, Java, and Node environment
    Init(InitArgs),
    /// Analyze selected files (defaults to staged, unstaged, and untracked changes)
    Scan(ScanArgs),
    /// List the files a scan would analyze, without starting the engine
    Files(FileArgs),
    /// Check runtime paths and Java availability
    Doctor(DoctorArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Managed runtime storage directory (default: user application data/rustmarine)
    #[arg(long, env = "RUSTMARINE_RUNTIME_DIR")]
    runtime_dir: Option<PathBuf>,
    /// Install using verified cached downloads only; never access the network
    #[arg(long)]
    offline: bool,
    /// Rebuild the installation from verified downloads and rerun analysis checks
    #[arg(long)]
    repair: bool,
}

#[derive(Args)]
struct FileArgs {
    /// Project directory; defaults to current directory
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Analyze every non-ignored project file
    #[arg(long, conflicts_with_all = ["staged", "base", "files"])]
    all: bool,
    /// Select staged files; analyzes their current working-tree contents
    #[arg(long, conflicts_with_all = ["all", "base", "files"])]
    staged: bool,
    /// Select changes since the merge base with this Git revision, plus untracked files
    #[arg(long, conflicts_with_all = ["all", "staged", "files"])]
    base: Option<String>,
    /// Explicit files, relative to --root (use -- before names beginning with '-')
    files: Vec<PathBuf>,
}

impl FileArgs {
    fn select(&self) -> Result<SelectedFiles> {
        let mode = if self.all {
            Selection::All
        } else if self.staged {
            Selection::Staged
        } else if let Some(base) = &self.base {
            Selection::Base(base.clone())
        } else if !self.files.is_empty() {
            Selection::Files(self.files.clone())
        } else {
            Selection::WorkingTree
        };
        selection::select(&self.root, &mode)
    }
}

#[derive(Args)]
struct RuntimeArgs {
    /// Managed runtime directory created by init
    #[arg(long, env = "RUSTMARINE_RUNTIME_DIR")]
    runtime_dir: Option<PathBuf>,
    /// Configuration file (default: <root>/.rustmarine.toml)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Extracted SLOOP directory containing lib/
    #[arg(long, env = "RUSTMARINE_BACKEND_DIR")]
    backend_dir: Option<PathBuf>,
    /// Directory containing Sonar analyzer JARs
    #[arg(long, env = "RUSTMARINE_ANALYZER_DIR")]
    analyzer_dir: Option<PathBuf>,
    /// Individual analyzer JAR; repeat to load multiple analyzers
    #[arg(long)]
    plugin: Vec<PathBuf>,
    /// Java executable; defaults to managed Java, then JAVA_HOME/PATH
    #[arg(long)]
    java: Option<PathBuf>,
    /// Node.js executable for JavaScript/TypeScript analysis
    #[arg(long)]
    node: Option<PathBuf>,
}

#[derive(Args)]
struct ScanArgs {
    #[command(flatten)]
    selection: FileArgs,
    #[command(flatten)]
    runtime: RuntimeArgs,
    /// Output format; JSON is the default for agent consumption
    #[arg(long, value_enum, default_value = "json")]
    format: OutputFormat,
    /// Analysis deadline in seconds
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=86400))]
    timeout: u64,
    /// Analysis property KEY=VALUE; repeat to set several properties
    #[arg(long = "property", value_parser = parse_property)]
    properties: Vec<(String, String)>,
    /// Finding threshold used for exit status (does not hide findings)
    #[arg(long, value_enum, default_value = "any")]
    fail_on: FailOn,
    /// Print engine diagnostics to stderr
    #[arg(long)]
    verbose: bool,
}

#[derive(Args)]
struct DoctorArgs {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(flatten)]
    runtime: RuntimeArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum FailOn {
    Any,
    High,
    None,
}

fn parse_property(value: &str) -> std::result::Result<(String, String), String> {
    let (key, value) = value.split_once('=').ok_or("Expected KEY=VALUE")?;
    if key.is_empty() {
        return Err("Property key cannot be empty".into());
    }
    Ok((key.into(), value.into()))
}

fn executable(path: PathBuf) -> PathBuf {
    if path.components().count() == 1 && path.is_relative() {
        if let Some(search) = env::var_os("PATH") {
            for dir in env::split_paths(&search) {
                let candidate = dir.join(&path);
                if candidate.is_file() {
                    return candidate;
                }
                #[cfg(windows)]
                {
                    let candidate = candidate.with_extension("exe");
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
        }
    }
    path
}

fn runtime_options(
    args: &RuntimeArgs,
    config: Config,
    timeout: u64,
    verbose: bool,
    properties: &[(String, String)],
) -> Result<engine::EngineOptions> {
    let external_backend = args.backend_dir.clone().or(config.backend_dir);
    let managed = if external_backend.is_none() {
        runtime::load(
            args.runtime_dir
                .as_deref()
                .or(config.runtime_dir.as_deref()),
        )?
    } else {
        None
    };
    let backend_dir = external_backend.or_else(|| managed.as_ref().map(|r| r.backend_dir.clone()))
        .context("SLOOP runtime is not configured. Run rustmarine init, or set --backend-dir for an external runtime. See rust-cli/README.md.")?
        .canonicalize().context("Cannot find SLOOP backend directory")?;
    if !backend_dir.join("lib").is_dir() {
        bail!(
            "SLOOP backend directory must contain lib/: {}",
            backend_dir.display()
        );
    }
    let directory = args
        .analyzer_dir
        .as_deref()
        .or(config.analyzer_dir.as_deref())
        .or_else(|| managed.as_ref().map(|r| r.analyzer_dir.as_path()));
    let mut configured_plugins = config.plugins;
    configured_plugins.extend(args.plugin.iter().cloned());
    let plugins = config::plugins(directory, &configured_plugins)?;
    let java = executable(
        args.java
            .clone()
            .or(config.java)
            .or_else(|| managed.as_ref().map(|r| r.java.clone()))
            .or_else(|| {
                env::var_os("JAVA_HOME").map(|home| {
                    PathBuf::from(home).join("bin").join(if cfg!(windows) {
                        "java.exe"
                    } else {
                        "java"
                    })
                })
            })
            .unwrap_or_else(|| PathBuf::from("java")),
    );
    let node = args
        .node
        .clone()
        .or(config.node)
        .or_else(|| managed.as_ref().map(|r| r.node.clone()))
        .map(executable);
    let mut analysis_properties = config.properties;
    analysis_properties.extend(properties.iter().cloned());
    let rules = config
        .rules
        .into_iter()
        .map(|(key, value)| {
            (
                key,
                serde_json::json!({"isActive":value.active,"paramValueByKey":value.parameters}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    Ok(engine::EngineOptions {
        backend_dir,
        plugins,
        java,
        node,
        timeout: Duration::from_secs(timeout),
        properties: analysis_properties,
        rules,
        verbose,
    })
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn report_file(root: &Path, uri: String) -> String {
    url::Url::parse(&uri)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .map(|path| relative(root, &path))
        .unwrap_or(uri)
}

fn validate_report_paths(selected: &SelectedFiles) -> Result<()> {
    selected
        .root
        .to_str()
        .context("Project root cannot be represented as UTF-8 in the report")?;
    for file in &selected.files {
        file.to_str()
            .context("Selected filename cannot be represented as UTF-8 in the report")?;
    }
    Ok(())
}

fn write_stdout(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(text.as_bytes()).and_then(|_| {
        if text.ends_with('\n') {
            Ok(())
        } else {
            stdout.write_all(b"\n")
        }
    }) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn java_major(version: &str) -> Option<u32> {
    let number = version.split('"').nth(1)?;
    let mut parts = number.split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    if major == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(major)
    }
}

fn scan(args: &ScanArgs) -> Result<u8> {
    let selected = args.selection.select()?;
    validate_report_paths(&selected)?;
    let config = Config::load(&selected.root, args.runtime.config.as_deref())?;
    let scanned_files = selected
        .files
        .iter()
        .map(|file| relative(&selected.root, file))
        .collect();
    let mut report = ScanReport {
        schema_version: 1,
        root: selected.root.to_string_lossy().into_owned(),
        scanned_files,
        findings: vec![],
        failed_files: vec![],
        skipped_files: BTreeMap::new(),
    };
    if !selected.files.is_empty() {
        let options = runtime_options(
            &args.runtime,
            config,
            args.timeout,
            args.verbose,
            &args.properties,
        )?;
        let result = engine::analyze(&options, &selected.root, &selected.files)?;
        report.findings = output::normalize(&selected.root, result.findings)?;
        report.failed_files = result
            .failed_files
            .into_iter()
            .map(|file| report_file(&selected.root, file))
            .collect();
        report.failed_files.sort();
        report.failed_files.dedup();
        report.skipped_files = result
            .skipped_files
            .into_iter()
            .map(|(file, reason)| (report_file(&selected.root, file), reason))
            .collect();
        report
            .scanned_files
            .retain(|file| !report.skipped_files.contains_key(file));
    }
    let failed = !report.failed_files.is_empty()
        || (report.scanned_files.is_empty() && !report.skipped_files.is_empty());
    let findings_failed = match args.fail_on {
        FailOn::Any => !report.findings.is_empty(),
        FailOn::None => false,
        FailOn::High => report.findings.iter().any(|f| {
            matches!(
                f.severity.to_uppercase().as_str(),
                "BLOCKER" | "CRITICAL" | "HIGH"
            ) || f
                .impacts
                .values()
                .any(|impact| matches!(impact.to_uppercase().as_str(), "HIGH" | "BLOCKER"))
        }),
    };
    write_stdout(&output::render(&report, args.format)?)?;
    Ok(if failed {
        2
    } else if findings_failed {
        1
    } else {
        0
    })
}

fn run(cli: &Cli) -> Result<u8> {
    match &cli.command {
        Commands::Init(args) => {
            let installation =
                runtime::init(args.runtime_dir.as_deref(), args.offline, args.repair)?;
            write_stdout(&serde_json::to_string_pretty(&installation)?)?;
            Ok(0)
        }
        Commands::Scan(args) => scan(args),
        Commands::Files(args) => {
            let selected = args.select()?;
            validate_report_paths(&selected)?;
            let files: Vec<_> = selected
                .files
                .iter()
                .map(|file| relative(&selected.root, file))
                .collect();
            write_stdout(&serde_json::to_string_pretty(
                &serde_json::json!({"schema_version":1,"root":selected.root,"files":files}),
            )?)?;
            Ok(0)
        }
        Commands::Doctor(args) => {
            let root = args
                .root
                .canonicalize()
                .context("Cannot find project root")?;
            let config = Config::load(&root, args.runtime.config.as_deref())?;
            let options = runtime_options(&args.runtime, config, 30, false, &[])?;
            let java = Command::new(&options.java)
                .arg("-version")
                .output()
                .context("Cannot run Java. Install Java 21+ or set --java/JAVA_HOME.")?;
            if !java.status.success() {
                bail!("Java version check failed");
            }
            let version = format!(
                "{}{}",
                String::from_utf8_lossy(&java.stdout),
                String::from_utf8_lossy(&java.stderr)
            );
            let major = java_major(&version).context("Cannot determine Java version")?;
            if major < 21 {
                bail!("Java 21+ is required; detected Java {major}");
            }
            write_stdout(&serde_json::to_string_pretty(
                &serde_json::json!({"backend_dir":options.backend_dir,"plugins":options.plugins,"java":options.java,"java_version":version.trim(),"note":"Path checks passed; scan verifies engine compatibility and analyzer readiness."}),
            )?)?;
            Ok(0)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if matches!(&cli.command, Commands::Scan(args) if matches!(args.format, OutputFormat::Json))
            {
                let _ = write_stdout(&serde_json::json!({"schema_version":1,"error":{"message":format!("{error:#}")}}).to_string());
            } else {
                eprintln!("error: {error:#}");
            }
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn understands_java_versions() {
        assert_eq!(
            java_major("openjdk version \"21.0.5\" 2024-10-15"),
            Some(21)
        );
        assert_eq!(java_major("java version \"1.8.0_422\""), Some(8));
        assert_eq!(java_major("openjdk version \"25-ea\""), Some(25));
        assert_eq!(java_major("unrecognized"), None);
    }
}
