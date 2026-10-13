//! Classify source files without excluding tests from file selection.
use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::path::Path;

const DEFAULT_TEST_PATTERNS: &[&str] = &[
    "**/*_test.go",
    "**/test_*.py",
    "**/*_test.py",
    "**/*.{test,spec}.{js,jsx,ts,tsx,mjs,cjs,mts,cts}",
    "**/{test,tests,_tests_,__tests__,e2e}/**",
];

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileClassificationConfig {
    pub use_defaults: bool,
    pub test_patterns: Vec<String>,
    pub main_patterns: Vec<String>,
}

impl Default for FileClassificationConfig {
    fn default() -> Self {
        Self {
            use_defaults: true,
            test_patterns: Vec::new(),
            main_patterns: Vec::new(),
        }
    }
}

pub struct FileClassifier {
    tests: GlobSet,
    main: GlobSet,
}

impl FileClassifier {
    pub fn new(config: &FileClassificationConfig) -> Result<Self> {
        let defaults = if config.use_defaults {
            DEFAULT_TEST_PATTERNS
        } else {
            &[]
        };
        Ok(Self {
            tests: patterns(
                defaults
                    .iter()
                    .copied()
                    .chain(config.test_patterns.iter().map(String::as_str)),
                "test_patterns",
            )?,
            main: patterns(
                config.main_patterns.iter().map(String::as_str),
                "main_patterns",
            )?,
        })
    }

    /// Paths are relative to the scan root. Explicit production patterns win.
    pub fn is_test(&self, relative_path: &Path) -> bool {
        !self.main.is_match(relative_path) && self.tests.is_match(relative_path)
    }
}

fn patterns<'a>(patterns: impl Iterator<Item = &'a str>, field: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .with_context(|| format!("Invalid file_classification.{field} glob {pattern:?}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .with_context(|| format!("Cannot compile file_classification.{field}"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn recognizes_root_and_nested_test_conventions_without_matching_similar_names() {
        let classifier = FileClassifier::new(&FileClassificationConfig::default()).unwrap();
        for path in [
            "server_test.go",
            "api/internal/app/auth_test.go",
            "test_receiver.py",
            "scripts/runtime_test.py",
            "ui/src/App.test.tsx",
            "ui/src/api.spec.ts",
            "ui/src/foo.test.mjs",
            "tests/helpers.py",
            "ui/tests/setup.ts",
            "ui/e2e/helpers.ts",
            "ui/src/__tests__/App.tsx",
            "_tests_/helpers.ts",
            "ui/src/_tests_/App.tsx",
            "_tests_/nested/helpers.go",
            "src/test/java/Example.java",
        ] {
            assert!(classifier.is_test(Path::new(path)), "{path}");
        }
        for path in [
            "api/internal/app/auth.go",
            "ui/src/App.tsx",
            "ui/src/contest.ts",
            "ui/src/testSession.ts",
            "latest/example.py",
            "test-data/sample.py",
            "test_receiver.py.bak",
            "_tests_data/helpers.ts",
            "ui/src/_tests_extra/App.tsx",
            "ui/src/_tests_.ts",
        ] {
            assert!(!classifier.is_test(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn custom_patterns_extend_defaults_and_production_overrides_win() {
        let config = FileClassificationConfig {
            test_patterns: vec!["ui/src/testSession.ts".into(), "fixtures/*.py".into()],
            main_patterns: vec![
                "tests/production/**".into(),
                "**/server_test.go".into(),
                "**/_tests_/production/**".into(),
            ],
            ..Default::default()
        };
        let classifier = FileClassifier::new(&config).unwrap();
        for path in [
            "ui/src/testSession.ts",
            "fixtures/data.py",
            "app_test.go",
            "_tests_/helpers.ts",
        ] {
            assert!(classifier.is_test(Path::new(path)), "{path}");
        }
        for path in [
            "tests/production/server.py",
            "api/server_test.go",
            "fixtures/nested/data.py",
            "_tests_/production/helpers.ts",
            "ui/_tests_/production/helpers.ts",
        ] {
            assert!(!classifier.is_test(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn defaults_can_be_disabled_and_invalid_patterns_are_errors() {
        let mut config = FileClassificationConfig {
            use_defaults: false,
            test_patterns: vec!["checks/**".into()],
            ..Default::default()
        };
        let classifier = FileClassifier::new(&config).unwrap();
        assert!(!classifier.is_test(Path::new("tests/test_main.py")));
        assert!(!classifier.is_test(Path::new("_tests_/helpers.ts")));
        assert!(classifier.is_test(Path::new("checks/main.py")));
        config.main_patterns.push("[".into());
        let error = FileClassifier::new(&config).err().unwrap();
        assert!(error.to_string().contains("main_patterns"));
    }

    #[cfg(unix)]
    #[test]
    fn a_literal_backslash_does_not_create_a_test_directory() {
        let classifier = FileClassifier::new(&FileClassificationConfig::default()).unwrap();
        assert!(!classifier.is_test(Path::new(r"tests\production.py")));
    }
}
