# Specio CLI

`specio` brings your product's specs repository into the `specs/` folder of your implementation
repository, shows what changed on either side, and sends your edits back through the Specio API.
It never talks to GitHub directly and never needs GitHub credentials.

## Install

No Rust toolchain is needed. The installers put `specio` in a per-user folder (no `sudo`/admin), verify the
download against `SHA256SUMS`, and are safe to re-run.

```bash
# macOS (Apple Silicon or Intel) and Linux (x86_64)
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/dayatech-labs/specio-cli/releases/latest/download/install.sh | sh

# pin a version (e.g. for company deployments)
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/dayatech-labs/specio-cli/releases/download/v0.1.0/install.sh | sh -s -- --version 0.1.0
```

```powershell
# Windows PowerShell (x86_64)
irm https://github.com/dayatech-labs/specio-cli/releases/latest/download/install.ps1 | iex

# pin a version
& ([scriptblock]::Create((irm https://github.com/dayatech-labs/specio-cli/releases/download/v0.1.0/install.ps1))) -Version 0.1.0
```

| OS | Installed to | Notes |
| --- | --- | --- |
| macOS, Linux | `~/.local/bin/specio` | the folder is added to your shell profile once if it is missing from `PATH` |
| Windows | `%LOCALAPPDATA%\Specio\bin\specio.exe` | the folder is added to your user `PATH` once |

Reload your shell (or open a new terminal) if `specio` is not found right after installing.

