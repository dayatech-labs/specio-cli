//! The Clap command tree. `speq help` and `speq <command> --help` are generated from here.
use clap::{Args, Parser, Subcommand};

pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\ncommit: ",
    env!("SPEQ_COMMIT"),
    "\ntarget: ",
    env!("SPEQ_TARGET")
);

const EXIT_CODES: &str = "\
Exit codes:
  0  success
  1  unexpected failure (I/O, internal)
  2  invalid usage
  3  not logged in or session expired
  4  network or server unavailable (including rate limits)
  5  conflict: unresolved conflicts, rejected push, or workspace not clean
  6  permission denied by the API
  7  invalid input or workspace state (unknown path, not initialized, ambiguous repo)";

#[derive(Parser, Debug)]
#[command(
    name = "speq",
    version = env!("CARGO_PKG_VERSION"),
    long_version = LONG_VERSION,
    about = "Sync product specs into your implementation repository",
    long_about = "Speq brings the product specs repository into your repository's specs/ folder, \
                  shows what changed on either side, and sends your edits back through the Speq API. \
                  It never talks to GitHub directly and never needs GitHub credentials.",
    after_help = "Run `speq <command> --help` for usage, arguments, flags, examples, and exit codes.\n\nGet started: speq login && speq list && speq init <repo> --type frontend && speq pull",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Print machine-readable JSON instead of text
    #[arg(long, global = true)]
    pub json: bool,

    /// Speq API origin (https only; http is allowed for localhost)
    #[arg(long, global = true, env = "SPEQ_API_URL", value_name = "URL")]
    pub api_url: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Log in to Speq in your browser
    #[command(
        after_help = "Examples:\n  speq login\n  speq login --no-browser\n\nThe access token is kept in the OS credential store and expires after 30 days. There is no refresh: when it\nexpires, commands fail with exit code 3 and you run `speq login` again.\n\n"
    )]
    Login(LoginArgs),

    /// Delete the local credential and cache (--all also ends every other device)
    #[command(
        after_help = "Examples:\n  speq logout\n  speq logout --all\n\nA token cannot be revoked on its own. Plain `logout` forgets it on this machine only; --all ends every token\nyour account holds, on every device and in the Web app. Workspace files and `.speq/` are left untouched.\n\n"
    )]
    Logout(LogoutArgs),

    /// List the specs repositories you are allowed to use
    #[command(
        after_help = "Examples:\n  speq list\n  speq list --json\n\nShown from the local capability cache while it is valid; refreshed automatically after it expires.\n\n"
    )]
    List,

    /// Choose a repository and prepare this directory as a workspace
    #[command(
        after_help = "Arguments:\n  <REPO>  `owner/repo` from `speq list`, or just the name when it is unique\n\nExamples:\n  speq init payment-specs --type frontend\n  speq init acme/payment-specs --type backend --local-dir docs/specs\n  speq init payment-specs --type frontend --reconfigure\n\n"
    )]
    Init(InitArgs),

    /// Download all specs and apply them without overwriting local work
    #[command(
        after_help = "Examples:\n  speq pull\n\nLocal-only edits are kept. A file changed on both sides, a remote delete over a local edit, or a remote\nadd over an untracked file is reported as a conflict and left alone; push is blocked until you resolve it.\n\n"
    )]
    Pull,

    /// Compare the working copy, the last sync, and the latest remote
    #[command(
        after_help = "Categories: unchanged, local-only, remote-only, conflict, deleted (removed locally).\n\nExamples:\n  speq status\n  speq status --json\n\nExits 5 when any path is in conflict.\n\n"
    )]
    Status,

    /// Show a diff of a file against the latest remote (or the last sync with --base)
    #[command(
        after_help = "Arguments:\n  <PATH>  document path relative to the specs folder, as printed by `speq status`\n\nExamples:\n  speq diff epics/epic-payment/prd.md\n  speq diff epics/epic-payment/prd.md --base\n\n--base works offline.\n\n"
    )]
    Diff(DiffArgs),

    /// Apply the latest remote version of one file if it has no local changes
    #[command(
        after_help = "Arguments:\n  <PATH>  document path relative to the specs folder\n\nExamples:\n  speq update epics/epic-payment/prd.md\n\nFails when the local copy changed; there is no force or discard option.\n\n"
    )]
    Update(UpdateArgs),

    /// Send local changes to the API as one batch
    #[command(
        after_help = "Examples:\n  speq push\n\nLists the paths and actions first. Refused when the workspace has conflicts or is not clean. On a\nrejection (HTTP 403/409) nothing is committed, nothing is retried automatically, and your files are untouched;\nuse `speq status`, `speq diff <path>`, and `speq pull`. Writes are decided by the API.\n\n"
    )]
    Push,

    /// Print the files a coding agent should read for one feature
    #[command(
        after_help = "Arguments:\n  <TARGET>  `<epic>/<feature>`, for example epic-payment/create-payment\n\nExamples:\n  speq context epic-payment/create-payment\n  speq context epic-payment/create-payment --json\n\nPrints one path per line in a fixed order, from the local copy (no network). Requires a clean workspace.\n\n"
    )]
    Context(ContextArgs),

    /// Check for or install a newer speq binary
    #[command(
        after_help = "Examples:\n  speq upgrade --check\n  speq upgrade\n  speq upgrade --yes\n  speq upgrade --version 1.4.0 --yes\n\n--check needs no login and changes nothing. Binaries managed by Homebrew, Scoop, WinGet, or another\npackage manager are never replaced; the matching package-manager command is shown instead.\n\n"
    )]
    Upgrade(UpgradeArgs),

    /// Remove speq's local data and, when the installer put it there, the binary
    #[command(
        after_help = "Examples:\n  speq uninstall\n  speq uninstall --yes\n\nRemoves the stored credential, the cache, and the background agent, then the binary if it was placed by the\nofficial installer. Binaries managed by Homebrew, Scoop, WinGet, or another package manager are left in place and\nthe matching package-manager command is shown. Workspaces (`specs/` and `.speq/`) are never touched, and the\nserver-side session is not ended: run `speq logout --all` first for that. Only the credential of the current API\norigin is removed.\n\n"
    )]
    Uninstall(UninstallArgs),

    /// Background snapshot refresh (run by the OS scheduler)
    #[command(hide = true)]
    Agent(AgentArgs),
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Login(_) => "login",
            Command::Logout(_) => "logout",
            Command::List => "list",
            Command::Init(_) => "init",
            Command::Pull => "pull",
            Command::Status => "status",
            Command::Diff(_) => "diff",
            Command::Update(_) => "update",
            Command::Push => "push",
            Command::Context(_) => "context",
            Command::Upgrade(_) => "upgrade",
            Command::Uninstall(_) => "uninstall",
            Command::Agent(_) => "agent",
        }
    }
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Print the approval URL without opening a browser
    #[arg(long)]
    pub no_browser: bool,
}

