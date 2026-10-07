//! Select existing files for analysis; all modes analyze working-tree contents.

use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone)]
pub enum Selection {
    /// Tracked changes relative to HEAD, plus non-ignored untracked files.
    WorkingTree,
    /// Files changed in the index. Their current on-disk contents are analyzed.
    Staged,
    /// Changes from the merge base with this revision, plus untracked files.
    Base(String),
    /// All files under the project root, honoring ignore files.
    All,
    /// Explicit paths, relative to the project root unless absolute.
    Files(Vec<PathBuf>),
}

#[derive(Debug)]
pub struct SelectedFiles {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

pub fn select(root: &Path, mode: &Selection) -> Result<SelectedFiles> {
    let root = root
        .canonicalize()
        .with_context(|| format!("Cannot resolve project root {}", root.display()))?;
    if !root.is_dir() {
        bail!("Project root is not a directory: {}", root.display());
    }
    let mut files = BTreeSet::new();
    match mode {
        Selection::Files(paths) => {
            for path in paths {
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(path)
                };
                let canonical = path
                    .canonicalize()
                    .with_context(|| format!("Cannot resolve input file {}", path.display()))?;
                if !canonical.starts_with(&root) {
                    bail!("Input file is outside the project root: {}", path.display());
                }
                if !canonical.is_file() {
                    bail!("Input path is not a file: {}", path.display());
                }
                files.insert(canonical);
            }
        }
        Selection::All => {
            let walker = WalkBuilder::new(&root)
                .hidden(false)
                .require_git(false)
                .follow_links(false)
                .filter_entry(|entry| {
                    entry.file_name() != ".git" && !entry.path().ends_with(".rustmarine/runs")
                })
                .build();
            for entry in walker {
                let entry = entry.context("Cannot enumerate project files")?;
                include_existing(&root, entry.path(), &mut files)?;
            }
        }
        Selection::WorkingTree | Selection::Staged | Selection::Base(_) => {
            let inside = git(&root, &["rev-parse", "--is-inside-work-tree"])?;
            if inside != b"true\n" {
                bail!("Project root is not inside a Git working tree");
            }
            let mut args = vec![
                "diff",
                "--relative",
                "--name-only",
                "-z",
                "--no-ext-diff",
                "--no-renames",
                "--diff-filter=ACMRTUXB",
            ];
            let base;
            let paths = match mode {
                Selection::Staged => {
                    args.extend(["--cached", "--", "."]);
                    git(&root, &args)?
                }
                Selection::WorkingTree => {
                    if git_output(&root, &["rev-parse", "--verify", "HEAD"])?
                        .status
                        .success()
                    {
                        args.extend(["HEAD", "--", "."]);
                        git(&root, &args)?
                    } else {
                        // Before the first commit every indexed file is new.
                        git(&root, &["ls-files", "--cached", "-z", "--", "."])?
                    }
                }
                Selection::Base(revision) => {
                    let commit = git(
                        &root,
                        &[
                            "rev-parse",
                            "--verify",
                            "--end-of-options",
                            &format!("{revision}^{{commit}}"),
                        ],
                    )?;
                    let commit = std::str::from_utf8(&commit)?.trim();
                    let merge_base = git(&root, &["merge-base", "HEAD", commit])?;
                    base = std::str::from_utf8(&merge_base)?.trim().to_owned();
                    args.extend([base.as_str(), "--", "."]);
                    git(&root, &args)?
                }
                _ => unreachable!(),
            };
            include_git_paths(&root, &paths, &mut files)?;
            if !matches!(mode, Selection::Staged) {
                let untracked = git(
                    &root,
                    &[
                        "ls-files",
                        "--others",
                        "--exclude-standard",
                        "-z",
                        "--",
                        ".",
                    ],
                )?;
                include_git_paths(&root, &untracked, &mut files)?;
            }
        }
    }
    Ok(SelectedFiles {
        root,
        files: files.into_iter().collect(),
    })
}

