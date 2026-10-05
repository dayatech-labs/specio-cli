# Releasing the Speq CLI

Releases are automatic: one push to `main`, one commit, one workflow (`.github/workflows/release.yml`).

## How a release happens

Every push to `main` runs `.github/workflows/release.yml`. There is no manual tagging: the version and the tag come
from the commit messages since the last `vX.Y.Z` tag (`scripts/next_version.py`), using the conventional-commit
subjects this repository already requires (`<type>: <summary>`):

| Commits since the last tag | Next version |
| --- | --- |
| a breaking commit (`feat!: …`, or a `BREAKING CHANGE:` footer) | major (minor while the version is `0.x`) |
| at least one `feat` | minor |
| only `fix`, `perf`, `refactor` | patch |
| only `docs`, `chore`, `test`, `ci`, `build`, `style`, `revert` (or subjects without a type) | **no release** |

The first releasable commit publishes `v0.1.0`. Merging with a squash commit? Make the squash subject carry the type.

The checked-in `version` in `Cargo.toml` is only a placeholder: CI stamps the computed version into `Cargo.toml` and the
`speq` entry of `Cargo.lock` before building (`scripts/stamp_version.py`, not committed), so `speq --version` and the
manifest always show the released version. Local builds report the placeholder.

Before merging to `main`: the PR is green (fmt, clippy, tests on macOS/Linux-musl/Windows, MSRV, `cargo deny`, gitleaks,
installers), dependency changes were reviewed (`../dependency-review.md`), and the repository is **public** (installers
download anonymously). To suppress a release for a code change, use a non-releasing type (`chore:`/`docs:`).

## What the workflow does

1. **plan** — runs the release-script tests, computes `release`/`version`/`tag`; stops here when nothing is releasable or the
   tag already exists. Releases are serialised (`concurrency: release`) so two quick pushes never collide.
2. **verify** — fmt, clippy, deny, installer lint.
3. **build** (macOS ARM64, macOS Intel, Linux x86_64 musl, Windows x86_64 MSVC) — stamps the version, `cargo test`, release build
   with `SPEQ_COMMIT=<sha>`, smoke test `speq --version` (checks version, commit, target), package.
4. **publish** — `SHA256SUMS`, `manifest.json`, rendered Homebrew/Scoop/WinGet files, `install.sh`, `install.ps1`; creates a
   **draft** release, uploads everything, then publishes (`--latest`). The tag is created at that last step on the built commit,
   so a failed build leaves no tag, and users never see a partial release.
5. **verify-install** — installs the published release with the real installers on Linux, macOS (both archs), and Windows, then
   runs `--version`, `help`, `upgrade --check`.

A `workflow_dispatch` run re-evaluates the same rules on the current `main`; it cannot force a version.

Release assets (per version `V`, target `T`):

| Asset | Purpose |
| --- | --- |
| `speq-V-T[.exe]` | bare binary: what the installers and `speq upgrade` download |
| `speq-V-T.tar.gz` / `.zip` | archive for manual install and package managers |
| `SHA256SUMS` | checksums of every file; the installers find the right line from it |
| `manifest.json` | versioned manifest read by `speq upgrade` (version, commit, per-target URL/SHA-256/size) |
| `install.sh`, `install.ps1` | installers |
| `speq.rb`, `speq.scoop.json`, `Dayatech.Speq.yaml` | package-manager metadata with fixed-version URLs |

Signing: no release signing key exists yet, so integrity rests on HTTPS plus SHA-256. When a key exists, sign
`manifest.json`, embed the public key in the binary, and verify in `upgrade` and both installers.

## After release

* Homebrew: copy `speq.rb` into the tap repository (`brew install <tap>/speq`).
* Scoop / WinGet: submit `speq.scoop.json` / `Dayatech.Speq.yaml` to the bucket or `microsoft/winget-pkgs`.
* Pin/downgrade check: `speq upgrade --version <old> --yes`.

## Rollback

Releases are immutable by convention. To withdraw a bad release: mark it as pre-release or delete it on GitHub
(`latest` then points at the previous stable release; delete the tag too if the version number must be reused, otherwise the next
`fix:` commit publishes a fixed patch version). Installed users move with
`speq upgrade`; pinned users stay until they choose to move.
