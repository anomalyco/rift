use crate::{Error, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct Config {
    postcreate: Vec<Postcreate>,
    postremove: Vec<Postremove>,
}

impl Config {
    pub(crate) fn load(workspace: &Path) -> Result<Self> {
        let path = workspace.join(".rift.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        parse(&path, &fs::read_to_string(&path)?)
    }

    pub(crate) fn postcreate(&self) -> &[Postcreate] {
        &self.postcreate
    }

    pub(crate) fn postremove(&self) -> &[Postremove] {
        &self.postremove
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Postcreate {
    run: String,
}

impl Postcreate {
    pub(crate) fn run(&self) -> &str {
        &self.run
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Postremove {
    run: String,
}

impl Postremove {
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
    postcreate: Vec<RawPostcreate>,
    #[serde(default)]
    postremove: Vec<RawPostremove>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPostcreate {
    run: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPostremove {
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
    let postcreate = raw
        .hooks
        .postcreate
        .into_iter()
        .map(|step| {
            let run = step.run.trim().to_owned();
            if run.is_empty() {
                Err(invalid_config(path, "postcreate run cannot be empty"))
            } else {
                Ok(Postcreate { run })
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let postremove = raw
        .hooks
        .postremove
        .into_iter()
        .map(|step| {
            let run = step.run.trim().to_owned();
            if run.is_empty() {
                Err(invalid_config(path, "postremove run cannot be empty"))
            } else {
                Ok(Postremove { run })
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Config {
        postcreate,
        postremove,
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
                .map(Postcreate::run)
                .collect::<Vec<_>>(),
            vec!["echo one", "echo two"]
        );
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

    #[test]
    fn parses_ordered_postremove_steps() {
        let config = parse(
            Path::new(".rift.toml"),
            r#"
version = 1

[[hooks.postremove]]
run = "echo one"

[[hooks.postremove]]
run = "echo two"
"#,
        )
        .unwrap();

        assert_eq!(
            config
                .postremove()
                .iter()
                .map(Postremove::run)
                .collect::<Vec<_>>(),
            vec!["echo one", "echo two"]
        );
    }

    #[test]
    fn rejects_empty_postremove_steps() {
        assert!(matches!(
            parse(
                Path::new(".rift.toml"),
                r#"
version = 1

[[hooks.postremove]]
run = " "
"#,
            ),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn rejects_unknown_fields_on_postremove() {
        assert!(matches!(
            parse(
                Path::new(".rift.toml"),
                r#"
version = 1

[[hooks.postremove]]
run = "echo ok"
shell = "sh"
"#,
            ),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn parses_both_hook_types_together() {
        let config = parse(
            Path::new(".rift.toml"),
            r#"
version = 1

[[hooks.postcreate]]
run = "npm install"

[[hooks.postremove]]
run = "docker compose down"
"#,
        )
        .unwrap();

        assert_eq!(config.postcreate().len(), 1);
        assert_eq!(config.postremove().len(), 1);
    }
}
