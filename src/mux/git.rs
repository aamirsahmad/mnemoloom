//! Repository and branch discovery by reading `.git` directly. git itself is
//! run only for reftable repositories, whose HEAD file is a placeholder.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::exec::{self, FAST};
use super::paths::resolve;

/// Reads `(repo, branch)` for pane directories, memoising the rare
/// `git` fork per worktree for the duration of one scan.
#[derive(Default)]
pub struct GitReader {
    symrefs: HashMap<PathBuf, String>,
}

/// Placeholder HEAD of a reftable repository; the real HEAD is in the tables.
const REFTABLE_PLACEHOLDER: &str = ".invalid";

impl GitReader {
    /// `(repo, branch)` for a directory: the enclosing worktree's basename and
    /// its checked-out branch (`""` when detached or not a repository).
    pub fn context(&mut self, cwd: &str) -> (String, String) {
        if cwd.is_empty() {
            return Default::default();
        }
        for dir in Path::new(cwd).ancestors().filter(|p| p.parent().is_some()) {
            let Some(head) = head_file(dir) else {
                continue;
            };
            if !head.exists() {
                continue;
            }
            let name = file_name(dir);
            let Ok(text) = fs::read_to_string(&head) else {
                return (name, String::new());
            };
            let branch = match branch_from_head(&text) {
                REFTABLE_PLACEHOLDER => self.symbolic_head(dir),
                branch => branch.to_owned(),
            };
            return (name, branch);
        }
        Default::default()
    }

    /// Ask git itself; detached (or any failure) is `""`.
    fn symbolic_head(&mut self, worktree: &Path) -> String {
        self.symrefs
            .entry(worktree.to_path_buf())
            .or_insert_with(|| {
                let dir = worktree.to_string_lossy();
                let args = ["-C", &dir, "symbolic-ref", "--short", "-q", "HEAD"];
                exec::run("git", &args, &[], FAST)
                    .map(|out| out.trim().to_owned())
                    .unwrap_or_default()
            })
            .clone()
    }
}

/// HEAD for a worktree root candidate: `.git/HEAD`, or via a `gitdir:` pointer
/// (relative to the directory holding `.git`).
fn head_file(dir: &Path) -> Option<PathBuf> {
    let git = dir.join(".git");
    if git.is_dir() {
        return Some(git.join("HEAD"));
    }
    if !git.is_file() {
        return None;
    }
    let text = fs::read_to_string(&git).ok()?;
    let target = text.trim();
    let target = target.strip_prefix("gitdir: ").unwrap_or(target);
    Some(resolve(&dir.join(target)).join("HEAD"))
}

/// Branch named by a HEAD file; `""` when detached.
fn branch_from_head(text: &str) -> &str {
    let text = text.trim();
    if !text.contains("ref:") {
        return "";
    }
    text.strip_prefix("ref: refs/heads/").unwrap_or(text)
}

/// The shared repository's name, including for linked worktrees (used by `--repo`).
///
/// In the bare-clone layout `monorepo/.bare` + sibling worktrees the common
/// dir is `.bare`; like `.git`, a hidden common dir names its parent.
pub fn repository_name(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    let path = resolve(Path::new(cwd));
    for root in path.ancestors() {
        let marker = root.join(".git");
        if marker.is_dir() {
            return file_name(root);
        }
        if !marker.is_file() {
            continue;
        }
        let Ok(pointer) = fs::read_to_string(&marker) else {
            return String::new();
        };
        let Some(target) = pointer.trim().strip_prefix("gitdir: ") else {
            return String::new();
        };
        let git_dir = resolve(&root.join(target));
        let common = match fs::read_to_string(git_dir.join("commondir")) {
            Ok(rel) => resolve(&git_dir.join(rel.trim())),
            Err(_) => return file_name(root),
        };
        return if is_hidden(&common) {
            common.parent().map(file_name).unwrap_or_default()
        } else {
            file_name(&common)
        };
    }
    String::new()
}

