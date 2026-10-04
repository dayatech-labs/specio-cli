#!/usr/bin/env python3
"""Tests for next_version.py and stamp_version.py. Run: python3 scripts/test_release_scripts.py"""
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import next_version  # noqa: E402


def git(repo, *args):
    subprocess.run(["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.test", *args], check=True, capture_output=True)


def repo_with(*steps):
    """steps: ('commit', message) or ('tag', name)."""
    d = tempfile.mkdtemp()
    git(d, "init", "-q", "-b", "main")
    for kind, value in steps:
        if kind == "commit":
            git(d, "commit", "-q", "--allow-empty", "-m", value)
        else:
            git(d, "tag", value)
    return d


def decide(*steps):
    version, _ = next_version.plan(repo_with(*steps))
    return None if version is None else ".".join(map(str, version))


class NextVersion(unittest.TestCase):
    def test_first_release_needs_a_releasable_commit(self):
        self.assertEqual(decide(("commit", "feat: first")), "0.1.0")
        self.assertEqual(decide(("commit", "fix: first")), "0.1.0")
        self.assertIsNone(decide(("commit", "docs: readme"), ("commit", "chore: tidy")))

    def test_bumps_follow_the_commit_types(self):
        base = [("commit", "feat: x"), ("tag", "v1.2.3")]
        self.assertEqual(decide(*base, ("commit", "fix: bug")), "1.2.4")
        self.assertEqual(decide(*base, ("commit", "feat(api): more")), "1.3.0")
        self.assertEqual(decide(*base, ("commit", "fix: a"), ("commit", "feat: b")), "1.3.0")
        self.assertEqual(decide(*base, ("commit", "feat!: drop it")), "2.0.0")
        self.assertEqual(decide(*base, ("commit", "fix: a\n\nBREAKING CHANGE: gone")), "2.0.0")
        self.assertEqual(decide(*base, ("commit", "refactor: tidy")), "1.2.4")

    def test_no_release_for_non_code_commits_or_nothing_new(self):
        base = [("commit", "feat: x"), ("tag", "v1.2.3")]
        self.assertIsNone(decide(*base))
        self.assertIsNone(decide(*base, ("commit", "docs: d"), ("commit", "ci: c"), ("commit", "test: t"), ("commit", "chore: k")))
        self.assertIsNone(decide(*base, ("commit", "update stuff")))

    def test_breaking_before_1_0_bumps_minor_and_latest_tag_wins(self):
        self.assertEqual(decide(("commit", "feat: x"), ("tag", "v0.4.0"), ("commit", "feat!: y")), "0.5.0")
        self.assertEqual(decide(("commit", "feat: x"), ("tag", "v0.9.0"), ("tag", "v0.10.0"), ("commit", "fix: z")), "0.10.1")


class Stamp(unittest.TestCase):
    def test_stamps_cargo_toml_and_lock_together(self):
        d = pathlib.Path(tempfile.mkdtemp())
        (d / "Cargo.toml").write_text('[package]\nname = "specio"\nversion = "0.0.0"\n\n[dependencies]\nclap = { version = "4.6.7" }\n')
        (d / "Cargo.lock").write_text('[[package]]\nname = "clap"\nversion = "4.6.7"\n\n[[package]]\nname = "specio"\nversion = "0.0.0"\ndependencies = [\n "clap",\n]\n')
        subprocess.run([sys.executable, str(HERE / "stamp_version.py"), "2.5.1"], check=True, cwd=d)
        self.assertIn('version = "2.5.1"', (d / "Cargo.toml").read_text())
        self.assertIn('version = "4.6.7"', (d / "Cargo.toml").read_text())
        lock = (d / "Cargo.lock").read_text()
        self.assertIn('name = "specio"\nversion = "2.5.1"', lock)
        self.assertIn('name = "clap"\nversion = "4.6.7"', lock)
        bad = subprocess.run([sys.executable, str(HERE / "stamp_version.py"), "1.2"], cwd=d)
        self.assertNotEqual(bad.returncode, 0)


if __name__ == "__main__":
    unittest.main()
