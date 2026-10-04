#!/usr/bin/env python3
"""Decide whether the commits since the last release tag warrant a release, and which version.

Conventional-commit subjects (`<type>[(scope)][!]: <summary>`, see CLAUDE.md) drive the bump:

  breaking (`!` or a `BREAKING CHANGE` footer)  -> major   (minor while the version is 0.x)
  feat                                          -> minor
  fix, perf, refactor                           -> patch
  docs, chore, test, ci, build, style, revert   -> no release on their own

Prints `release=true|false`, `version=X.Y.Z`, `tag=vX.Y.Z` lines (GITHUB_OUTPUT format).
Usage: next_version.py [--repo DIR]
"""
import re
import subprocess
import sys

FIRST_VERSION = (0, 1, 0)
SUBJECT = re.compile(r"^(?P<type>[a-z]+)(?:\([^)]*\))?(?P<bang>!)?:\s+\S")
PATCH_TYPES = {"fix", "perf", "refactor"}


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True, text=True).stdout


def last_tag(repo):
    tags = [t for t in git(repo, "tag", "--merged", "HEAD", "--list", "v[0-9]*.[0-9]*.[0-9]*").split() if re.fullmatch(r"v\d+\.\d+\.\d+", t)]
    return max(tags, key=lambda t: tuple(map(int, t[1:].split(".")))) if tags else None


def classify(commits):
    """commits: list of (subject, body). Returns 'major' | 'minor' | 'patch' | None."""
    level = None
    for subject, body in commits:
        match = SUBJECT.match(subject)
        if not match:
            continue
        kind = match.group("type")
        if match.group("bang") or re.search(r"^BREAKING[ -]CHANGE:", body, re.M):
            return "major"
        if kind == "feat":
            level = "minor"
        elif kind in PATCH_TYPES and level is None:
            level = "patch"
    return level


def bump(version, level):
    major, minor, patch = version
    if level == "major":
        # Before 1.0, breaking changes bump the minor version.
        return (0, minor + 1, 0) if major == 0 else (major + 1, 0, 0)
    if level == "minor":
        return (major, minor + 1, 0)
    return (major, minor, patch + 1)


def plan(repo):
    tag = last_tag(repo)
    rev = f"{tag}..HEAD" if tag else "HEAD"
    raw = git(repo, "log", rev, "--format=%s%x1f%b%x1e")
    commits = [tuple(part.split("\x1f", 1)) for part in raw.split("\x1e") if part.strip()]
    commits = [(s.strip(), b) for s, b in commits]
    level = classify(commits)
    if tag is None:
        # No release yet: the first releasable commit publishes the first version.
        return (FIRST_VERSION if level else None), None
    if level is None:
        return None, tag
    return bump(tuple(map(int, tag[1:].split("."))), level), tag


def main():
    repo = sys.argv[sys.argv.index("--repo") + 1] if "--repo" in sys.argv else "."
    version, _ = plan(repo)
    if version is None:
        print("release=false")
    else:
        text = ".".join(map(str, version))
        print("release=true")
        print(f"version={text}")
        print(f"tag=v{text}")


if __name__ == "__main__":
    main()
