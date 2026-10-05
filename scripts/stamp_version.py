#!/usr/bin/env python3
"""Stamp the release version into Cargo.toml and the `speq` entry of Cargo.lock (CI only, not committed).

The version comes from the commit history (scripts/next_version.py), so the checked-in 0.x value is only a
placeholder. `cargo build --locked` stays valid because both files are changed together.
Usage: stamp_version.py <x.y.z>
"""
import pathlib
import re
import sys

version = sys.argv[1]
if not re.fullmatch(r"\d+\.\d+\.\d+", version):
    sys.exit(f"invalid version: {version}")

toml = pathlib.Path("Cargo.toml")
text, count = re.subn(r'(?m)^(version\s*=\s*)"[^"]*"', rf'\g<1>"{version}"', toml.read_text(encoding="utf-8"), count=1)
if count != 1:
    sys.exit("no version in Cargo.toml")
toml.write_text(text, encoding="utf-8")

lock = pathlib.Path("Cargo.lock")
text, count = re.subn(r'(\[\[package\]\]\nname = "speq"\nversion = )"[^"]*"', rf'\g<1>"{version}"', lock.read_text(encoding="utf-8"), count=1)
if count != 1:
    sys.exit("no speq entry in Cargo.lock")
lock.write_text(text, encoding="utf-8")
