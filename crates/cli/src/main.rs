use clap::{Parser, Subcommand, ValueEnum};
use rift::{CopyMode, Create, CreateOptions, HookMode, InitProgress, Manager, RemoveOptions};
use std::path::PathBuf;
use thiserror::Error;

type Result<T> = std::result::Result<T, CliError>;

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Rift(#[from] rift::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(
        "This is the root workspace.\n\nUnregistering it removes Rift metadata and trashes all child rifts.\nRun `rift remove -f` to continue."
    )]
    ForceRequired,
}

#[derive(Parser)]
#[command(name = "rift")]
struct Cli {
    #[arg(long, hide = true)]
    database: Option<PathBuf>,
    #[arg(long, hide = true, global = true)]
    shell_cwd: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Nushell,
}

impl Shell {
    fn init_script(self, executable: &str) -> String {
        match self {
            Shell::Bash | Shell::Zsh => {
                let executable = posix_shell_quote(executable);
                format!(
                    r#"rift() {{
  case "${{1-}}" in
    init|create|remove)
      local __rift_cwd
      __rift_cwd="$({executable} --shell-cwd "$@")" || return $?
      if [ -n "$__rift_cwd" ]; then
        builtin cd -- "$__rift_cwd" || return $?
      fi
      ;;
    *)
      {executable} "$@"
      ;;
  esac
}}"#,
                )
            }
            Shell::Nushell => {
                let executable = nushell_shell_quote(executable);
                format!(
                    r#"def --env --wrapped rift [...rest] {{
  match ($rest | get 0? | default "" | into string) {{
    "init" | "create" | "remove" => {{
      let cwd = (^{executable} --shell-cwd ...$rest | str trim)
      if ($cwd | is-not-empty) {{
        cd $cwd
      }}
    }}
    _ => {{
      ^{executable} ...$rest
    }}
  }}
}}"#,
                )
            }
        }
    }
}

#[derive(Subcommand)]
enum Command {
    ShellInit {
        #[arg(value_enum)]
        shell: Shell,
    },
    Init {
        at: Option<PathBuf>,
        #[arg(long)]
        here: bool,
    },
    Create {
        from: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        into: Option<PathBuf>,
        #[arg(long)]
        copy_all: bool,
        #[arg(long)]
        no_hooks: bool,
    },
    Remove {
        at: Option<PathBuf>,
        #[arg(long)]
        children: bool,
        #[arg(short = 'f', long)]
        force: bool,
        #[arg(long)]
        no_hooks: bool,
    },
    List {
        of: Option<PathBuf>,
    },
    Ancestors {
        of: Option<PathBuf>,
    },
    Gc,
}

