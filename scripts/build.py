#!/usr/bin/env python3
"""Build portable VTCode packages from the current checkout on Windows or Linux."""

from __future__ import annotations

import argparse
from pathlib import Path
import shlex
import subprocess
import sys

if sys.version_info < (3, 11):
    sys.exit("ERROR: Python 3.11 or newer is required to build VTCode.")

import tomllib

import local


ROOT = Path(__file__).resolve().parents[1]


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog="Builds run the release checks unless --skip-tests is given and allow uncommitted changes. "
               "They never commit, push, tag, or upload anything.",
    )
    parser.add_argument("--target", choices=("windows", "linux", "all"),
                        default="windows" if sys.platform == "win32" else "linux",
                        help="Platform(s) to build; defaults to the current platform")
    parser.add_argument("--glibc", choices=("both", *local.linux_abi.BASELINES),
                        help="Linux version to build: 2.28, 2.43, or both (default)")
    parser.add_argument("--output-dir", type=Path,
                        help="Output root; packages go in <root>/preview/v<VERSION> (default: repo/dist)")
    parser.add_argument("--wsl-distro", default="Ubuntu",
                        help="WSL distribution for Linux builds started on Windows (default: Ubuntu)")
    parser.add_argument("--engine", choices=("auto", "docker", "podman"), default="auto",
                        help="Linux container engine; auto prefers Docker, then Podman")
    tests = parser.add_mutually_exclusive_group()
    tests.add_argument("--reuse-tests", action="store_true",
                       help="Reuse a complete test report only if sources, compiler, and baseline still match")
    tests.add_argument("--skip-tests", action="store_true",
                       help="Compile and package without running tests or installing test dependencies")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show the build commands and output path without running or installing anything")
    args = parser.parse_args(argv)
    if sys.platform not in ("win32", "linux"):
        parser.error("Builds require Windows or x86_64 Linux (including WSL).")
    if args.target in ("windows", "all") and sys.platform != "win32":
        parser.error("Windows builds require Windows with the MSVC build tools; use --target linux here.")
    if args.target == "windows" and (args.glibc is not None or args.engine != "auto"):
        parser.error("--glibc and --engine require --target linux or --target all.")
    args.output_dir = args.output_dir.expanduser().resolve() if args.output_dir else ROOT / "dist"
    return args


def build_commands(args):
    """Keep one release implementation for the convenience scripts and CI."""
    command = [sys.executable, str(ROOT / "scripts" / "local.py")]
    common = ["--preview", "--output-dir", str(args.output_dir)]
    if args.reuse_tests:
        common.append("--reuse-tests")
    if args.skip_tests:
        common.append("--skip-tests")
    commands = []
    if args.target in ("windows", "all"):
        if not args.skip_tests:
            commands.append([*command, "setup"])
        commands.append([*command, "release", *common])
    if args.target in ("linux", "all"):
        options = ["--engine", args.engine]
        if sys.platform == "win32":
            options.extend(["--wsl-distro", args.wsl_distro])
        if args.glibc not in (None, "both"):
            options.extend(["--glibc", args.glibc])
        commands.append([*command, "linux-release", *options, *common])
    return commands


def main(argv=None):
    args = parse_args(argv)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    output = args.output_dir / "preview" / f"v{version}"
    commands = build_commands(args)
    if args.dry_run:
        for command in commands:
            rendered = subprocess.list2cmdline(command) if sys.platform == "win32" else shlex.join(command)
            print("+ " + rendered)
        print(f"Planned output: {output}")
        return
    local.resolve_output_dir(args.output_dir)
    state = local.source_state()
    for command in commands:
        local.run(command)
        if local.source_state() != state:
            raise local.Failure("Sources changed between build steps. Rerun to build all targets from the same sources.")
    print(f"Builds complete: {output}")


if __name__ == "__main__":
    local.configure_output()
    try:
        main()
    except (local.Failure, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        print("Build cancelled.", file=sys.stderr)
        sys.exit(130)
