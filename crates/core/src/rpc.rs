use crate::{CopyMode, Create, CreateOptions, Error, HookMode, Manager, RemoveOptions};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize)]
struct Request {
    database: Option<PathBuf>,
    #[serde(flatten)]
    command: Command,
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    Init {
        at: PathBuf,
    },
    Create {
        from: PathBuf,
        name: Option<String>,
        into: Option<PathBuf>,
        #[serde(rename = "copyAll")]
        copy_all: Option<bool>,
        hooks: Option<bool>,
    },
    Remove {
        at: PathBuf,
        all: Option<bool>,
        hooks: Option<bool>,
    },
    List {
        of: PathBuf,
    },
    Ancestors {
        of: PathBuf,
    },
    Gc,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Value {
    Empty(()),
    Path(PathBuf),
    Paths(Vec<PathBuf>),
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Ok { value: Value },
    Error { error: Failure },
}

#[derive(Serialize)]
struct Failure {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    committed: Option<bool>,
}

impl Failure {
    fn protocol(code: &'static str, message: String) -> Self {
        Self {
            code,
            message,
            path: None,
            hook: None,
            committed: None,
        }
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let (code, path) = match &error {
            Error::Io(_) => ("io", None),
            Error::IoAt { path, .. } => ("io", Some(path.clone())),
            Error::Database(_) => ("database", None),
            Error::Walk(_) => ("walk", None),
            Error::Path(_) => ("invalid_path", None),
            Error::CowUnavailable(_) => ("cow_unavailable", None),
            Error::InitializationRequired(path) => ("initialization_required", Some(path.clone())),
            Error::WorkspaceNotInitialized(path) => {
                ("workspace_not_initialized", Some(path.clone()))
            }
            Error::MissingMarker(path) => ("missing_marker", Some(path.clone())),
            Error::UnsupportedEntry(path) => ("unsupported_entry", Some(path.clone())),
            Error::UnsafeGit(_) => ("unsafe_git", None),
            Error::NotManaged(path) => ("not_managed", Some(path.clone())),
            Error::MarkerMismatch(path) => ("marker_mismatch", Some(path.clone())),
            Error::UnknownMarker(path) => ("unknown_marker", Some(path.clone())),
            Error::AlreadyExists(path) => ("already_exists", Some(path.clone())),
            Error::NamesExhausted(path) => ("names_exhausted", Some(path.clone())),
            Error::MissingRift(path) => ("missing_rift", Some(path.clone())),
            Error::OverlappingWorkspace(path) => ("inside_source", Some(path.clone())),
            Error::InvalidConfig { path, .. } => ("invalid_config", Some(path.clone())),
            Error::HookFailed { path, .. } => ("hook_failed", Some(path.clone())),
        };
        let (hook, committed) = match &error {
            Error::HookFailed { hook, .. } => (
                Some(hook.clone()),
                Some(matches!(hook.as_str(), "postcreate" | "postremove")),
            ),
            _ => (None, None),
        };
        Self {
            code,
            message: error.to_string(),
            path,
            hook,
            committed,
        }
    }
}

pub fn call(input: &str) -> String {
    let response = std::panic::catch_unwind(|| match execute(input) {
        Ok(value) => Response::Ok { value },
        Err(error) => Response::Error { error },
    })
    .unwrap_or_else(|_| Response::Error {
        error: Failure::protocol("panic", "rift RPC call panicked".into()),
    });
    serialize(response)
}

pub fn error(code: &'static str, message: impl Into<String>) -> String {
    serialize(Response::Error {
        error: Failure::protocol(code, message.into()),
    })
}

fn serialize(response: Response) -> String {
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"status":"error","error":{"code":"serialization","message":"failed to serialize response"}}"#
            .to_owned()
    })
}

fn execute(input: &str) -> Result<Value, Failure> {
    let request: Request = serde_json::from_str(input)
        .map_err(|error| Failure::protocol("invalid_request", error.to_string()))?;
    let mut manager = request
        .database
        .map_or_else(Manager::open_default, Manager::open)
        .map_err(Failure::from)?;
    match request.command {
        Command::Init { at } => manager
            .init(at)
            .map(|_| Value::Empty(()))
            .map_err(Failure::from),
        Command::Create {
            from,
            name,
            into,
            copy_all,
            hooks,
        } => manager
            .create_with_options(
                Create::new(from).with_name(name).with_storage(into),
                CreateOptions::default()
                    .copy_mode(if copy_all.unwrap_or(false) {
                        CopyMode::All
                    } else {
                        CopyMode::Filtered
                    })
                    .hook_mode(if hooks.unwrap_or(true) {
                        HookMode::Run
                    } else {
                        HookMode::Skip
                    }),
            )
            .map(Value::Path)
            .map_err(Failure::from),
        Command::Remove { at, all, hooks } => {
            let options = RemoveOptions::default().hook_mode(if hooks.unwrap_or(true) {
                HookMode::Run
            } else {
                HookMode::Skip
            });
            if all.unwrap_or(false) {
                manager
                    .remove_all_with_options(at, options)
                    .map(Value::Paths)
                    .map_err(Failure::from)
            } else {
                manager
                    .remove_with_options(at, options)
                    .map(|()| Value::Empty(()))
                    .map_err(Failure::from)
            }
        }
        Command::List { of } => manager.list(of).map(Value::Paths).map_err(Failure::from),
        Command::Ancestors { of } => manager
            .ancestors(of)
            .map(Value::Paths)
            .map_err(Failure::from),
        Command::Gc => manager.gc().map(Value::Paths).map_err(Failure::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_errors_with_structured_hook_state() {
        let response = serde_json::to_value(Response::Error {
            error: Error::HookFailed {
                hook: "postcreate".into(),
                path: PathBuf::from("/tmp/app"),
                command: "exit 1".into(),
                message: "exited with 1".into(),
            }
            .into(),
        })
        .unwrap();

        assert_eq!(response["error"]["code"], "hook_failed");
        assert_eq!(response["error"]["path"], "/tmp/app");
        assert_eq!(response["error"]["hook"], "postcreate");
        assert_eq!(response["error"]["committed"], true);
    }

    #[test]
    fn accepts_create_and_remove_options() {
        let create = serde_json::from_str::<Request>(
            r#"{"command":"create","from":"/tmp/app","copyAll":true,"hooks":false}"#,
        )
        .unwrap();
        let remove = serde_json::from_str::<Request>(
            r#"{"command":"remove","at":"/tmp/app","all":true,"hooks":false}"#,
        )
        .unwrap();

        assert!(matches!(
            create.command,
            Command::Create {
                copy_all: Some(true),
                hooks: Some(false),
                ..
            }
        ));
        assert!(matches!(
            remove.command,
            Command::Remove {
                all: Some(true),
                hooks: Some(false),
                ..
            }
        ));
    }
}
