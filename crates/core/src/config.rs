use crate::{Error, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct Config {
    precreate: Vec<Hook>,
    postcreate: Vec<Hook>,
    preremove: Vec<Hook>,
    postremove: Vec<Hook>,
}

impl Config {
    pub(crate) fn load(workspace: &Path) -> Result<Self> {
        let path = workspace.join(".rift.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        parse(&path, &fs::read_to_string(&path)?)
    }

    pub(crate) fn precreate(&self) -> &[Hook] {
        &self.precreate
    }

    pub(crate) fn postcreate(&self) -> &[Hook] {
        &self.postcreate
    }

    pub(crate) fn preremove(&self) -> &[Hook] {
        &self.preremove
    }

    pub(crate) fn postremove(&self) -> &[Hook] {
        &self.postremove
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Hook {
    run: String,
}

impl Hook {
    pub(crate) fn run(&self) -> &str {
        &self.run
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    #[serde(default)]
    hooks: RawHooks,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHooks {
    #[serde(default)]
    precreate: Vec<RawHook>,
    #[serde(default)]
    postcreate: Vec<RawHook>,
    #[serde(default)]
    preremove: Vec<RawHook>,
    #[serde(default)]
    postremove: Vec<RawHook>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHook {
    run: String,
}

fn parse(path: &Path, contents: &str) -> Result<Config> {
    let raw = toml::from_str::<RawConfig>(contents)
        .map_err(|error| invalid_config(path, error.to_string()))?;
    if raw.version != 1 {
        return Err(invalid_config(
            path,
            format!("unsupported config version {}", raw.version),
        ));
    }
    let parse_hooks = |name: &str, steps: Vec<RawHook>| {
        steps
            .into_iter()
            .map(|step| {
                let run = step.run.trim().to_owned();
                if run.is_empty() {
                    Err(invalid_config(path, format!("{name} run cannot be empty")))
                } else {
                    Ok(Hook { run })
                }
            })
            .collect::<Result<Vec<_>>>()
    };
    Ok(Config {
        precreate: parse_hooks("precreate", raw.hooks.precreate)?,
        postcreate: parse_hooks("postcreate", raw.hooks.postcreate)?,
        preremove: parse_hooks("preremove", raw.hooks.preremove)?,
        postremove: parse_hooks("postremove", raw.hooks.postremove)?,
    })
}

fn invalid_config(path: &Path, message: impl Into<String>) -> Error {
    Error::InvalidConfig {
        path: PathBuf::from(path),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordered_postcreate_steps() {
        let config = parse(
            Path::new(".rift.toml"),
            r#"
version = 1

[[hooks.postcreate]]
run = "echo one"

[[hooks.postcreate]]
run = "echo two"
"#,
        )
        .unwrap();

        assert_eq!(
            config
                .postcreate()
                .iter()
                .map(Hook::run)
                .collect::<Vec<_>>(),
            vec!["echo one", "echo two"]
        );
    }

    #[test]
    fn parses_all_lifecycle_hooks() {
        let config = parse(
            Path::new(".rift.toml"),
            r#"
version = 1
[[hooks.precreate]]
run = "echo precreate"
[[hooks.preremove]]
run = "echo preremove"
[[hooks.postremove]]
run = "echo postremove"
"#,
        )
        .unwrap();

        assert_eq!(config.precreate()[0].run(), "echo precreate");
        assert_eq!(config.preremove()[0].run(), "echo preremove");
        assert_eq!(config.postremove()[0].run(), "echo postremove");
    }

    #[test]
    fn rejects_empty_steps() {
        assert!(matches!(
            parse(
                Path::new(".rift.toml"),
                r#"
version = 1

[[hooks.postcreate]]
run = " "
"#,
            ),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(matches!(
            parse(
                Path::new(".rift.toml"),
                r#"
version = 1

[[hooks.postcreate]]
run = "echo ok"
shell = "sh"
"#,
            ),
            Err(Error::InvalidConfig { .. })
        ));
    }
}
