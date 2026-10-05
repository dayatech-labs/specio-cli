#!/usr/bin/env python3
"""Build SHA256SUMS and manifest.json for a release from the raw binaries in a directory.

Usage: release_manifest.py <dir> <version> <commit> <base-download-url>

The directory holds, per target, `speq-<version>-<target>[.exe]` (raw binary) and the matching
archive (`.tar.gz` / `.zip`). SHA256SUMS lists every file; manifest.json lists the raw binaries,
which is what the installers and `speq upgrade` download.
"""
import hashlib
import json
import pathlib
import sys

TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
]


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    directory, version, commit, base = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4].rstrip("/")
    artifacts = {}
    for target in TARGETS:
        suffix = ".exe" if "windows" in target else ""
        binary = directory / f"speq-{version}-{target}{suffix}"
        if not binary.is_file():
            sys.exit(f"missing binary for {target}: {binary.name}")
        artifacts[target] = {
            "binary": {"url": f"{base}/{binary.name}", "sha256": sha256(binary), "size": binary.stat().st_size},
        }

    files = sorted(p for p in directory.iterdir() if p.is_file() and p.name.startswith(f"speq-{version}-"))
    (directory / "SHA256SUMS").write_text("".join(f"{sha256(p)}  {p.name}\n" for p in files), encoding="utf-8")
    manifest = {"schema": 1, "version": version, "commit": commit, "artifacts": artifacts}
    (directory / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
