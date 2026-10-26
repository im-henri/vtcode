"""Build both GNU/Linux variants in isolated, pinned x86_64 environments."""

import os
from pathlib import Path
import shutil
import sys
import tomllib


# Pin the base filesystem; local.py also checks its actual glibc before building.
IMAGES = {
    "2.28": "quay.io/pypa/manylinux_2_28_x86_64@sha256:4131ddd7acb9c6c966af02fe7e71edc51e078c2d41f475966697676430bfa2ae",
    "2.43": "docker.io/library/ubuntu@sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794",
}

# A generic /workspace prefix also occurs in LSP JSON pointers and portable
# workspace settings paths. Use a distinct root for strict build-path checks.
CONTAINER_WORKSPACE = "/vtcode-release-source"
CONTAINER_OUTPUT = "/vtcode-release-output"


def build(root, run, args):
    if sys.platform == "win32":
        # --exec bypasses WSL's shell, which otherwise consumes Windows backslashes.
        linux_root = run(["wsl", "-d", args.wsl_distro, "--exec", "wslpath", "-u", root.as_posix()], capture=True)
        command = ["wsl", "-d", args.wsl_distro, "--cd", linux_root, "--exec",
                   "python3", "scripts/local.py", "linux-release", "--engine", args.engine]
        if args.glibc:
            command.extend(["--glibc", args.glibc])
        if args.preview:
            command.append("--preview")
        if args.reuse_tests:
            command.append("--reuse-tests")
        if args.skip_tests:
            command.append("--skip-tests")
        if args.output_dir is not None:
            output = Path(args.output_dir).expanduser().resolve()
            linux_output = run(["wsl", "-d", args.wsl_distro, "--exec", "wslpath", "-u", output.as_posix()], capture=True)
            command.extend(["--output-dir", linux_output])
        run(command)
        return
    if sys.platform != "linux":
        raise ValueError("Linux releases require x86_64 Linux or WSL.")
    engine = args.engine
    if engine == "auto":
        engine = next((name for name in ("docker", "podman") if shutil.which(name)), None)
    if not engine or not shutil.which(engine):
        raise ValueError("Install Docker or rootless Podman in Linux/WSL, then rerun linux-release.")
    if os.uname().machine != "x86_64":
        raise ValueError("Linux releases currently require an x86_64 build host.")
    command = [engine] + (["--cgroup-manager=cgroupfs"] if engine == "podman" else [])
    channel = tomllib.loads((root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    cache_root = Path(os.environ.get("VTCODE_LINUX_RELEASE_CACHE", "~/.cache/vtcode-release")).expanduser().absolute()
    output_mount = []
    if args.output_dir is not None:
        output = Path(args.output_dir).expanduser().resolve()
        output.mkdir(parents=True, exist_ok=True)
        output_mount = ["--volume", f"{output}:{CONTAINER_OUTPUT}"]
    for baseline in ([args.glibc] if args.glibc else IMAGES):
        image = f"vtcode-release:glibc-{baseline}-rust-{channel}"
        build_options = ["--network=slirp4netns"] if engine == "podman" else []
        run([*command, "build", *build_options, "--platform", "linux/amd64", "--tag", image,
             "--build-arg", f"BASE_IMAGE={IMAGES[baseline]}",
             "--build-arg", f"GLIBC_BASELINE={baseline}", "--build-arg", f"RUST_TOOLCHAIN={channel}",
             root / "scripts/linux"])
        cache = cache_root / f"glibc-{baseline}"
        cache.mkdir(parents=True, exist_ok=True)
        # Keep caches in Linux and outputs owned by the caller, including under WSL.
        options = ["--user", f"{os.getuid()}:{os.getgid()}"]
        if engine == "podman":
            options.extend(["--userns=keep-id", "--network=slirp4netns", "--security-opt=label=disable"])
        release_args = ["--glibc", baseline]
        if args.preview:
            release_args.append("--preview")
        if args.reuse_tests:
            release_args.append("--reuse-tests")
        if args.skip_tests:
            release_args.append("--skip-tests")
        if args.output_dir is not None:
            release_args.extend(["--output-dir", CONTAINER_OUTPUT])
        run([*command, "run", "--rm", *options,
             "--volume", f"{root}:{CONTAINER_WORKSPACE}", "--volume", f"{cache}:/cache",
             *output_mount,
             "--workdir", CONTAINER_WORKSPACE, "--env", "HOME=/cache/home",
             "--env", "CARGO_HOME=/cache/cargo", "--env", "CARGO_TARGET_DIR=/cache/target",
             "--env", f"CARGO_BUILD_JOBS={os.environ.get('CARGO_BUILD_JOBS', '2')}",
             "--env", f"RUST_TEST_THREADS={os.environ.get('RUST_TEST_THREADS', '2')}",
             image, *release_args])
