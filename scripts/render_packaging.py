#!/usr/bin/env python3
"""Render the Homebrew formula and Scoop/WinGet manifests from templates.

Usage: render_packaging.py <release-dir> <version> <out-dir>

URLs point at the fixed `download/v<version>/` release assets (never `latest`) and the checksums
are the ones in SHA256SUMS, so package-manager metadata and the release always agree.
"""
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent / "packaging"
RELEASE_URL = "https://github.com/dayatech-labs/speq-cli/releases/download"


def sums(directory: pathlib.Path) -> dict:
    table = {}
    for line in (directory / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        digest, name = line.split(None, 1)
        table[name.strip()] = digest
    return table


def main() -> int:
    directory, version, out = pathlib.Path(sys.argv[1]), sys.argv[2], pathlib.Path(sys.argv[3])
    table = sums(directory)
    out.mkdir(parents=True, exist_ok=True)

    def archive(target: str, ext: str) -> tuple:
        name = f"speq-{version}-{target}.{ext}"
        return f"{RELEASE_URL}/v{version}/{name}", table[name]

    values = {"VERSION": version}
    for key, target, ext in [
        ("MACOS_ARM", "aarch64-apple-darwin", "tar.gz"),
        ("MACOS_INTEL", "x86_64-apple-darwin", "tar.gz"),
        ("LINUX", "x86_64-unknown-linux-musl", "tar.gz"),
        ("WINDOWS", "x86_64-pc-windows-msvc", "zip"),
    ]:
        url, digest = archive(target, ext)
        values[f"{key}_URL"], values[f"{key}_SHA256"] = url, digest

    for template, destination in [
        (ROOT / "homebrew" / "speq.rb.template", out / "speq.rb"),
        (ROOT / "windows" / "scoop.json.template", out / "speq.scoop.json"),
        (ROOT / "windows" / "winget.yaml.template", out / "Dayatech.Speq.yaml"),
    ]:
        text = template.read_text(encoding="utf-8")
        for key, value in values.items():
            text = text.replace("@" + key + "@", value)
        if "@" in text and any(f"@{k}@" in text for k in values):
            sys.exit(f"unrendered placeholder in {template.name}")
        destination.write_text(text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