**Package managers.** Each release also ships a Homebrew formula (`specio.rb`), a Scoop manifest
(`specio.scoop.json`), and a WinGet manifest (`Dayatech.Specio.yaml`) pointing at the same fixed-version
URLs and checksums. **Manual install:** download `specio-<version>-<target>.tar.gz` (or `.zip`) from the
[releases page](https://github.com/dayatech-labs/specio-cli/releases), verify it with `SHA256SUMS`, and put
the `specio` binary on your `PATH`.

Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-musl`, `x86_64-pc-windows-msvc`.
Other systems are refused with a link to the archives; the installer never guesses.

### Upgrade, downgrade, pin

```bash
specio upgrade --check            # installed vs. latest; exit 0 either way, non-zero if the check itself fails
specio upgrade                    # asks for confirmation on a terminal
specio upgrade --yes              # for automation
specio upgrade --version 0.1.0 --yes   # pin or downgrade
```

`upgrade` verifies size and SHA-256, runs `--version` on the new binary, then swaps it in atomically; any failure
keeps the old binary. It only replaces binaries the official installer put in the folders above. If `specio`
is managed by Homebrew, Scoop, WinGet, or Nix it stops and prints the package manager's own command
(`brew upgrade specio`, `scoop update specio`, ...). It never runs on its own during `login`, `pull`, or any other command.

## Quick start

```bash
specio login                              # approve this device in your browser
specio list                               # repositories you may use
cd my-frontend-repo
specio init payment-specs --type frontend # short name works when it is unique; otherwise owner/repo
specio pull                               # downloads every specs document into ./specs
# ... edit files under specs/ ...
specio status                             # unchanged / local-only / remote-only / conflict / deleted
specio diff epic-payment/prd.md           # against the latest remote (add --base to compare offline)
specio push                               # one batch through the API
specio context epic-payment/create-payment   # files a coding agent should read, one per line
```

`specio help` lists every command; `specio <command> --help` shows usage, arguments, flags, examples, and exit codes.
Add `--json` to any command for machine-readable output (errors are JSON on stderr).

| Command | What it does |
| --- | --- |
| `login` / `logout` | PKCE device login; logout revokes this device and deletes local credentials and cache |
| `list` | repositories you can use, from the local capability cache while it is valid |
| `init <repo> --type <frontend\|backend\|cli>` | writes `.specio/config.toml`, creates `specs/`, updates `.gitignore`; re-init needs `--reconfigure` |
| `pull` | full-project sync; never overwrites local work |
| `status` | local vs. last sync vs. latest remote, per path |
| `diff <path> [--base]` | unified diff; `--base` needs no network |
| `update <path>` | apply the latest remote version of one file, only when it has no local changes |
| `push` | send local changes as one batch; refused while the workspace has conflicts |
| `context <epic>/<feature>` | deterministic list of context files, from the local copy |
| `upgrade` | check for / install a newer `specio` binary |

### How sync stays safe

* `pull` compares three versions of each file (remote, last synced baseline, your copy). Remote-only changes are
  applied; local-only edits are kept; a file changed on both sides, a remote delete over a local edit, or a remote
  add over an untracked file is a **conflict** and is left exactly as it is. The lock (`.specio/lock.json`) only
  advances when the whole manifest reconciled without conflicts, and `push` is refused until then.
* Every file is fetched at the same commit as the manifest and checked against its Git blob SHA. If the
  repository moves mid-pull, the attempt is discarded and repeated (3 tries), with nothing changed locally.
* Files are written through a temporary file and an atomic rename. Symbolic links and paths outside the specs
  folder are never followed.
* There is no force or discard-local option. `push` never merges and never retries a write automatically; after a
  rejection use `status`, `diff`, `pull`. Whether you may write a path is always decided by the API.

### Workspace files

```
.specio/config.toml      project mapping — safe to commit
.specio/lock.json        last fully synced commit, unresolved conflicts      ┐
.specio/manifest.json    per-path baseline hashes                            │ ignored by Git
.specio/base/            exact copies of the last applied remote content     ┘
specs/                   the working copy (ignored by Git)
```

## Security

* The Specio refresh credential lives only in the OS credential store (Keychain, Credential Manager, Secret
  Service). The 15-minute access token lives only in memory. Google and GitHub tokens never reach the CLI.
* The capability cache (projects, roles, expiry) is stored in the per-user application-data folder with owner-only
  permissions. It is never used after it expires and never authorizes anything: the API checks every request.
* A background job (launchd on macOS, a systemd user timer on Linux, Task Scheduler on Windows) refreshes the cache
  shortly before it expires. If it is unavailable, the next command refreshes in the foreground. `specio logout`
  removes the job.
* Logs never contain tokens or document content. Set `SPECIO_LOG=debug` to see request method, path, status, and
  request ID.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | unexpected failure (I/O, internal) |
| 2 | invalid usage |
| 3 | not logged in, or session expired/revoked |
| 4 | network or server unavailable, including rate limits |
| 5 | conflict: unresolved conflicts, rejected push, workspace not clean |
| 6 | permission denied by the API |
| 7 | invalid input or workspace state (unknown path, not initialized, ambiguous repository) |

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `specio: command not found` after install | open a new terminal, or `export PATH="$HOME/.local/bin:$PATH"` (Windows: new terminal) |
| `credential store: ... Secret Service` on Linux | start a Secret Service provider (GNOME Keyring/KWallet) in your session; headless servers have none |
| `error: not logged in` (exit 3) | run `specio login`. A revoked or expired session also lands here after local state is cleared |
| `pull` reports conflicts | `specio diff <path>`, edit the file to the result you want, then `specio pull` again |
| `push` is refused: "workspace is not clean" | a previous pull left conflicts or never completed: `specio status`, resolve, `specio pull` |
| `push` rejected with `head_changed` / `document_changed` (exit 5) | someone changed the specs first; nothing was committed: `specio pull`, re-check, push again |
| `push` rejected with `policy_denied` (exit 6) | your role may not write that path; ask a Project Admin. `list` refreshes after the denial |
| "the remote kept changing while pulling" | the repository moved during all 3 attempts; nothing changed locally, retry |
| `429`/rate-limited (exit 4) | reads are retried automatically a few times; wait a minute and retry |
| `another specio command is already running in this workspace` | wait for it to finish; it is a per-workspace lock |
| `upgrade` says it was "not installed by the official installer" | reinstall with the installer, or use your package manager |

## Development

```bash
cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings
cargo test --locked          # unit tests + integration tests against an in-process fake API
cargo deny check             # advisories, licenses, bans, sources
```

Rust 1.89+ (edition 2024). Tests never touch the real credential store, scheduler, or network: they run the
library against a stateful fake of the API contract in `specio-api/contract/` (`tests/support/`). See
[docs/releasing.md](docs/releasing.md) for the release process.