fn include_git_paths(root: &Path, paths: &[u8], files: &mut BTreeSet<PathBuf>) -> Result<()> {
    for bytes in paths
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
    {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(OsString::from_vec(bytes.to_vec()))
        };
        #[cfg(not(unix))]
        let path = PathBuf::from(OsString::from(std::str::from_utf8(bytes).context(
            "Git returned a filename that cannot be represented on this platform",
        )?));
        include_existing(root, &root.join(path), files)?;
    }
    Ok(())
}

fn include_existing(root: &Path, path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    if path
        .strip_prefix(root)
        .is_ok_and(|path| path.starts_with(".rustmarine/runs"))
    {
        return Ok(());
    }
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot resolve {}", path.display()))
        }
    };
    // Deleted files and symlinks that escape the project cannot be analyzed.
    if canonical.starts_with(root) && canonical.is_file() {
        files.insert(canonical);
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .context("Cannot run Git; install Git or select --all / explicit files")
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_output(root, args)?;
    if !output.status.success() {
        bail!(
            "Git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn repository() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]).unwrap();
        git(dir.path(), &["config", "user.email", "test@example.com"]).unwrap();
        git(dir.path(), &["config", "user.name", "Test"]).unwrap();
        git(dir.path(), &["config", "commit.gpgsign", "false"]).unwrap();
        git(dir.path(), &["config", "core.hooksPath", ".git/no-hooks"]).unwrap();
        dir
    }

    fn commit(root: &Path) {
        git(root, &["add", "."]).unwrap();
        git(root, &["commit", "-qm", "test"]).unwrap();
    }

    fn names(selected: SelectedFiles) -> Vec<PathBuf> {
        selected
            .files
            .iter()
            .map(|path| path.strip_prefix(&selected.root).unwrap().to_owned())
            .collect()
    }

    #[test]
    fn working_tree_includes_staged_unstaged_and_untracked_but_not_deleted_or_ignored() {
        let dir = repository();
        let root = dir.path();
        for file in ["staged.rs", "unstaged.rs", "deleted.rs", "unchanged.rs"] {
            fs::write(root.join(file), "before").unwrap();
        }
        fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        commit(root);
        fs::write(root.join("staged.rs"), "after").unwrap();
        git(root, &["add", "staged.rs"]).unwrap();
        fs::write(root.join("unstaged.rs"), "after").unwrap();
        fs::remove_file(root.join("deleted.rs")).unwrap();
        fs::write(root.join("new file\nwith newline.rs"), "new").unwrap();
        fs::write(root.join("ignored.rs"), "ignored").unwrap();
        assert_eq!(
            names(select(root, &Selection::WorkingTree).unwrap()),
            vec![
                PathBuf::from("new file\nwith newline.rs"),
                PathBuf::from("staged.rs"),
                PathBuf::from("unstaged.rs")
            ]
        );
        assert_eq!(
            names(select(root, &Selection::Staged).unwrap()),
            vec![PathBuf::from("staged.rs")]
        );
    }

    #[test]
    fn initial_repository_includes_indexed_and_untracked_files() {
        let dir = repository();
        fs::write(dir.path().join("indexed.rs"), "indexed").unwrap();
        fs::write(dir.path().join("untracked.rs"), "untracked").unwrap();
        git(dir.path(), &["add", "indexed.rs"]).unwrap();
        assert_eq!(
            names(select(dir.path(), &Selection::WorkingTree).unwrap()),
            vec![PathBuf::from("indexed.rs"), PathBuf::from("untracked.rs")]
        );
        assert_eq!(
            names(select(dir.path(), &Selection::Staged).unwrap()),
            vec![PathBuf::from("indexed.rs")]
        );
    }

    #[test]
    fn base_uses_merge_base_and_includes_working_tree_changes() {
        let dir = repository();
        let root = dir.path();
        fs::write(root.join("shared.rs"), "initial").unwrap();
        commit(root);
        git(root, &["branch", "comparison"]).unwrap();
        git(root, &["checkout", "-qb", "feature"]).unwrap();
        fs::write(root.join("branch.rs"), "branch change").unwrap();
        commit(root);
        git(root, &["checkout", "-q", "comparison"]).unwrap();
        fs::write(root.join("comparison.rs"), "other branch change").unwrap();
        commit(root);
        git(root, &["checkout", "-q", "feature"]).unwrap();
        fs::write(root.join("shared.rs"), "working change").unwrap();
        fs::write(root.join("new.rs"), "new").unwrap();
        assert_eq!(
            names(select(root, &Selection::Base("comparison".into())).unwrap()),
            vec![
                PathBuf::from("branch.rs"),
                PathBuf::from("new.rs"),
                PathBuf::from("shared.rs")
            ]
        );
    }

    #[test]
    fn nested_project_root_is_scoped_and_paths_are_relative_to_it() {
        let dir = repository();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("outer.rs"), "initial").unwrap();
        fs::write(dir.path().join("nested/inner.rs"), "initial").unwrap();
        commit(dir.path());
        fs::write(dir.path().join("outer.rs"), "changed").unwrap();
        fs::write(dir.path().join("nested/inner.rs"), "changed").unwrap();
        fs::write(dir.path().join("nested/new.rs"), "new").unwrap();
        assert_eq!(
            names(select(&dir.path().join("nested"), &Selection::WorkingTree).unwrap()),
            vec![PathBuf::from("inner.rs"), PathBuf::from("new.rs")]
        );
    }

    #[test]
    fn renamed_files_select_the_destination_only() {
        let dir = repository();
        fs::write(dir.path().join("old.rs"), "source").unwrap();
        commit(dir.path());
        git(dir.path(), &["mv", "old.rs", "new.rs"]).unwrap();
        assert_eq!(
            names(select(dir.path(), &Selection::WorkingTree).unwrap()),
            vec![PathBuf::from("new.rs")]
        );
        assert_eq!(
            names(select(dir.path(), &Selection::Staged).unwrap()),
            vec![PathBuf::from("new.rs")]
        );
    }

    // macOS filesystems reject these names even though OsString supports bytes.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn git_filenames_need_not_be_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let dir = repository();
        let path = PathBuf::from(OsString::from_vec(b"source-\xff.rs".to_vec()));
        fs::write(dir.path().join(&path), "source").unwrap();
        assert_eq!(
            names(select(dir.path(), &Selection::WorkingTree).unwrap()),
            vec![path]
        );
    }

    #[test]
    fn all_respects_gitignore_and_includes_hidden_source_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(dir.path().join("ignored.rs"), "ignored").unwrap();
        fs::write(dir.path().join(".hidden.rs"), "source").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "internal").unwrap();
        assert_eq!(
            names(select(dir.path(), &Selection::All).unwrap()),
            vec![PathBuf::from(".gitignore"), PathBuf::from(".hidden.rs")]
        );
    }

    #[test]
    fn explicit_files_are_deduplicated_and_missing_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "source").unwrap();
        assert_eq!(
            names(
                select(
                    dir.path(),
                    &Selection::Files(vec!["a.rs".into(), "./a.rs".into()])
                )
                .unwrap()
            ),
            vec![PathBuf::from("a.rs")]
        );
        assert!(select(dir.path(), &Selection::Files(vec!["missing.rs".into()])).is_err());
        assert!(select(dir.path(), &Selection::Files(vec![".".into()])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_outside_root_are_skipped_or_rejected_for_explicit_files() {
        use std::os::unix::fs::symlink;
        let dir = repository();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "outside").unwrap();
        symlink(outside.path().join("secret.rs"), dir.path().join("link.rs")).unwrap();
        assert!(select(dir.path(), &Selection::WorkingTree)
            .unwrap()
            .files
            .is_empty());
        assert!(select(dir.path(), &Selection::All)
            .unwrap()
            .files
            .is_empty());
        assert!(select(dir.path(), &Selection::Files(vec!["link.rs".into()])).is_err());
    }
}
