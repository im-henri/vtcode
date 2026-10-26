"""Freeze tracked Rust source blobs from a Git revision for performance replays.

The destination must not exist. Git blobs preserve exact bytes independently
of working-tree edits or platform newline conversion. No worktree is changed.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import subprocess


def git(repo, *args, **kwargs):
    return subprocess.run(["git", "-C", str(repo), *args], check=True,
                          stdout=subprocess.PIPE, **kwargs).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    args = parser.parse_args()
    if args.output.exists():
        parser.error("destination already exists; choose a new directory")
    revision = git(args.repo, "rev-parse", "--verify", args.revision + "^{commit}").decode().strip()
    tree = git(args.repo, "ls-tree", "-r", "-z", revision, "--", "src")
    objects = []
    for record in tree.split(b"\0"):
        if not record:
            continue
        metadata, path = record.split(b"\t", 1)
        mode, kind, oid = metadata.split()
        name = path.decode("utf-8")
        if kind == b"blob" and mode in (b"100644", b"100755") and name.endswith(".rs"):
            relative = Path(name)
            if relative.is_absolute() or ".." in relative.parts or relative.parts[0] != "src":
                raise ValueError(f"unexpected source path: {name}")
            objects.append((name, oid))
    objects.sort()
    data = io.BytesIO(git(args.repo, "cat-file", "--batch",
                          input=b"\n".join(oid for _, oid in objects) + b"\n"))
    args.output.mkdir(parents=True)
    manifest = {"revision": revision, "representation": "exact Git blobs", "files": []}
    for name, oid in objects:
        actual, kind, size = data.readline().split()
        if actual != oid or kind != b"blob":
            raise ValueError(f"unexpected Git object for {name}")
        blob = data.read(int(size))
        if len(blob) != int(size) or data.read(1) != b"\n":
            raise ValueError(f"truncated Git object for {name}")
        destination = args.output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(blob)
        manifest["files"].append({"path": name, "bytes": len(blob),
                                  "sha256": hashlib.sha256(blob).hexdigest()})
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Frozen {len(objects)} Rust files, {sum(f['bytes'] for f in manifest['files']):,} bytes, {revision}")


if __name__ == "__main__":
    main()
