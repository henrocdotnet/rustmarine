//! Project-local scan reports and diagnostics, independent of terminal verbosity.
use crate::output::OutputFormat;
use anyhow::{bail, Context, Result};
use chrono::Local;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct RunLog(Arc<Mutex<LogState>>);

struct LogState {
    file: File,
    error: Option<String>,
}

impl RunLog {
    pub fn line(&self, message: &str) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Scan log lock poisoned"))?;
        if let Err(error) = writeln!(state.file, "{message}") {
            state.error = Some(error.to_string());
            return Err(error).context("Cannot write scan log");
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Scan log lock poisoned"))?;
        if let Some(error) = &state.error {
            bail!("Cannot write scan log: {error}");
        }
        state.file.flush().context("Cannot flush scan log")
    }
}

pub struct RunArtifacts {
    pub dir: PathBuf,
    pub log: RunLog,
}

impl RunArtifacts {
    pub fn create(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("Cannot resolve project root for saved scan")?;
        let parent = root.join(".rustmarine/runs");
        fs::create_dir_all(&parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
        let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
        let mut suffix = 0_u64;
        let dir = loop {
            let name = if suffix == 0 {
                stamp.clone()
            } else {
                format!("{stamp}_{suffix}")
            };
            let dir = parent.join(name);
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => suffix += 1,
                Err(error) => {
                    return Err(error).with_context(|| format!("Cannot create {}", dir.display()))
                }
            }
        };
        let file = File::create(dir.join("scan.log")).context("Cannot create scan log")?;
        Ok(Self {
            dir,
            log: RunLog(Arc::new(Mutex::new(LogState { file, error: None }))),
        })
    }

    pub fn save_report(&self, format: OutputFormat, content: &str) -> Result<()> {
        let name = match format {
            OutputFormat::Json => "report.json",
            OutputFormat::Text => "report.txt",
            OutputFormat::Sarif => "report.sarif",
        };
        let path = self.dir.join(name);
        // Publish only a complete report, including on platforms without overwrite rename.
        let mut temporary = tempfile::NamedTempFile::new_in(&self.dir)?;
        temporary.write_all(content.as_bytes())?;
        if !content.ends_with('\n') {
            temporary.write_all(b"\n")?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .with_context(|| format!("Cannot save {}", path.display()))?;
        Ok(())
    }

    pub fn save_error(&self, error: &anyhow::Error) -> Result<()> {
        self.log.line(&format!("error: {error:#}"))?;
        self.save_report(
            OutputFormat::Json,
            &serde_json::json!({
                "schema_version": 1, "error": {"message": format!("{error:#}")}
            })
            .to_string(),
        )?;
        self.log.finish()
    }
}
