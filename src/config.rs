use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub runtime_dir: Option<PathBuf>,
    pub backend_dir: Option<PathBuf>,
    pub analyzer_dir: Option<PathBuf>,
    #[serde(default)]
    pub plugins: Vec<PathBuf>,
    pub java: Option<PathBuf>,
    pub node: Option<PathBuf>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    #[serde(default)]
    pub rules: BTreeMap<String, RuleConfig>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    pub active: bool,
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
}

impl Config {
    pub fn load(root: &Path, explicit: Option<&Path>) -> Result<Self> {
        let path = explicit
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join(".rustmarine.toml"));
        if !path.exists() && explicit.is_none() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("Cannot read configuration {}", path.display()))?;
        let mut config: Self = toml::from_str(&text)
            .with_context(|| format!("Invalid configuration {}", path.display()))?;
        let base = path
            .canonicalize()?
            .parent()
            .context("Configuration has no parent")?
            .to_path_buf();
        for path in [
            &mut config.runtime_dir,
            &mut config.backend_dir,
            &mut config.analyzer_dir,
        ]
        .into_iter()
        .flatten()
        {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        for path in &mut config.plugins {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        // Bare executable names use PATH; other relative Java paths use the config directory.
        for executable in [&mut config.java, &mut config.node].into_iter().flatten() {
            if executable.is_relative() && executable.components().count() > 1 {
                *executable = base.join(&*executable);
            }
        }
        Ok(config)
    }
}

pub fn plugins(directory: Option<&Path>, explicit: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut result = explicit.to_vec();
    if let Some(directory) = directory {
        for entry in fs::read_dir(directory)
            .with_context(|| format!("Cannot read analyzer directory {}", directory.display()))?
        {
            let path = entry?.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "jar") {
                result.push(path);
            }
        }
    }
    for path in &mut result {
        *path = path
            .canonicalize()
            .with_context(|| format!("Analyzer does not exist: {}", path.display()))?;
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "jar") {
            bail!("Analyzer must be a JAR file: {}", path.display());
        }
    }
    result.sort();
    result.dedup();
    if result.is_empty() {
        bail!("No Sonar analyzer JARs configured. Set --analyzer-dir, --plugin, or plugins in .rustmarine.toml (see rust-cli/README.md).");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_config_and_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".rustmarine.toml"), "backend_dir = 'runtime/sloop'\nplugins = ['runtime/sonar.jar']\n[rules.\"java:S100\"]\nactive = false\n").unwrap();
        let config = Config::load(temp.path(), None).unwrap();
        assert_eq!(
            config.backend_dir.unwrap(),
            temp.path().canonicalize().unwrap().join("runtime/sloop")
        );
        assert!(!config.rules["java:S100"].active);
        fs::write(temp.path().join(".rustmarine.toml"), "typo = true").unwrap();
        assert!(Config::load(temp.path(), None).is_err());
    }
}
