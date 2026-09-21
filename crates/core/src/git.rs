use crate::{Error, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Source {
    PlainDirectory,
    Repository,
}

impl Source {
    pub(crate) fn is_repository(self) -> bool {
        matches!(self, Self::Repository)
    }
}

pub(crate) fn check_source(path: &Path) -> Result<Source> {
    let git = path.join(".git");
    let metadata = match fs::symlink_metadata(&git) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Source::PlainDirectory);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        return Err(Error::UnsafeGit(
            "linked Git worktree sources are not supported".into(),
        ));
    }

    for state in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
        "index.lock",
        "HEAD.lock",
    ] {
        if git.join(state).exists() {
            return Err(Error::UnsafeGit(format!("Git state in progress: {state}")));
        }
    }
    Ok(Source::Repository)
}

pub(crate) fn hide_marker(path: &Path) -> Result<()> {
    let info = path.join(".git").join("info");
    fs::create_dir_all(&info)?;
    let exclude = info.join("exclude");
    let existing = match fs::read_to_string(&exclude) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    if existing.lines().any(|line| line.trim_end() == "/.rift") {
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    fs::write(exclude, format!("{existing}{separator}/.rift\n"))?;
    Ok(())
}

pub(crate) fn detach_destination(path: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    // Avoid process startup when libgit2 understands the repository;
    // the Git CLI remains the authority for layouts it cannot resolve.
    if let Some(commit) = resolve_head_commit(path) {
        fs::write(path.join(".git").join("HEAD"), format!("{commit}\n"))?;
        return Ok(());
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()?;
    if !output.status.success() {
        return Ok(());
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    fs::write(path.join(".git").join("HEAD"), format!("{commit}\n"))?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn resolve_head_commit(path: &Path) -> Option<git2::Oid> {
    let repository = git2::Repository::open(path).ok()?;
    repository
        .head()
        .ok()?
        .peel_to_commit()
        .ok()
        .map(|commit| commit.id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn linked_worktree_marker_is_rejected() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join(".git"), "gitdir: elsewhere").unwrap();

        assert!(matches!(
            check_source(temp.path()),
            Err(Error::UnsafeGit(_))
        ));

        #[cfg(unix)]
        {
            fs::remove_file(temp.path().join(".git")).unwrap();
            let external = TempDir::new().unwrap();
            std::os::unix::fs::symlink(external.path(), temp.path().join(".git")).unwrap();
            assert!(matches!(
                check_source(temp.path()),
                Err(Error::UnsafeGit(_))
            ));
        }
    }

    #[test]
    fn check_source_distinguishes_plain_and_git_directories() {
        let plain = TempDir::new().unwrap();
        assert_eq!(check_source(plain.path()).unwrap(), Source::PlainDirectory);

        let git = TempDir::new().unwrap();
        fs::create_dir(git.path().join(".git")).unwrap();
        assert_eq!(check_source(git.path()).unwrap(), Source::Repository);
    }

    #[test]
    fn hide_marker_creates_and_appends_exclude_cleanly() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        hide_marker(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join(".git/info/exclude")).unwrap(),
            "/.rift\n"
        );
        fs::write(temp.path().join(".git/info/exclude"), "existing").unwrap();
        hide_marker(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join(".git/info/exclude")).unwrap(),
            "existing\n/.rift\n"
        );
        hide_marker(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join(".git/info/exclude")).unwrap(),
            "existing\n/.rift\n"
        );
        fs::write(temp.path().join(".git/info/exclude"), " /.rift\n").unwrap();
        hide_marker(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join(".git/info/exclude")).unwrap(),
            " /.rift\n/.rift\n"
        );
    }

    #[test]
    fn detach_does_nothing_for_a_repository_without_a_commit() {
        let temp = TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        let head = fs::read_to_string(temp.path().join(".git/HEAD")).unwrap();

        detach_destination(temp.path()).unwrap();

        assert_eq!(
            fs::read_to_string(temp.path().join(".git/HEAD")).unwrap(),
            head
        );
    }
}
