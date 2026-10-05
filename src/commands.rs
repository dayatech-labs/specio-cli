//! Dispatch from the parsed command line to the library, then to output.
use crate::cli::{AgentAction, Cli, Command, exit_code_help};
use crate::context;
use crate::env::Env;
use crate::error::{Error, Result, exit};
use crate::init::{self, InitOptions};
use crate::output::emit;
use crate::session::{AgentOutcome, Session};
use crate::sync::{pull, push, status};
use crate::upgrade::{self, Install};
use crate::workspace::Workspace;
use clap::{CommandFactory, FromArgMatches};
use std::io::{IsTerminal, Write};

/// The Clap command with the shared exit-code table appended to every command's help.
pub fn build_command() -> clap::Command {
    let codes = exit_code_help();
    let mut cmd = Cli::command();
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    for name in names {
        cmd = cmd.mut_subcommand(name, |sub| {
            let existing = sub
                .get_after_help()
                .map(|s| s.to_string())
                .unwrap_or_default();
            sub.after_help(format!(
                "{}{codes}",
                if existing.is_empty() {
                    String::new()
                } else {
                    format!("{}\n\n", existing.trim_end())
                }
            ))
        });
    }
    let top = cmd
        .get_after_help()
        .map(|s| s.to_string())
        .unwrap_or_default();
    cmd.after_help(format!("{top}\n\n{codes}"))
}

pub fn parse_args<I: IntoIterator<Item = String>>(
    args: I,
) -> std::result::Result<Cli, clap::Error> {
    let matches = build_command().try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

fn workspace() -> Result<Workspace> {
    Workspace::discover(&std::env::current_dir().map_err(|e| Error::Io {
        context: "read the current directory".into(),
        source: e,
    })?)
}

fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::invalid(
            "confirmation needs a terminal; pass --yes to proceed without asking",
        ));
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| Error::Io {
            context: "read the answer".into(),
            source: e,
        })?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

pub async fn run(cli: Cli, env: &Env) -> Result<u8> {
    let json = cli.json;
    match cli.command {
        Command::Login(_) => Ok(emit(json, &Session::new(env).login().await?)),
        Command::Logout(args) => Ok(emit(json, &Session::new(env).logout(args.all).await?)),
        Command::List => Ok(emit(json, &init::list(env).await?)),
        Command::Init(args) => {
            let root = std::env::current_dir().map_err(|e| Error::Io {
                context: "read the current directory".into(),
                source: e,
            })?;
            let root = std::fs::canonicalize(&root).map_err(|e| Error::Io {
                context: "resolve the current directory".into(),
                source: e,
            })?;
            let options = InitOptions {
                repo: &args.repo,
                repository_type: &args.repository_type,
                local_dir: &args.local_dir,
                reconfigure: args.reconfigure,
            };
            Ok(emit(json, &init::init(env, &root, options).await?))
        }
        Command::Pull => Ok(emit(json, &pull::pull(env, &workspace()?).await?)),
        Command::Status => Ok(emit(json, &status::status(env, &workspace()?).await?)),
        Command::Diff(args) => {
            let ws = workspace()?;
            let report = if args.base {
                status::diff_base(&ws, &args.path)?
            } else {
                status::diff_remote(env, &ws, &args.path).await?
            };
            Ok(emit(json, &report))
        }
        Command::Update(args) => Ok(emit(
            json,
            &pull::update(env, &workspace()?, &args.path).await?,
        )),
        Command::Push => Ok(emit(json, &push::push(env, &workspace()?).await?)),
        Command::Context(args) => Ok(emit(json, &context::context(&workspace()?, &args.target)?)),
        Command::Upgrade(args) => upgrade_command(json, args).await,
        Command::Agent(args) => match args.action {
            AgentAction::Run => {
                match Session::new(env).agent_tick(true).await? {
                    AgentOutcome::LoggedOut
                    | AgentOutcome::StillFresh
                    | AgentOutcome::Refreshed => {}
                }
                Ok(exit::OK)
            }
        },
    }
}

async fn upgrade_command(json: bool, args: crate::cli::UpgradeArgs) -> Result<u8> {
    let base = std::env::var("SPEQ_RELEASE_URL")
        .unwrap_or_else(|_| upgrade::DEFAULT_RELEASE_BASE.to_string());
    let pinned = args
        .version
        .as_deref()
        .map(|v| {
            semver::Version::parse(v.trim_start_matches('v'))
                .map_err(|_| Error::invalid(format!("{v:?} is not a valid version")))
        })
        .transpose()?;
    let http = upgrade::http_client()?;
    let manifest =
        upgrade::fetch_manifest(&http, &upgrade::manifest_url(&base, pinned.as_ref())?).await?;
    let current = upgrade::current_version();
    let report = upgrade::check(&current, &manifest);

    if args.check {
        return Ok(emit(json, &report));
    }

    let exe = std::env::current_exe().map_err(|e| Error::Io {
        context: "locate the speq executable".into(),
        source: e,
    })?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let dirs = directories::BaseDirs::new();
    let official = upgrade::official_dir(
        dirs.as_ref().map(|d| d.home_dir()),
        dirs.as_ref().map(|d| d.data_local_dir()),
    );
    match upgrade::detect_install(&exe, official.as_deref()) {
        Install::Managed { manager, command } => {
            return Err(Error::invalid(format!(
                "speq is managed by {manager}; update it with `{command}` (nothing was changed)"
            )));
        }
        Install::Unmanaged => {
            return Err(Error::invalid(format!(
                "{} was not installed by the official installer, so `speq upgrade` will not replace it; reinstall with the installer or your package manager",
                exe.display()
            )));
        }
        Install::Official => {}
    }

    let target = pinned.clone().unwrap_or_else(|| manifest.version.clone());
    if pinned.is_none() && manifest.version <= current {
        return Ok(emit(
            json,
            &upgrade::UpgradeReport {
                from: current.to_string(),
                to: current.to_string(),
                installed: false,
                path: None,
            },
        ));
    }
    if pinned.as_ref() == Some(&current) {
        return Ok(emit(
            json,
            &upgrade::UpgradeReport {
                from: current.to_string(),
                to: current.to_string(),
                installed: false,
                path: None,
            },
        ));
    }
    let artifact = manifest.artifacts.get(upgrade::CURRENT_TARGET).ok_or_else(|| Error::invalid(format!("this release has no build for {}; download an archive manually from the release page", upgrade::CURRENT_TARGET)))?;
    if !args.yes && !confirm(&format!("Install speq {target} (currently {current})?"))? {
        return Err(Error::invalid("cancelled; nothing was changed"));
    }
    upgrade::install_binary(&http, &artifact.binary, &exe, &target).await?;
    Ok(emit(
        json,
        &upgrade::UpgradeReport {
            from: current.to_string(),
            to: target.to_string(),
            installed: true,
            path: Some(exe.display().to_string()),
        },
    ))
}
