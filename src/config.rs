use crate::classification::FileClassificationConfig;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub file_classification: FileClassificationConfig,
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
    fn path(root: &Path, explicit: Option<&Path>) -> PathBuf {
        explicit
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join(".rustmarine/config.toml"))
    }

    pub fn load(root: &Path, explicit: Option<&Path>) -> Result<Self> {
        let path = Self::path(root, explicit);
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

pub fn parse_rule_key(rule: &str) -> std::result::Result<String, String> {
    let valid = rule.split_once(':').is_some_and(|(repository, key)| {
        [repository, key].iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte))
        })
    });
    if valid {
        Ok(rule.to_owned())
    } else {
        Err("Expected a rule key such as typescript:S1135 (repository:rule)".into())
    }
}

/// Disable one rule while preserving the rest of the TOML document.
/// The returned boolean reports whether the configuration was changed.
pub fn ignore_rule(root: &Path, explicit: Option<&Path>, rule: &str) -> Result<(PathBuf, bool)> {
    parse_rule_key(rule).map_err(anyhow::Error::msg)?;
    let root = root.canonicalize().context("Cannot find project root")?;
    if !root.is_dir() {
        bail!("Project root is not a directory: {}", root.display());
    }
    let path = Config::path(&root, explicit);
    let mut path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let original = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && fs::symlink_metadata(&path).is_err() =>
        {
            String::new()
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Cannot read configuration {}", path.display()))
        }
    };
    let config: Config = toml::from_str(&original)
        .with_context(|| format!("Invalid configuration {}", path.display()))?;
    let permissions = if path.exists() {
        // Follow existing symlinks, updating their target rather than replacing the link.
        path = path.canonicalize()?;
        Some(fs::metadata(&path)?.permissions())
    } else {
        None
    };
    if config.rules.get(rule).is_some_and(|rule| !rule.active) {
        return Ok((path, false));
    }

    let mut document: toml_edit::DocumentMut = original.parse()?;
    let rules = document.as_table_mut().entry("rules").or_insert_with(|| {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        toml_edit::Item::Table(table)
    });
    let new_rule = if rules.is_inline_table() {
        toml_edit::value(toml_edit::InlineTable::new())
    } else {
        toml_edit::Item::Table(toml_edit::Table::new())
    };
    let rule_config = rules
        .as_table_like_mut()
        .context("Rules must be a table")?
        .entry(rule)
        .or_insert(new_rule);
    let active = rule_config
        .as_table_like_mut()
        .context("Rule configuration must be a table")?
        .entry("active")
        .or_insert(toml_edit::value(false));
    let decor = active.as_value().map(|value| value.decor().clone());
    *active = toml_edit::value(false);
    if let Some(decor) = decor {
        *active
            .as_value_mut()
            .context("Active must be a value")?
            .decor_mut() = decor;
    }
    let updated = document.to_string();
    // Validate the edited document before creating directories or replacing any file.
    toml::from_str::<Config>(&updated).context("Cannot serialize updated configuration")?;
    let parent = path.parent().context("Configuration has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Cannot create configuration directory {}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(updated.as_bytes())?;
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .with_context(|| format!("Cannot save configuration {}", path.display()))?;
    Ok((path.canonicalize()?, true))
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
        bail!("No Sonar analyzer JARs configured. Set --analyzer-dir, --plugin, or plugins in .rustmarine/config.toml (see rust-cli/README.md).");
    }
    Ok(result)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn file_classification_defaults_and_overrides_deserialize_consistently() {
        assert!(Config::default().file_classification.use_defaults);
        let empty: Config = toml::from_str("").unwrap();
        assert!(empty.file_classification.use_defaults);
        let config: Config = toml::from_str(
            "[file_classification]\ntest_patterns = ['checks/**']\nmain_patterns = ['tests/production/**']\n",
        ).unwrap();
        assert!(config.file_classification.use_defaults);
        assert_eq!(config.file_classification.test_patterns, ["checks/**"]);
        let config: Config =
            toml::from_str("[file_classification]\nuse_defaults = false\n").unwrap();
        assert!(!config.file_classification.use_defaults);
        assert!(toml::from_str::<Config>("[file_classification]\ntest_paterns = []\n").is_err());
    }
    #[test]
    fn strict_config_and_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".rustmarine")).unwrap();
        let path = temp.path().join(".rustmarine/config.toml");
        fs::write(&path, "backend_dir = 'runtime/sloop'\nplugins = ['runtime/sonar.jar']\n[rules.\"java:S100\"]\nactive = false\n").unwrap();
        let config = Config::load(temp.path(), None).unwrap();
        assert_eq!(
            config.backend_dir.unwrap(),
            temp.path()
                .canonicalize()
                .unwrap()
                .join(".rustmarine/runtime/sloop")
        );
        assert!(!config.rules["java:S100"].active);
        fs::write(&path, "typo = true").unwrap();
        assert!(Config::load(temp.path(), None).is_err());
    }
}
