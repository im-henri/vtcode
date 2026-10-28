#!/usr/bin/env python3
"""Local checks and portable releases. Python 3.11+; Linux releases use containers."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import zipfile

import linux_abi
import linux_release


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"win32": "x86_64-pc-windows-msvc", "linux": "x86_64-unknown-linux-gnu"}
ACP_FILTERS = ("acp_chat_sdk_", "close_agent_sdk", "agent_settings_official_echo", "chat_files_official_sdk")


class Failure(Exception):
    pass


def configure_output():
    # PowerShell redirects can otherwise leave Python using the Windows ANSI code page.
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(encoding="utf-8", errors="replace")


def run(args, *, env=None, cwd=ROOT, capture=False, log=None):
    args = [str(arg) for arg in args]
    print("+ " + shlex.join(args), flush=True)
    if log is None:
        result = subprocess.run(args, cwd=cwd, env=env, text=True,
                                encoding="utf-8", errors="replace", capture_output=capture)
        if result.returncode and capture:
            print(result.stdout, file=sys.stderr)
            print(result.stderr, file=sys.stderr)
        result.check_returncode()
        return result.stdout.strip() if capture else None
    with subprocess.Popen(args, cwd=cwd, env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace") as process:
        for line in process.stdout:
            print(line, end="", flush=True)
            log.write(line)
        if process.wait():
            raise subprocess.CalledProcessError(process.returncode, args)


def digest(stream):
    return hashlib.file_digest(stream, "sha256").hexdigest()


def file_digest(path):
    with path.open("rb") as stream:
        return digest(stream)


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")


def source_state():
    revision = run(["git", "rev-parse", "HEAD"], capture=True)
    dirty = bool(run(["git", "status", "--porcelain", "--untracked-files=all"], capture=True))
    paths = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=ROOT
    ).split(b"\0")
    fingerprint = hashlib.sha256()
    for name in sorted(set(paths) - {b""}):
        path = ROOT / os.fsdecode(name)
        fingerprint.update(name + b"\0")
        fingerprint.update((file_digest(path) if path.is_file() else "missing").encode() + b"\0")
    return {"sourceRevision": revision, "sourceDirty": dirty, "sourceFingerprint": fingerprint.hexdigest()}


def environment(glibc=None):
    env = os.environ.copy()
    env.setdefault("CARGO_BUILD_JOBS", "2")
    env.setdefault("RUST_TEST_THREADS", "2")
    env.update(PYTHONUTF8="1", PYTHONIOENCODING="utf-8")
    # A WSL checkout can share sources with Windows without sharing native objects.
    if sys.platform == "linux":
        env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target" / "local-linux"))
    env.pop("VTCODE_GLIBC_BASELINE", None)
    if glibc:
        if sys.platform != "linux":
            raise Failure("--glibc is only valid for native Linux checks and releases.")
        actual = run(["getconf", "GNU_LIBC_VERSION"], capture=True)
        if actual != f"glibc {glibc}":
            raise Failure(f"Requested glibc {glibc}, but this environment has {actual}. Use linux-release.")
        env["VTCODE_GLIBC_BASELINE"] = glibc
        env["CARGO_TARGET_DIR"] = str(Path(env["CARGO_TARGET_DIR"]) / f"glibc-{glibc}")
        env.update(LIBGIT2_NO_PKG_CONFIG="1", LIBZ_SYS_STATIC="1", PCRE2_SYS_STATIC="1")
    return env


def python_path(env=None):
    env = os.environ if env is None else env
    override = env.get("VTCODE_ACP_TEST_PYTHON")
    if override:
        # Resolving a Unix venv's python symlink would select the base interpreter
        # and lose the packages installed in that venv.
        return Path(override).expanduser().absolute()
    folder = ROOT / ".venv" / f"local-ci-{sys.platform}"
    if env.get("VTCODE_GLIBC_BASELINE"):
        folder = folder.with_name(f"{folder.name}-glibc-{env['VTCODE_GLIBC_BASELINE']}")
    return folder / ("Scripts/python.exe" if sys.platform == "win32" else "bin/python")


def setup(glibc=None):
    env = environment(glibc)
    channel = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    installed = run(["rustup", "toolchain", "list"], capture=True)
    components = ["clippy", "rustfmt", "rust-analyzer", "rust-src"]
    if not any(line.startswith(channel + "-") for line in installed.splitlines()):
        run(["rustup", "toolchain", "install", channel, "--profile", "minimal",
             *[arg for component in components for arg in ("--component", component)]])
    else:
        installed_components = run(["rustup", "component", "list", "--installed"], capture=True)
        missing = [name for name in components if not any(
            line == name or line.startswith(name + "-") for line in installed_components.splitlines())]
        if missing:
            run(["rustup", "component", "add", *missing])
    python = python_path(env)
    if not python.is_file():
        if os.environ.get("VTCODE_ACP_TEST_PYTHON"):
            raise Failure(f"VTCODE_ACP_TEST_PYTHON does not exist: {python}")
        run([sys.executable, "-m", "venv", python.parent.parent])
    run([python, "-m", "pip", "install", "-r", ROOT / "tests/fixtures/requirements.txt"], env=env)
    print("Setup complete. Run: python scripts/local.py test")


def context(glibc=None):
    if sys.platform not in TARGETS:
        raise Failure("Use native x86_64 Windows (MSVC) or Linux (including WSL).")
    env = environment(glibc)
    rustc = run(["rustc", "-vV"], env=env, capture=True)
    target = re.search(r"(?m)^host: (.+)$", rustc).group(1)
    if target != TARGETS[sys.platform] or env.get("CARGO_BUILD_TARGET", target) != target:
        raise Failure(f"Build natively for {TARGETS[sys.platform]}; detected {target}.")
    metadata = json.loads(run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"], env=env, capture=True))
    package = next(package for package in metadata["packages"] if package["name"] == "vtcode")
    return env, target, package["version"], Path(metadata["target_directory"]), rustc


def check_commands(target):
    commands = [
        ["python", "-m", "unittest", "discover", "-s", "scripts/tests"],
        ["cargo", "test", "--locked"],
        ["cargo", "test", "--manifest-path", "extension-api/Cargo.toml"],
        ["cargo", "clippy", "--locked", "--all-targets"],
        ["cargo", "test", "--locked", "--test", "acp_integration", "--", "--ignored", "--test-threads=1"],
    ]
    commands.extend(["cargo", "test", "--locked", "--bin", "vtcode", name,
                     "--", "--ignored", "--test-threads=1"] for name in ACP_FILTERS)
    if target == TARGETS["linux"]:
        commands.append(["cargo", "test", "--locked", "--bin", "vtcode",
                         "rust_analyzer_linux_feature_round_trip", "--", "--ignored", "--test-threads=1"])
    return commands


def check_python(env):
    python = python_path(env)
    if not python.is_file():
        raise Failure("ACP test Python is missing. Run: python scripts/local.py setup")
    code = ("import importlib.metadata as m, json; "
            "print(json.dumps({d.metadata['Name'].lower().replace('_', '-'): d.version for d in m.distributions()}))")
    installed = json.loads(run([python, "-c", code], env=env, capture=True))
    for line in (ROOT / "tests/fixtures/requirements.txt").read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            name, version = line.split("==")
            if installed.get(name) != version:
                raise Failure(f"ACP tests require {line}. Run: python scripts/local.py setup")
    env["VTCODE_ACP_TEST_PYTHON"] = str(python)


def test(ctx):
    env, target, _, build, rustc = ctx
    check_python(env)
    report_dir = build / "local-ci" / target
    report_dir.mkdir(parents=True, exist_ok=True)
    report_path = report_dir / "validation.json"
    report_path.unlink(missing_ok=True)
    state = source_state()
    report = {"formatVersion": 1, "target": target, "rustc": rustc, **state, "passed": False, "checks": []}
    if env.get("VTCODE_GLIBC_BASELINE"):
        report["glibcBaseline"] = env["VTCODE_GLIBC_BASELINE"]
    write_json(report_path, report)
    with (report_dir / "test.log").open("w", encoding="utf-8") as log:
        for command in check_commands(target):
            started = time.monotonic()
            executable_command = [sys.executable, *command[1:]] if command[0] == "python" else command
            run(executable_command, env=env, log=log)
            report["checks"].append({"command": command, "exitCode": 0,
                                     "seconds": round(time.monotonic() - started, 2)})
            write_json(report_path, report)
    if source_state() != state:
        raise Failure("Sources changed during testing. Run the checks again.")
    report["passed"] = True
    write_json(report_path, report)
    print(f"All checks passed. Report: {report_path}")
    return report


def reuse_tests(ctx):
    env, target, _, build, rustc = ctx
    check_python(env)
    path = build / "local-ci" / target / "validation.json"
    if not path.is_file():
        raise Failure("No test report. Run test first, or release without --reuse-tests.")
    report = json.loads(path.read_text(encoding="utf-8"))
    if (not report.get("passed") or report.get("target") != target or report.get("rustc") != rustc
            or report.get("glibcBaseline") != env.get("VTCODE_GLIBC_BASELINE")
            or any(report.get(key) != value for key, value in source_state().items())
            or [check["command"] for check in report["checks"]] != check_commands(target)
            or any(check.get("exitCode") != 0 for check in report["checks"])):
        raise Failure("Test report is stale or incomplete. Run test again, or omit --reuse-tests.")
    return report


def release_environment(env):
    env = env.copy()
    mappings = [(str(Path.home()), "/build/user"), (str(ROOT), "/build/vtcode")]
    flags = (env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") if env.get("CARGO_ENCODED_RUSTFLAGS")
             else env.get("RUSTFLAGS", "").split())
    native = ["/experimental:deterministic"] if sys.platform == "win32" else []
    for directory, replacement in mappings:
        for prefix in dict.fromkeys((directory, directory.replace("\\", "/"))):
            flags.append(f"--remap-path-prefix={prefix}={replacement}")
            native.append(f"/pathmap:{prefix}={replacement}" if sys.platform == "win32"
                          else f"-ffile-prefix-map={prefix}={replacement}")
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    env["CC_SHELL_ESCAPED_FLAGS"] = "1"
    target = TARGETS[sys.platform].replace("-", "_")
    for name in ("CFLAGS", "CXXFLAGS"):
        key = f"{name}_{target}"
        env[key] = " ".join(filter(None, [env.get(key, env.get(name, "")), shlex.join(native)]))
    return env


def copy_assets(package):
    for asset in ("grammars", "themes"):
        for source in sorted((ROOT / asset).rglob("*")):
            relative = source.relative_to(ROOT)
            if any(part in (".cache", "__pycache__", ".DS_Store") for part in relative.parts):
                continue
            if source.is_file():
                destination = package / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, destination)
    for relative in ("README.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "vendor/crossterm/LICENSE"):
        destination = package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / relative, destination)


def bundled_grammar_paths(data):
    try:
        index = json.loads(data)
        grammars = index["grammars"]
        if index["version"] != 1 or not isinstance(grammars, dict) or not grammars:
            raise ValueError("invalid catalog")
        if any(not isinstance(name, str) or not name or "/" in name or "\\" in name
               or name in (".", "..") for name in grammars):
            raise ValueError("invalid grammar name")
        return {f"grammars/bundled/{name}.json" for name in grammars}
    except (KeyError, TypeError, ValueError) as error:
        raise Failure("Missing or invalid bundled grammar catalog.") from error


def check_payload(package, binary):
    if len(list((package / "grammars").rglob("*.tmLanguage.json"))) < 3:
        raise Failure("Missing shipped grammars.")
    if not list((package / "themes").glob("*.json")):
        raise Failure("Missing shipped themes.")
    try:
        bundled = bundled_grammar_paths((package / "grammars/bundled/index.json").read_bytes())
    except OSError as error:
        raise Failure("Missing bundled grammar catalog.") from error
    if any(not (package / name).is_file() or (package / name).stat().st_size == 0 for name in bundled):
        raise Failure("Missing bundled grammar source.")
    for relative in (binary, "grammars/.cache/textmate-registry.json.gz", "grammars/.cache/source.fingerprint"):
        data = (package / relative).read_bytes()
        if not data:
            raise Failure(f"Empty payload: {relative}")
        for directory in (str(ROOT), str(Path.home())):
            for prefix in (directory, directory.replace("\\", "/")):
                for encoding in ("utf-8", "utf-16-le"):
                    if prefix.encode(encoding) in data:
                        raise Failure(f"Build-machine path in {relative}.")


def verify_archive(archive, *, allow_preview=False):
    checksum = Path(str(archive) + ".sha256").read_text(encoding="utf-8").strip()
    if checksum != f"{file_digest(archive)}  {archive.name}":
        raise Failure(f"Archive checksum mismatch: {archive.name}")
    if archive.suffix == ".zip":
        container = zipfile.ZipFile(archive)
        members = [(entry.filename, entry.file_size, entry.external_attr >> 16, entry)
                   for entry in container.infolist() if not entry.is_dir()]
        open_member = container.open
    else:
        container = tarfile.open(archive, "r:gz")
        members = []
        for entry in container.getmembers():
            if not entry.isfile() and not entry.isdir():
                container.close()
                raise Failure(f"Unsupported archive entry: {entry.name}")
            if entry.isfile():
                members.append((entry.name, entry.size, entry.mode, entry))
        open_member = container.extractfile
    with container:
        names = [name for name, _, _, _ in members]
        if len(names) != len(set(names)):
            raise Failure("Duplicate archive paths.")
        roots = {PurePosixPath(name).parts[0] for name in names}
        if len(roots) != 1 or any(".." in PurePosixPath(name).parts or "\\" in name
                                  or PurePosixPath(name).is_absolute() for name in names):
            raise Failure("Unsafe archive paths.")
        root = roots.pop()
        entries = {name.removeprefix(root + "/"): (size, mode, entry) for name, size, mode, entry in members}
        with open_member(entries["manifest.json"][2]) as stream:
            manifest = json.load(stream)
        target, version = manifest["target"], manifest["version"]
        if target not in TARGETS.values():
            raise Failure("Unsupported release target.")
        baseline = manifest.get("glibcBaseline")
        if target == TARGETS["linux"] and manifest.get("formatVersion") == 2:
            if baseline not in linux_abi.BASELINES:
                raise Failure("Missing or unsupported Linux glibc baseline.")
        elif baseline is not None:
            raise Failure("Unexpected glibc baseline in release manifest.")
        expected_root = archive_name(version, target, baseline)
        extension = ".zip" if target == TARGETS["win32"] else ".tar.gz"
        if root != expected_root or archive.name != expected_root + extension:
            raise Failure("Archive name does not match its manifest.")
        if (manifest.get("formatVersion") not in (1, 2) or manifest.get("product") != "VTCode"
                or manifest.get("portable") is not True or manifest.get("tag") != f"v{version}"
                or not re.fullmatch(r"[0-9a-f]{40}", manifest.get("sourceRevision", ""))
                or not re.fullmatch(r"[0-9a-f]{64}", manifest.get("sourceFingerprint", ""))):
            raise Failure("Invalid release manifest.")
        if not allow_preview and (manifest.get("preview") is not False or manifest.get("sourceDirty") is not False):
            raise Failure("Preview or uncommitted source: rebuild from a clean commit for publication.")
        files = {file["path"]: file for file in manifest["files"]}
        if len(files) != len(manifest["files"]) or set(entries) != set(files) | {"manifest.json"}:
            raise Failure("Archive payload does not match its manifest.")
        for name, file in files.items():
            size, _, entry = entries[name]
            with open_member(entry) as stream:
                if size != file["size"] or digest(stream) != file["sha256"]:
                    raise Failure(f"Archive payload mismatch: {name}")
        binary = "vtcode.exe" if target == TARGETS["win32"] else "vtcode"
        required = {binary, "settings.json", "validation.json", "LICENSE", "README.md",
                    "THIRD_PARTY_NOTICES.md", "vendor/crossterm/LICENSE",
                    "grammars/.cache/textmate-registry.json.gz", "grammars/.cache/source.fingerprint",
                    "grammars/bundled/index.json"}
        if (manifest.get("binary") != binary or not required <= set(files)
                or any(files[name]["size"] == 0 for name in required)
                or sum(name.startswith("grammars/") and name.endswith(".tmLanguage.json") for name in files) < 3
                or not any(name.startswith("themes/") and name.endswith(".json") for name in files)):
            raise Failure("Incomplete portable payload.")
        with open_member(entries["grammars/bundled/index.json"][2]) as stream:
            bundled = bundled_grammar_paths(stream.read())
        if not bundled <= set(files) or any(files[name]["size"] == 0 for name in bundled):
            raise Failure("Missing bundled grammar source.")
        if target == TARGETS["linux"] and not entries[binary][1] & 0o111:
            raise Failure("Linux binary is not executable.")
        if baseline:
            with open_member(entries[binary][2]) as stream:
                try:
                    abi = linux_abi.verify(stream.read(), baseline)
                except ValueError as error:
                    raise Failure(str(error)) from error
            if manifest.get("linuxAbi") != abi:
                raise Failure("Linux ABI manifest does not match the executable.")
        with open_member(entries["validation.json"][2]) as stream:
            report = json.load(stream)
        identity_matches = (report.get("target") == target and report.get("glibcBaseline") == baseline
                            and all(report.get(key) == manifest.get(key) for key in source_state_keys()))
        if manifest.get("testsSkipped") is True or report.get("testsSkipped") is True:
            validation_matches = (allow_preview and manifest.get("preview") is True
                                  and manifest.get("testsSkipped") is True and report.get("testsSkipped") is True
                                  and report.get("passed") is False and report.get("checks") == [])
        else:
            validation_matches = (manifest.get("testsSkipped", False) is False
                                  and report.get("testsSkipped", False) is False and report.get("passed") is True
                                  and [check.get("command") for check in report.get("checks", [])] == check_commands(target)
                                  and all(check.get("exitCode") == 0 for check in report["checks"]))
        if not identity_matches or not validation_matches:
            raise Failure("Missing or inconsistent validation report.")
    print(f"Verified: {archive}")
    return manifest


def source_state_keys():
    return ("sourceRevision", "sourceDirty", "sourceFingerprint")


def archive_name(version, target, baseline=None):
    suffix = f"-glibc-{baseline}" if baseline else ""
    return f"vtcode-{version}-{target}{suffix}-portable"


def resolve_output_dir(output_dir=None):
    output = Path(output_dir).expanduser().resolve() if output_dir is not None else ROOT / "dist"
    # Generated packages inside an unignored source directory would invalidate
    # source fingerprints partway through testing or packaging.
    if output.is_relative_to(ROOT):
        ignored = subprocess.run(["git", "check-ignore", "--quiet", "--", output.as_posix() + "/"], cwd=ROOT)
        if ignored.returncode != 0:
            raise Failure("The output directory inside the checkout must be Git-ignored. "
                          "Use dist/, target/, or a directory outside the checkout.")
    return output


def release(ctx, *, preview=False, reuse=False, output_dir=None, skip_tests=False):
    if reuse and skip_tests:
        raise Failure("--reuse-tests and --skip-tests cannot be combined.")
    preview = preview or skip_tests
    env, target, version, build, rustc = ctx
    baseline = env.get("VTCODE_GLIBC_BASELINE")
    if target == TARGETS["linux"] and baseline not in linux_abi.BASELINES:
        raise Failure("Linux releases need an explicit baseline. Use linux-release for both, "
                      "or release --glibc 2.28/2.43 in a matching environment.")
    output = resolve_output_dir(output_dir)
    state = source_state()
    if state["sourceDirty"] and not preview:
        raise Failure("Commit changes before creating release artifacts, or use --preview.")
    existing_tag = subprocess.run(["git", "rev-parse", "--verify", f"refs/tags/v{version}^{{commit}}"],
                                  cwd=ROOT, capture_output=True, text=True)
    if not preview and existing_tag.returncode == 0 and existing_tag.stdout.strip() != state["sourceRevision"]:
        raise Failure(f"v{version} already points to another commit. Bump Cargo.toml and Cargo.lock first.")
    if skip_tests:
        print("Tests skipped; compiling and packaging only.", flush=True)
        report = {"formatVersion": 1, "target": target, "rustc": rustc, **state,
                  "passed": False, "testsSkipped": True, "checks": []}
        if baseline:
            report["glibcBaseline"] = baseline
    else:
        report = reuse_tests(ctx) if reuse else test(ctx)
    run(["cargo", "build", "--locked", "--release", "--target", target,
         "--bin", "vtcode", "--example", "rebuild_syntax_cache"], env=release_environment(env))
    if preview:
        output /= "preview"
    output /= f"v{version}"
    output.mkdir(parents=True, exist_ok=True)
    name = archive_name(version, target, baseline)
    binary = "vtcode.exe" if sys.platform == "win32" else "vtcode"
    executable_suffix = ".exe" if sys.platform == "win32" else ""
    with tempfile.TemporaryDirectory(prefix=".stage-", dir=output) as staging:
        staging = Path(staging)
        package = staging / name
        package.mkdir()
        shutil.copyfile(build / target / "release" / binary, package / binary)
        (package / binary).chmod(0o755)
        copy_assets(package)
        run([build / target / "release" / "examples" / f"rebuild_syntax_cache{executable_suffix}", "grammars"],
            env=env, cwd=package)
        check_payload(package, binary)
        abi = linux_abi.verify((package / binary).read_bytes(), baseline) if baseline else None
        (package / "settings.json").write_text("{}\n", encoding="utf-8", newline="\n")
        write_json(package / "validation.json", report)
        files = [{"path": file.relative_to(package).as_posix(), "size": file.stat().st_size,
                  "sha256": file_digest(file)} for file in sorted(package.rglob("*")) if file.is_file()]
        manifest = {"formatVersion": 2, "product": "VTCode", "version": version, "tag": f"v{version}",
                    "target": target, "binary": binary, "portable": True, "preview": preview, **state, "files": files}
        if skip_tests:
            manifest["testsSkipped"] = True
        if baseline:
            manifest.update(glibcBaseline=baseline, linuxAbi=abi)
        write_json(package / "manifest.json", manifest)
        extension = ".zip" if sys.platform == "win32" else ".tar.gz"
        archive = staging / (name + extension)
        if sys.platform == "win32":
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as container:
                for file in sorted(package.rglob("*")):
                    if file.is_file():
                        container.write(file, file.relative_to(staging).as_posix())
        else:
            with tarfile.open(archive, "w:gz") as container:
                container.add(package, arcname=name)
        checksum = Path(str(archive) + ".sha256")
        checksum.write_text(f"{file_digest(archive)}  {archive.name}\n", encoding="utf-8", newline="\n")
        verify_archive(archive, allow_preview=preview)
        if source_state() != state:
            raise Failure("Sources changed during the release build. Run checks and packaging again.")
        archive.replace(output / archive.name)
        checksum.replace(output / checksum.name)
    print(f"{'Preview' if preview else 'Release'} artifacts ready: {output}")


def verify(directory, require_both=False, allow_preview=False, require_linux_both=False):
    paths = sorted([*directory.glob("*.zip"), *directory.glob("*.tar.gz")])
    if not paths:
        raise Failure(f"No release archives in {directory}")
    manifests = [verify_archive(path, allow_preview=allow_preview) for path in paths]
    identities = [(manifest["target"], manifest.get("glibcBaseline")) for manifest in manifests]
    if len(identities) != len(set(identities)):
        raise Failure("Duplicate release target and glibc baseline.")
    linux = {(TARGETS["linux"], baseline) for baseline in linux_abi.BASELINES}
    if require_both and set(identities) != linux | {(TARGETS["win32"], None)}:
        raise Failure("Expected exactly one Windows archive and both Linux archives (glibc 2.28 and 2.43).")
    if require_linux_both and set(identities) != linux:
        raise Failure("Expected exactly both Linux archives (glibc 2.28 and 2.43).")
    for key in ("version", *source_state_keys()):
        if len({manifest[key] for manifest in manifests}) != 1:
            raise Failure(f"Archives were built from different sources: {key}")
    print(f"Verified {len(paths)} archive(s) for {manifests[0]['tag']} at {manifests[0]['sourceRevision']}.")


def main():
    configure_output()
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    setup_parser = sub.add_parser("setup", help="Install missing Rust components and a private ACP Python environment")
    test_parser = sub.add_parser("test", help="Run the full native CI suite, including external ACP peers")
    release_parser = sub.add_parser("release", help="Test, build and verify a native portable release")
    linux_parser = sub.add_parser("linux-release", help="Build glibc 2.28 and 2.43 Linux releases in containers (or WSL)")
    linux_parser.add_argument("--engine", choices=("auto", "docker", "podman"), default="auto")
    linux_parser.add_argument("--wsl-distro", default="Ubuntu", help="WSL distribution when invoked on Windows")
    for native_parser in (setup_parser, test_parser, release_parser, linux_parser):
        native_parser.add_argument("--glibc", choices=linux_abi.BASELINES,
                                   help="Linux glibc baseline; linux-release builds both by default")
    for build_parser in (release_parser, linux_parser):
        tests = build_parser.add_mutually_exclusive_group()
        tests.add_argument("--reuse-tests", action="store_true", help="Reuse checks only if sources, compiler and baseline still match")
        tests.add_argument("--skip-tests", action="store_true",
                           help="Compile and package without tests; implies --preview")
        build_parser.add_argument("--preview", action="store_true", help="Allow work in progress; write non-publishable artifacts under dist/preview")
        build_parser.add_argument("--output-dir", type=Path,
                                  help="Output root (default: repo/dist); previews add preview/, then all builds add v<VERSION>/")
    verify_parser = sub.add_parser("verify", help="Verify archive checksums, manifests and matching source revisions")
    verify_parser.add_argument("directory", type=Path, help="Directory containing archives and their .sha256 files")
    verify_parser.add_argument("--require-all", "--require-both", dest="require_both", action="store_true",
                               help="Require Windows plus Linux glibc 2.28 and 2.43 from the same sources")
    verify_parser.add_argument("--require-linux-both", action="store_true", help="Require exactly the two Linux variants")
    verify_parser.add_argument("--allow-preview", action="store_true")
    args = parser.parse_args()
    if args.command in ("release", "linux-release") and args.skip_tests:
        args.preview = True
    if args.command == "setup":
        setup(args.glibc)
    elif args.command == "verify":
        verify(args.directory, args.require_both, args.allow_preview, args.require_linux_both)
    elif args.command == "linux-release":
        if args.output_dir is not None:
            args.output_dir = resolve_output_dir(args.output_dir)
        state = source_state()
        if state["sourceDirty"] and not args.preview:
            raise Failure("Commit changes before creating release artifacts, or use --preview.")
        linux_release.build(ROOT, run, args)
        if source_state() != state:
            raise Failure("Sources changed between Linux builds. Rebuild both variants from the same sources.")
    else:
        ctx = context(args.glibc)
        if args.command == "test":
            test(ctx)
        else:
            release(ctx, preview=args.preview, reuse=args.reuse_tests, output_dir=args.output_dir, skip_tests=args.skip_tests)


if __name__ == "__main__":
    try:
        main()
    except (Failure, subprocess.CalledProcessError, OSError, ValueError, KeyError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
