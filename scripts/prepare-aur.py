#!/usr/bin/env python3
# SPDX-License-Identifier: MIT

import argparse
import hashlib
import json
import re
import shutil
import tarfile
import tomllib
from pathlib import Path

PACKAGING = Path(__file__).resolve().parents[1] / "packaging" / "aur"
PACKAGE = "arknights-launcher"
VERSION = r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}"


def package_version(archive: Path, root: str) -> str:
    with tarfile.open(archive, "r:gz") as source:
        versions = {}
        for name in ("package.json", "src-tauri/Cargo.toml", "src-tauri/tauri.conf.json"):
            member = source.getmember(f"{root}/{name}")
            if not member.isfile():
                raise ValueError(f"{name} is not a regular file")
            data = source.extractfile(member).read()
            if name.endswith(".toml"):
                versions[name] = tomllib.loads(data.decode())["package"]["version"]
            else:
                versions[name] = json.loads(data)["version"]
    version = versions["package.json"]
    if not isinstance(version, str) or not re.fullmatch(VERSION, version) or any(
        value != version for value in versions.values()
    ):
        raise ValueError(f"expected matching numeric X.Y.Z app versions, got {versions}")
    return version


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare an AUR recipe for a pinned source archive.")
    parser.add_argument("commit", help="full source commit SHA")
    parser.add_argument("directory", type=Path, help="directory containing the commit archive")
    parser.add_argument("--release-version", help="require the app version to match the release tag")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
        parser.error("expected a full Git commit SHA")

    root = f"{PACKAGE}-{args.commit}"
    archive = args.directory / f"{root}.tar.gz"
    desktop = PACKAGING / f"{PACKAGE}.desktop"
    try:
        version = package_version(archive, root)
        if args.release_version is not None and args.release_version != version:
            raise ValueError(f"release tag v{args.release_version} does not match app version {version}")
        replacements = {
            "VERSION": version,
            "COMMIT": args.commit,
            "ARCHIVE_SHA256": sha256(archive),
            "DESKTOP_SHA256": sha256(desktop),
        }
        recipe = (PACKAGING / "PKGBUILD.in").read_text(encoding="utf-8")
        recipe = re.sub(r"@([A-Z0-9_]+)@", lambda match: replacements[match[1]], recipe)
        shutil.copyfile(desktop, args.directory / desktop.name)
        (args.directory / "PKGBUILD").write_text(recipe, encoding="utf-8")
    except (OSError, ValueError, KeyError, tarfile.TarError) as error:
        parser.exit(1, f"prepare-aur: {error}\n")
    print(f"Prepared {PACKAGE} {version}-1 from {args.commit}")


if __name__ == "__main__":
    main()