#[derive(Args, Debug)]
pub struct LogoutArgs {
    /// Also end every other session of this account (all devices and the Web app)
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Repository: `owner/repo`, or a name that is unique in `speq list`
    pub repo: String,
    /// What this repository is: frontend, backend, or cli
    #[arg(long = "type", value_name = "TYPE")]
    pub repository_type: String,
    /// Folder for the specs, relative to this directory
    #[arg(long, default_value = "specs", value_name = "DIR")]
    pub local_dir: String,
    /// Replace the existing configuration of this workspace
    #[arg(long)]
    pub reconfigure: bool,
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// Document path relative to the specs folder
    pub path: String,
    /// Compare with the last synced baseline instead of the latest remote (offline)
    #[arg(long)]
    pub base: bool,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Document path relative to the specs folder
    pub path: String,
}

#[derive(Args, Debug)]
pub struct ContextArgs {
    /// `<epic>/<feature>`
    pub target: String,
}

#[derive(Args, Debug)]
pub struct UpgradeArgs {
    /// Only report the installed and latest versions
    #[arg(long)]
    pub check: bool,
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Install this exact version (also allows downgrades)
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
}

#[derive(Args, Debug)]
pub struct UninstallArgs {
    /// Do not ask for confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub action: AgentAction,
}

#[derive(Subcommand, Debug)]
pub enum AgentAction {
    /// One scheduled tick: refresh the capability snapshot when it is close to expiry
    Run,
}

/// Exit-code table, exposed so docs and tests can reuse it.
pub fn exit_code_help() -> &'static str {
    EXIT_CODES
}
