"""Check build isolation and Windows-to-WSL forwarding without a container daemon."""

import argparse
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import linux_release
import local


class LinuxReleaseTests(unittest.TestCase):
    def options(self, **overrides):
        return argparse.Namespace(**({"engine": "podman", "wsl_distro": "Ubuntu",
                                      "glibc": None, "preview": True, "reuse_tests": False,
                                      "skip_tests": False, "output_dir": None} | overrides))

    def test_both_variants_use_different_images_and_caches(self):
        with tempfile.TemporaryDirectory() as temporary:
            calls = []
            with (patch.object(sys, "platform", "linux"),
                  patch.object(os, "uname", create=True) as uname,
                  patch.object(os, "getuid", create=True, return_value=1000),
                  patch.object(os, "getgid", create=True, return_value=1000),
                  patch.object(linux_release.shutil, "which", return_value="/usr/bin/podman"),
                  patch.dict(os.environ, {"VTCODE_LINUX_RELEASE_CACHE": temporary})):
                uname.return_value.machine = "x86_64"
                linux_release.build(local.ROOT, lambda command: calls.append([str(arg) for arg in command]), self.options())
            builds = [call for call in calls if "build" in call]
            runs = [call for call in calls if "run" in call]
            self.assertEqual(len(builds), 2)
            self.assertEqual(len(runs), 2)
            for baseline, build, run in zip(("2.28", "2.43"), builds, runs):
                self.assertIn(f"BASE_IMAGE={linux_release.IMAGES[baseline]}", build)
                self.assertIn(f"{Path(temporary) / ('glibc-' + baseline)}:/cache", run)
                self.assertIn(f"{local.ROOT}:{linux_release.CONTAINER_WORKSPACE}", run)
                self.assertEqual(run[run.index("--workdir") + 1], linux_release.CONTAINER_WORKSPACE)
                self.assertEqual(run[-3:], ["--glibc", baseline, "--preview"])
                self.assertIn("--userns=keep-id", run)

    def test_payload_paths_distinguish_lsp_literals_from_build_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = Path(temporary)
            (package / "grammars/.cache").mkdir(parents=True)
            (package / "themes").mkdir()
            (package / "grammars/bundled").mkdir()
            (package / "grammars/bundled/index.json").write_bytes(b'{"version":1,"grammars":{"fixture":{"scopeName":"source.fixture"}}}')
            (package / "grammars/bundled/fixture.json").write_bytes(b'{"scopeName":"source.fixture","patterns":[]}')
            for number in range(3):
                (package / f"grammars/grammar-{number}.tmLanguage.json").write_bytes(b"grammar")
            (package / "themes/dark.json").write_bytes(b"{}")
            (package / "grammars/.cache/textmate-registry.json.gz").write_bytes(b"cache")
            (package / "grammars/.cache/source.fingerprint").write_bytes(b"fingerprint")
            binary = package / "vtcode"
            binary.write_bytes(b"/workspaceSymbolProvider\0/workspace/fileOperations/willRename\0.vtcode/workspaces")
            with (patch.object(local, "ROOT", Path(linux_release.CONTAINER_WORKSPACE)),
                  patch.object(Path, "home", return_value=Path("/cache/home"))):
                local.check_payload(package, "vtcode")
                for directory in (local.ROOT, Path.home()):
                    for encoding in ("utf-8", "utf-16-le"):
                        with self.subTest(directory=str(directory), encoding=encoding):
                            path = str(directory / "src/main.rs").replace("\\", "/")
                            binary.write_bytes(path.encode(encoding))
                            with self.assertRaisesRegex(local.Failure, "Build-machine path"):
                                local.check_payload(package, "vtcode")

    def test_windows_forwards_selected_options_to_wsl(self):
        calls = []

        def run(command, **kwargs):
            calls.append([str(arg) for arg in command])
            return "/mnt/c/repository with spaces" if kwargs.get("capture") else None

        with patch.object(sys, "platform", "win32"):
            linux_release.build(local.ROOT, run,
                                self.options(glibc="2.28", wsl_distro="Build Ubuntu", reuse_tests=True))
        self.assertIn("Build Ubuntu", calls[1])
        self.assertIn("--exec", calls[0])
        self.assertIn("--exec", calls[1])
        self.assertNotIn("\\", calls[0][-1])
        self.assertIn("/mnt/c/repository with spaces", calls[1])
        self.assertEqual(calls[1][-4:], ["--glibc", "2.28", "--preview", "--reuse-tests"])

    def test_windows_forwards_skip_tests_to_wsl(self):
        calls = []

        def run(command, **kwargs):
            calls.append([str(arg) for arg in command])
            return "/mnt/c/repository" if kwargs.get("capture") else None

        with patch.object(sys, "platform", "win32"):
            linux_release.build(local.ROOT, run, self.options(skip_tests=True))
        self.assertIn("--skip-tests", calls[-1])
        self.assertNotIn("--reuse-tests", calls[-1])

    @unittest.skipUnless(sys.platform == "linux", "The container entrypoint runs under Linux")
    def test_container_entrypoint_skips_test_environment_setup(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            fake_bin = directory / "bin"
            fake_bin.mkdir()
            python = fake_bin / "python3"
            python.write_text('#!/bin/sh\nprintf \'%s\\n\' "$@" >> "$VTCODE_TEST_COMMANDS"\n', encoding="utf-8")
            python.chmod(0o755)
            log = directory / "commands"
            arguments = ["--glibc", "2.28", "--skip-tests", "--output-dir", str(directory / "build output")]
            env = {**os.environ, "PATH": str(fake_bin) + os.pathsep + os.environ["PATH"],
                   "CARGO_HOME": str(directory / "cargo"), "VTCODE_TEST_COMMANDS": str(log)}
            subprocess.run(["bash", str(local.ROOT / "scripts/linux/run.sh"), *arguments], env=env, check=True)
            self.assertEqual(log.read_text().splitlines(), ["scripts/local.py", "release", *arguments])

    def test_native_baseline_must_match_host_glibc(self):
        with patch.object(sys, "platform", "linux"), patch.object(local, "run", return_value="glibc 2.43"):
            with self.assertRaisesRegex(local.Failure, "Requested glibc 2.28"):
                local.environment("2.28")
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": "/build/target"}):
                env = local.environment("2.43")
                self.assertEqual(env["CARGO_TARGET_DIR"], str(Path("/build/target") / "glibc-2.43"))
                self.assertEqual(env["VTCODE_GLIBC_BASELINE"], "2.43")

    def test_windows_translates_the_output_path_before_forwarding_to_wsl(self):
        with tempfile.TemporaryDirectory() as temporary:
            calls = []
            output = Path(temporary) / "build output"

            def run(command, **kwargs):
                calls.append([str(arg) for arg in command])
                if kwargs.get("capture"):
                    return "/mnt/d/build output" if command[-1] == output.as_posix() else "/mnt/c/repository"

            with patch.object(sys, "platform", "win32"):
                linux_release.build(local.ROOT, run,
                                    self.options(glibc="2.43", output_dir=output))
        self.assertEqual(len(calls), 3)
        self.assertIn("--exec", calls[1])
        self.assertEqual(calls[1][-3:], ["wslpath", "-u", output.as_posix()])
        self.assertEqual(calls[2][-2:], ["--output-dir", "/mnt/d/build output"])
        self.assertEqual(calls[2][calls[2].index("--glibc") + 1], "2.43")

    def test_linux_mounts_custom_output_separately_from_each_variants_cache(self):
        with tempfile.TemporaryDirectory() as temporary:
            calls = []
            output = Path(temporary) / "build output"
            with (patch.object(sys, "platform", "linux"),
                  patch.object(os, "uname", create=True) as uname,
                  patch.object(os, "getuid", create=True, return_value=1000),
                  patch.object(os, "getgid", create=True, return_value=1000),
                  patch.object(linux_release.shutil, "which", return_value="/usr/bin/docker"),
                  patch.dict(os.environ, {"VTCODE_LINUX_RELEASE_CACHE": str(Path(temporary) / "cache")})):
                uname.return_value.machine = "x86_64"
                linux_release.build(local.ROOT, lambda command: calls.append([str(arg) for arg in command]),
                                    self.options(engine="docker", output_dir=output, skip_tests=True))
            self.assertTrue(output.is_dir())
            runs = [call for call in calls if "run" in call]
            self.assertEqual(len(runs), 2)
            for baseline, command in zip(("2.28", "2.43"), runs):
                self.assertIn(f"{output}:{linux_release.CONTAINER_OUTPUT}", command)
                self.assertIn(f"{Path(temporary) / 'cache' / ('glibc-' + baseline)}:/cache", command)
                self.assertEqual(command[-6:], ["--glibc", baseline, "--preview", "--skip-tests",
                                               "--output-dir", linux_release.CONTAINER_OUTPUT])

    def test_test_report_from_other_baseline_is_not_reusable(self):
        with tempfile.TemporaryDirectory() as temporary:
            build = Path(temporary)
            target = local.TARGETS["linux"]
            state = {"sourceRevision": "a" * 40, "sourceDirty": True, "sourceFingerprint": "b" * 64}
            report = {**state, "passed": True, "target": target, "rustc": "test compiler", "glibcBaseline": "2.43",
                      "checks": [{"command": command, "exitCode": 0} for command in local.check_commands(target)]}
            directory = build / "local-ci" / target
            directory.mkdir(parents=True)
            local.write_json(directory / "validation.json", report)
            with patch.object(local, "check_python"), patch.object(local, "source_state", return_value=state):
                with self.assertRaisesRegex(local.Failure, "stale or incomplete"):
                    local.reuse_tests(({"VTCODE_GLIBC_BASELINE": "2.28"}, target, "1.2.3", build, "test compiler"))


if __name__ == "__main__":
    unittest.main()
