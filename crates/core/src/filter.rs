use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct CopyFilter {
    tracked: Vec<PathBuf>,
}

impl CopyFilter {
    pub(crate) fn for_source(source: &Path) -> Self {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            let tracked = git2::Repository::open(source)
                .and_then(|repository| repository.index())
                .map(|index| {
                    index
                        .iter()
                        .map(|entry| PathBuf::from(OsString::from_vec(entry.path)))
                        .collect()
                })
                .unwrap_or_default();
            Self { tracked }
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Self::default()
    }

    pub(crate) fn excludes(&self, path: &Path) -> bool {
        let parts = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part),
                _ => None,
            })
            .collect::<Vec<_>>();

        let is_artifact = parts.iter().any(|part| excludes_component(part))
            || parts
                .windows(2)
                .any(|parts| matches_yarn_artifact(parts[0], parts[1]));

        is_artifact && !self.tracked.iter().any(|tracked| tracked.starts_with(path))
    }
}

fn excludes_component(part: &OsStr) -> bool {
    [
        "node_modules",
        ".pnpm-store",
        "target",
        ".venv",
        "venv",
        ".tox",
        ".nox",
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        ".next",
        ".nuxt",
        ".svelte-kit",
        ".turbo",
        ".vite",
        ".parcel-cache",
        ".cache",
        "dist",
        "build",
        "coverage",
    ]
    .into_iter()
    .any(|excluded| part == excluded)
}

fn matches_yarn_artifact(first: &OsStr, second: &OsStr) -> bool {
    first == ".yarn"
        && ["cache", "unplugged", "install-state.gz", "build-state.yml"]
            .into_iter()
            .any(|artifact| second == artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_artifacts_at_any_depth() {
        let filter = CopyFilter::default();

        assert!(filter.excludes(Path::new("packages/app/node_modules/react/index.js")));
        assert!(filter.excludes(Path::new("packages/app/.yarn/cache/react.zip")));
        assert!(!filter.excludes(Path::new("packages/app/package-lock.json")));
    }
}
