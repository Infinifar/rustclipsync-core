#!/usr/bin/env python3
"""Export a reproducible library snapshot; never maintain consumer copies by hand."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    # Reject symbolic destination components before resolving them.
    raw = args.destination.absolute()
    if any(p.is_symlink() for p in [raw, *raw.parents]):
        parser.error("destination must not contain symlinks")
    dest = raw.resolve()
    if dest == root or dest in root.parents or root in dest.parents:
        parser.error("destination must be outside the source project")
    paths = [root / n for n in ("Cargo.toml", "LICENSE", "README.md", "VALIDATION.md")]
    paths += sorted((root / "src").rglob("*.rs"))
    files = {p.relative_to(root).as_posix(): p.read_bytes() for p in paths}
    manifest = {
        "source": "rustclipsync-core",
        "version": tomllib.loads(files["Cargo.toml"].decode())["package"]["version"],
        "extracted_from": "Infinifar/omaclipsync@a8c330136886ad547dfadf386bb66901b3bbc45c",
        "files": {name: hashlib.sha256(data).hexdigest() for name, data in sorted(files.items())},
    }
    files["UPSTREAM.json"] = (json.dumps(manifest, indent=2) + "\n").encode()
    actual = {}
    if dest.exists():
        for path in dest.rglob("*"):
            if path.is_symlink():
                parser.error("snapshot contains a symlink")
            if path.is_file():
                actual[path.relative_to(dest).as_posix()] = path.read_bytes()
    if args.check:
        if files != actual:
            parser.exit(1, "Shared core snapshot differs; regenerate it from rustclipsync-core.\n")
        print("Shared core snapshot matches source (all files and SHA-256 values).")
        return
    if actual and ("UPSTREAM.json" not in actual or json.loads(actual["UPSTREAM.json"]).get("source") != "rustclipsync-core"):
        parser.error("refusing to replace a directory not marked as our generated snapshot")
    # Only an explicitly marked generated directory may be replaced.
    if dest.exists():
        shutil.rmtree(dest)
    for name, data in files.items():
        target = dest / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    print(f"Exported rustclipsync-core {manifest['version']} to {dest}")


if __name__ == "__main__":
    main()
