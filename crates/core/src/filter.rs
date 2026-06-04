use std::ffi::OsStr;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CopyFilter;

impl CopyFilter {
    pub(crate) fn excludes(self, path: &Path) -> bool {
        let mut parts = path.components().filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        });

        let first = match parts.next() {
            Some(p) => p,
            None => return false,
        };

        if excludes_component(first) {
            return true;
        }

        let mut prev = first;
        parts.any(|part| {
            let result = excludes_component(part) || matches_yarn_artifact(prev, part);
            prev = part;
            result
        })
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
        let filter = CopyFilter;

        assert!(filter.excludes(Path::new("packages/app/node_modules/react/index.js")));
        assert!(filter.excludes(Path::new("packages/app/.yarn/cache/react.zip")));
        assert!(!filter.excludes(Path::new("packages/app/package-lock.json")));
    }
}