fn is_hidden(path: &Path) -> bool {
    file_name(path).starts_with('.')
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn git_context(cwd: &str) -> (String, String) {
        GitReader::default().context(cwd)
    }

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    #[test]
    fn plain_repo_and_subdir() {
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("proj");
        write(&repo.join(".git/HEAD"), "ref: refs/heads/feat/x\n");
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        let expect = ("proj".to_owned(), "feat/x".to_owned());
        assert_eq!(git_context(&s(&repo)), expect);
        assert_eq!(git_context(&s(&repo.join("src/deep"))), expect);
        assert_eq!(repository_name(&s(&repo.join("src"))), "proj");
    }

    #[test]
    fn detached_none_and_empty() {
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("d");
        write(&repo.join(".git/HEAD"), "0123456789abcdef\n");
        assert_eq!(git_context(&s(&repo)), ("d".into(), "".into()));
        assert_eq!(git_context(""), Default::default());
        let plain = t.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(git_context(&s(&plain)).1, "");
        assert_eq!(repository_name(""), "");
    }

    #[test]
    fn linked_worktree_with_relative_pointer() {
        let t = tempfile::tempdir().unwrap();
        let main = t.path().join("main");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &main.join(".git/worktrees/wt/HEAD"),
            "ref: refs/heads/topic\n",
        );
        write(&main.join(".git/worktrees/wt/commondir"), "../..\n");
        let wt = t.path().join("wt");
        write(&wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n");
        assert_eq!(git_context(&s(&wt)), ("wt".into(), "topic".into()));
        assert_eq!(repository_name(&s(&wt)), "main");
    }

    #[test]
    fn bare_clone_with_sibling_worktrees() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("monorepo");
        write(&root.join(".bare/HEAD"), "ref: refs/heads/main\n");
        write(&root.join(".git"), "gitdir: ./.bare\n");
        let gitdir = root.join(".bare/worktrees/ABC-1");
        write(&gitdir.join("HEAD"), "ref: refs/heads/alice/ABC-1\n");
        write(&gitdir.join("commondir"), "../..\n");
        let wt = root.join("ABC-1");
        write(&wt.join(".git"), &format!("gitdir: {}\n", gitdir.display()));
        assert_eq!(git_context(&s(&wt)), ("ABC-1".into(), "alice/ABC-1".into()));
        assert_eq!(repository_name(&s(&wt)), "monorepo");
        assert_eq!(git_context(&s(&root)), ("monorepo".into(), "main".into()));
        assert_eq!(repository_name(&s(&root)), "monorepo");
    }

    #[test]
    fn non_gitdir_pointer_is_not_a_repo() {
        let t = tempfile::tempdir().unwrap();
        write(&t.path().join("x/.git"), "nonsense\n");
        assert_eq!(repository_name(&s(&t.path().join("x"))), "");
    }

    fn git(args: &[&str], dir: &Path) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn reftable_placeholder_asks_git() {
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("rt");
        fs::create_dir(&repo).unwrap();
        git(
            &["init", "-q", "--ref-format=reftable", "-b", "feature"],
            &repo,
        );
        let head = fs::read_to_string(repo.join(".git/HEAD")).unwrap();
        assert_eq!(branch_from_head(&head), REFTABLE_PLACEHOLDER);
        let commit = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ];
        git(&commit, &repo);
        assert_eq!(git_context(&s(&repo)), ("rt".into(), "feature".into()));
        git(&["checkout", "-q", "--detach"], &repo);
        assert_eq!(git_context(&s(&repo)), ("rt".into(), "".into()));
    }

    #[test]
    fn symbolic_head_is_memoised_per_worktree() {
        let mut reader = GitReader::default();
        let dir = Path::new("/definitely/not/a/repo");
        reader.symrefs.insert(dir.to_path_buf(), "cached".into());
        assert_eq!(reader.symbolic_head(dir), "cached");
    }
}
