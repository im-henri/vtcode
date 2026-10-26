"""Check platform selection, fail-fast builds, and destination handling without Rust builds."""

import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parents[1]))
import build
import local


class BuildTests(unittest.TestCase):
    def test_windows_all_keeps_arguments_with_spaces_and_builds_sequentially(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(sys, "platform", "win32"):
            output = Path(temporary) / "build output"
            args = build.parse_args(["--target", "all", "--glibc", "2.28", "--output-dir", str(output),
                                     "--wsl-distro", "Build Ubuntu", "--engine", "podman", "--reuse-tests"])
            commands = build.build_commands(args)
        self.assertEqual([command[2] for command in commands], ["setup", "release", "linux-release"])
        for command in commands[1:]:
            self.assertIn("--preview", command)
            self.assertIn("--reuse-tests", command)
            self.assertEqual(command[command.index("--output-dir") + 1], str(output.resolve()))
        linux = commands[-1]
        self.assertEqual(linux[linux.index("--wsl-distro") + 1], "Build Ubuntu")
        self.assertEqual(linux[linux.index("--glibc") + 1], "2.28")
        self.assertEqual(linux[linux.index("--engine") + 1], "podman")

    def test_each_linux_variant_and_both_are_available(self):
        with patch.object(sys, "platform", "linux"):
            for version in (None, "both", "2.28", "2.43"):
                with self.subTest(version=version):
                    args = build.parse_args(["--target", "linux", *(["--glibc", version] if version else [])])
                    commands = build.build_commands(args)
                    self.assertEqual(len(commands), 1)
                    command = commands[0]
                    self.assertEqual(command[2], "linux-release")
                    self.assertNotIn("--wsl-distro", command)
                    if version in (None, "both"):
                        self.assertNotIn("--glibc", command)
                    else:
                        self.assertEqual(command[command.index("--glibc") + 1], version)

    def test_defaults_follow_host_platform_and_use_repo_dist(self):
        for host, target in (("win32", "windows"), ("linux", "linux")):
            with self.subTest(host=host), patch.object(sys, "platform", host):
                args = build.parse_args([])
                self.assertEqual(args.target, target)
                self.assertEqual(args.output_dir, build.ROOT / "dist")

    def test_skip_tests_omits_windows_test_setup_and_reaches_all_build_targets(self):
        with patch.object(sys, "platform", "win32"):
            commands = build.build_commands(build.parse_args(["--target", "all", "--skip-tests"]))
        self.assertEqual([command[2] for command in commands], ["release", "linux-release"])
        for command in commands:
            self.assertIn("--skip-tests", command)
            self.assertIn("--preview", command)
            self.assertNotIn("--reuse-tests", command)

    def test_skip_tests_and_reuse_tests_are_mutually_exclusive(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            build.parse_args(["--skip-tests", "--reuse-tests"])
        self.assertEqual(error.exception.code, 2)

    def test_invalid_or_unsupported_targets_fail_before_building(self):
        cases = [("linux", ["--target", "windows"]), ("linux", ["--target", "all"]),
                 ("win32", ["--target", "windows", "--glibc", "2.28"]),
                 ("win32", ["--target", "windows", "--engine", "podman"]),
                 ("linux", ["--glibc", "2.99"]), ("darwin", [])]
        for host, arguments in cases:
            with (self.subTest(host=host, arguments=arguments), patch.object(sys, "platform", host),
                  contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error):
                build.parse_args(arguments)
            self.assertEqual(error.exception.code, 2)

    def test_dry_run_neither_spawns_processes_nor_creates_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "not created"
            with (patch.object(sys, "platform", "win32"), patch.object(local, "run") as run,
                  patch.object(local, "source_state") as state,
                  patch.object(local, "resolve_output_dir") as resolve,
                  contextlib.redirect_stdout(io.StringIO()) as printed):
                build.main(["--target", "all", "--output-dir", str(output), "--dry-run"])
            run.assert_not_called()
            state.assert_not_called()
            resolve.assert_not_called()
            self.assertFalse(output.exists())
            self.assertIn("Planned output:", printed.getvalue())
            self.assertIn("linux-release", printed.getvalue())

    def test_failed_windows_build_does_not_start_linux_or_report_success(self):
        failure = subprocess.CalledProcessError(7, ["release"])
        with (patch.object(sys, "platform", "win32"), patch.object(local, "resolve_output_dir"),
              patch.object(local, "source_state", return_value={"sourceFingerprint": "same"}),
              patch.object(local, "run", side_effect=[None, failure]) as run,
              contextlib.redirect_stdout(io.StringIO()) as printed):
            with self.assertRaises(subprocess.CalledProcessError):
                build.main(["--target", "all"])
        self.assertEqual([call.args[0][2] for call in run.call_args_list], ["setup", "release"])
        self.assertNotIn("Builds complete:", printed.getvalue())

    def test_source_change_stops_before_building_another_target(self):
        with (patch.object(sys, "platform", "win32"), patch.object(local, "resolve_output_dir"),
              patch.object(local, "source_state", side_effect=[{"fingerprint": 1}, {"fingerprint": 2}]),
              patch.object(local, "run") as run):
            with self.assertRaisesRegex(local.Failure, "Sources changed between build steps"):
                build.main(["--target", "all"])
        self.assertEqual(run.call_count, 1)

    def test_successful_build_reports_destination_only_after_all_steps(self):
        with (patch.object(sys, "platform", "win32"), patch.object(local, "resolve_output_dir"),
              patch.object(local, "source_state", return_value={"sourceFingerprint": "same"}),
              patch.object(local, "run") as run,
              contextlib.redirect_stdout(io.StringIO()) as printed):
            build.main(["--target", "all"])
        self.assertEqual(run.call_count, 3)
        self.assertIn("Builds complete:", printed.getvalue())


class OutputDirectoryTests(unittest.TestCase):
    def test_only_ignored_output_is_allowed_inside_the_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source checkout"
            root.mkdir()
            subprocess.run(["git", "init", "--quiet", str(root)], check=True)
            (root / ".gitignore").write_text("/dist/\n/target/\n", encoding="utf-8")
            with patch.object(local, "ROOT", root):
                self.assertEqual(local.resolve_output_dir(), root / "dist")
                self.assertEqual(local.resolve_output_dir(root / "target" / "my builds"),
                                 root / "target" / "my builds")
                for invalid in (root, root / "src", root / "new output"):
                    with self.subTest(path=invalid), self.assertRaisesRegex(local.Failure, "Git-ignored"):
                        local.resolve_output_dir(invalid)
                external = Path(temporary) / "external builds"
                self.assertEqual(local.resolve_output_dir(external), external)
                self.assertFalse(external.exists())


if __name__ == "__main__":
    unittest.main()