fn main() {
    if let Err(error) = run() {
        let message = match &error {
            CliError::Rift(error) => error_message(error),
            _ => error.to_string(),
        };
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn error_message(error: &rift::Error) -> String {
    match error {
        rift::Error::InitializationRequired(_) => {
            "this workspace must be initialized first; run `rift init` from its root folder".into()
        }
        rift::Error::WorkspaceNotInitialized(_) => {
            "no initialized workspace found; run `rift init` from the root folder".into()
        }
        rift::Error::MissingMarker(_) => {
            "this workspace is missing its `.rift` marker; run `rift init` to restore it".into()
        }
        _ => error.to_string(),
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let command = match cli.command {
        Command::ShellInit { shell } => {
            print_shell_init(shell);
            return Ok(());
        }
        command => command,
    };
    let mut manager = match cli.database {
        Some(path) => Manager::open(path)?,
        None => Manager::open_default()?,
    };
    match command {
        Command::ShellInit { shell } => {
            print_shell_init(shell);
            Ok(())
        }
        Command::Init { at, here } => {
            let requested = std::fs::canonicalize(at.unwrap_or(std::env::current_dir()?))?;
            let (at, existing, missing_marker) = init_target(&manager, &requested, here)?;
            let initialized_from_inside = std::env::current_dir()?.starts_with(&at);
            let mut converting = false;
            let outcome = manager.init_with_progress(&at, |progress| match progress {
                InitProgress::CreatingSubvolume => {
                    converting = true;
                    eprintln!("Initializing  {}\n", at.display());
                    eprintln!("First-time setup can take a moment.");
                    eprintln!("New rifts will be instant.\n");
                    eprintln!("Creating BTRFS subvolume...");
                }
                InitProgress::ImportingWorkspace => eprintln!("Importing workspace..."),
                InitProgress::ImportedEntries { .. } => {}
                InitProgress::ActivatingWorkspace
                | InitProgress::RegisteringWorkspace
                | InitProgress::RestoringMarker
                | InitProgress::RemovingOriginal => {}
            })?;
            if outcome.is_converted() {
                if converting {
                    eprintln!("\nReady  {}", at.display());
                } else {
                    eprintln!("Ready  {}", at.display());
                }
                if initialized_from_inside {
                    if cli.shell_cwd {
                        println!("{}", at.display());
                    } else {
                        eprintln!(
                            "run `cd {}` to enter the initialized workspace",
                            at.display()
                        );
                    }
                }
            } else if let Some(root) = missing_marker {
                eprintln!("Restored marker  {}", root.display());
            } else if let Some(existing) = existing {
                eprintln!("Already initialized  {}", existing.display());
            } else {
                eprintln!("Ready  {}", at.display());
            }
            Ok(())
        }
        Command::Create {
            from,
            name,
            into,
            copy_all,
            no_hooks,
        } => {
            let destination = manager.create_with_options(
                Create::new(from.unwrap_or(std::env::current_dir()?))
                    .with_name(name)
                    .with_storage(into),
                CreateOptions::default()
                    .copy_mode(if copy_all {
                        CopyMode::All
                    } else {
                        CopyMode::Filtered
                    })
                    .hook_mode(if no_hooks {
                        HookMode::Skip
                    } else {
                        HookMode::Run
                    }),
            )?;
            if cli.shell_cwd {
                eprintln!("created {}", destination.display());
            }
            println!("{}", destination.display());
            Ok(())
        }
        Command::Remove {
            at,
            children,
            force,
            no_hooks,
        } => {
            let at = manager.workspace(at.unwrap_or(std::env::current_dir()?))?;
            let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
            if children {
                let removed = manager.remove_all_with_options(
                    &at,
                    RemoveOptions::default().hook_mode(if no_hooks {
                        HookMode::Skip
                    } else {
                        HookMode::Run
                    }),
                )?;
                for path in &removed {
                    if cli.shell_cwd {
                        eprintln!("removed {}", path.display());
                    } else {
                        println!("{}", path.display());
                    }
                }
                if cli.shell_cwd && removed.iter().any(|path| cwd.starts_with(path)) {
                    println!("{}", at.display());
                }
            } else {
                let ancestors = manager.ancestors(&at)?;
                let unregistering_root = ancestors.is_empty();
                require_force_for_root(unregistering_root, force)?;
                let destination = if cli.shell_cwd && cwd.starts_with(&at) {
                    if unregistering_root {
                        Some(at.clone())
                    } else {
                        ancestors.into_iter().next()
                    }
                } else {
                    None
                };
                manager.remove_with_options(
                    &at,
                    RemoveOptions::default().hook_mode(if no_hooks {
                        HookMode::Skip
                    } else {
                        HookMode::Run
                    }),
                )?;
                if unregistering_root {
                    eprintln!("Unregistered  {}", at.display());
                }
                if cli.shell_cwd {
                    if !unregistering_root {
                        eprintln!("removed {}", at.display());
                    }
                    if let Some(destination) = destination {
                        println!("{}", destination.display());
                    }
                }
            }
            Ok(())
        }
        Command::List { of } => {
            for path in manager.list(of.unwrap_or(std::env::current_dir()?))? {
                println!("{}", path.display());
            }
            Ok(())
        }
        Command::Ancestors { of } => {
            for path in manager.ancestors(of.unwrap_or(std::env::current_dir()?))? {
                println!("{}", path.display());
            }
            Ok(())
        }
        Command::Gc => {
            for path in manager.gc()? {
                println!("{}", path.display());
            }
            Ok(())
        }
    }
}

fn init_target(
    manager: &Manager,
    requested: &std::path::Path,
    here: bool,
) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>)> {
    if here {
        return Ok((requested.to_path_buf(), None, None));
    }
    match manager.workspace(requested) {
        Ok(root) => Ok((root.clone(), Some(root), None)),
        Err(rift::Error::MissingMarker(root)) => Ok((root.clone(), None, Some(root))),
        Err(rift::Error::WorkspaceNotInitialized(_)) => Ok((git_root(requested), None, None)),
        Err(error) => Err(error.into()),
    }
}

fn git_root(path: &std::path::Path) -> PathBuf {
    path.ancestors()
        .find(|directory| directory.join(".git").exists())
        .unwrap_or(path)
        .to_path_buf()
}

fn require_force_for_root(unregistering_root: bool, force: bool) -> Result<()> {
    if unregistering_root && !force {
        return Err(CliError::ForceRequired);
    }
    Ok(())
}

fn print_shell_init(shell: Shell) {
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rift"));
    println!("{}", shell.init_script(&executable.to_string_lossy()));
}

fn posix_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn nushell_shell_quote(value: &str) -> String {
    let mut hashes = String::from("#");
    while value.contains(&format!("'{}", hashes)) {
        hashes.push('#');
    }
    format!("r{}'{}'{}", hashes, value, hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn initialization_guidance_is_rendered_by_the_cli() {
        let path = PathBuf::from("/tmp/app");

        assert_eq!(
            error_message(&rift::Error::WorkspaceNotInitialized(path.clone())),
            "no initialized workspace found; run `rift init` from the root folder"
        );
        assert_eq!(
            error_message(&rift::Error::MissingMarker(path)),
            "this workspace is missing its `.rift` marker; run `rift init` to restore it"
        );
    }

    #[test]
    fn init_target_selects_git_root_unless_here_is_requested() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("app");
        let nested = root.join("nested");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir(&nested).unwrap();
        let manager = Manager::open(temp.path().join("rift.sqlite")).unwrap();

        assert_eq!(init_target(&manager, &nested, false).unwrap().0, root);
        assert_eq!(init_target(&manager, &nested, true).unwrap().0, nested);
    }

    #[test]
    fn root_unregistration_requires_force() {
        assert!(matches!(
            require_force_for_root(true, false),
            Err(CliError::ForceRequired)
        ));
        assert!(require_force_for_root(true, true).is_ok());
        assert!(require_force_for_root(false, false).is_ok());
        assert_eq!(
            CliError::ForceRequired.to_string(),
            "This is the root workspace.\n\nUnregistering it removes Rift metadata and trashes all child rifts.\nRun `rift remove -f` to continue."
        );
    }

    #[test]
    fn create_command_accepts_copy_and_hook_flags() {
        let cli = Cli::try_parse_from([
            "rift",
            "create",
            "--name",
            "child",
            "--copy-all",
            "--no-hooks",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Create {
                copy_all: true,
                no_hooks: true,
                ..
            }
        ));
    }

    #[test]
    fn remove_command_accepts_no_hooks() {
        let cli = Cli::try_parse_from(["rift", "remove", "--no-hooks"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Remove { no_hooks: true, .. }
        ));
    }

    #[test]
    fn shell_init_renders_posix_wrapper_for_bash_and_zsh() {
        let wrapper = r#"rift() {
  case "${1-}" in
    init|create|remove)
      local __rift_cwd
      __rift_cwd="$('/tmp/rift' --shell-cwd "$@")" || return $?
      if [ -n "$__rift_cwd" ]; then
        builtin cd -- "$__rift_cwd" || return $?
      fi
      ;;
    *)
      '/tmp/rift' "$@"
      ;;
  esac
}"#;

        assert_eq!(Shell::Bash.init_script("/tmp/rift"), wrapper);
        assert_eq!(Shell::Zsh.init_script("/tmp/rift"), wrapper);
    }

    #[test]
    fn shell_init_renders_nushell_wrapper() {
        let wrapper = r#"def --env --wrapped rift [...rest] {
  match ($rest | get 0? | default "" | into string) {
    "init" | "create" | "remove" => {
      let cwd = (^r#'/tmp/rift'# --shell-cwd ...$rest | str trim)
      if ($cwd | is-not-empty) {
        cd $cwd
      }
    }
    _ => {
      ^r#'/tmp/rift'# ...$rest
    }
  }
}"#;

        assert_eq!(Shell::Nushell.init_script("/tmp/rift"), wrapper);
    }

    #[test]
    fn nushell_shell_quote_uses_enough_raw_string_hashes() {
        assert_eq!(nushell_shell_quote("/tmp/rift"), "r#'/tmp/rift'#");
        assert_eq!(
            nushell_shell_quote("/tmp/it's'#rift"),
            "r##'/tmp/it's'#rift'##"
        );
    }
}
