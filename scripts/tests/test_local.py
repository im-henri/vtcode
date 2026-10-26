"""Exercise release integrity without compiling Rust or contacting GitHub."""

import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile


sys.path.insert(0, str(Path(__file__).parents[1]))
spec = importlib.util.spec_from_file_location("local", Path(__file__).parents[1] / "local.py")
local = importlib.util.module_from_spec(spec)
spec.loader.exec_module(local)


class ReleaseIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def archive(self, target=None, *, preview=False, dirty=False, revision="a" * 40, executable=True,
                baseline="2.28", required_glibc=None, report_baseline=None, skip_tests=False):
        target = target or local.TARGETS["win32"]
        binary = "vtcode.exe" if target == local.TARGETS["win32"] else "vtcode"
        state = {"sourceRevision": revision, "sourceDirty": dirty, "sourceFingerprint": "b" * 64}
        report = {**state, "passed": True, "target": target,
                  "checks": [{"command": command, "exitCode": 0} for command in local.check_commands(target)]}
        if skip_tests:
            report.update(passed=False, testsSkipped=True, checks=[])
        payload = {name: b"payload" for name in (binary, "LICENSE", "README.md", "THIRD_PARTY_NOTICES.md",
                   "vendor/crossterm/LICENSE", "syntaxes/.cache/default_newlines.packdump",
                   "syntaxes/.cache/source.fingerprint", "themes/dark.json")}
        payload["settings.json"] = b"{}\n"
        if target == local.TARGETS["linux"] and baseline:
            from test_linux_abi import elf
            payload[binary] = elf(glibc=required_glibc or baseline)
            report["glibcBaseline"] = report_baseline or baseline
        payload["validation.json"] = json.dumps(report).encode()
        payload.update({f"syntaxes/grammar-{number}.sublime-syntax": b"grammar" for number in range(226)})
        manifest = {"formatVersion": 2 if baseline else 1, "product": "VTCode", "version": "1.2.3", "tag": "v1.2.3",
                    "target": target, "binary": binary, "portable": True, "preview": preview, **state,
                    "files": [{"path": name, "size": len(data), "sha256": local.digest(io.BytesIO(data))}
                              for name, data in payload.items()]}
        if skip_tests:
            manifest["testsSkipped"] = True
        if target == local.TARGETS["linux"] and baseline:
            manifest.update(glibcBaseline=baseline, linuxAbi=local.linux_abi.inspect(payload[binary]))
        payload["manifest.json"] = json.dumps(manifest).encode()
        name = local.archive_name("1.2.3", target, baseline if target == local.TARGETS["linux"] else None)
        archive = self.directory / (name + (".zip" if binary.endswith(".exe") else ".tar.gz"))
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive, "w") as container:
                for relative, data in payload.items():
                    container.writestr(f"{name}/{relative}", data)
        else:
            with tarfile.open(archive, "w:gz") as container:
                for relative, data in payload.items():
                    entry = tarfile.TarInfo(f"{name}/{relative}")
                    entry.size = len(data)
                    entry.mode = 0o755 if relative == binary and executable else 0o644
                    container.addfile(entry, io.BytesIO(data))
        self.checksum(archive)
        return archive

    def checksum(self, archive):
        Path(str(archive) + ".sha256").write_text(f"{local.file_digest(archive)}  {archive.name}\n")

    def test_valid_windows_and_linux_archives_can_be_verified_together(self):
        self.archive()
        self.archive(local.TARGETS["linux"])
        self.archive(local.TARGETS["linux"], baseline="2.43")
        local.verify(self.directory, require_both=True)

    def test_linux_pair_can_be_verified_without_windows(self):
        self.archive(local.TARGETS["linux"])
        self.archive(local.TARGETS["linux"], baseline="2.43")
        local.verify(self.directory, require_linux_both=True)

    def test_linux_baseline_is_checked_against_binary(self):
        archive = self.archive(local.TARGETS["linux"], required_glibc="2.43")
        with self.assertRaisesRegex(local.Failure, "newer than its 2.28 baseline"):
            local.verify_archive(archive)

    def test_linux_tests_must_match_the_build_baseline(self):
        archive = self.archive(local.TARGETS["linux"], report_baseline="2.43")
        with self.assertRaisesRegex(local.Failure, "inconsistent validation"):
            local.verify_archive(archive)

    def test_legacy_linux_archive_is_verifiable_but_not_a_complete_new_release(self):
        archive = self.archive(local.TARGETS["linux"], baseline=None)
        local.verify_archive(archive)
        self.archive()
        with self.assertRaisesRegex(local.Failure, "both Linux archives"):
            local.verify(self.directory, require_both=True)

    def test_checksum_detects_archive_corruption(self):
        archive = self.archive()
        archive.write_bytes(archive.read_bytes() + b"corruption")
        with self.assertRaisesRegex(local.Failure, "checksum mismatch"):
            local.verify_archive(archive)

    def test_manifest_detects_payload_corruption_even_with_a_new_checksum(self):
        archive = self.archive()
        with zipfile.ZipFile(archive) as container:
            payload = {name: container.read(name) for name in container.namelist()}
        binary = next(name for name in payload if name.endswith("/vtcode.exe"))
        payload[binary] = b"tampered"
        with zipfile.ZipFile(archive, "w") as container:
            for name, data in payload.items():
                container.writestr(name, data)
        self.checksum(archive)
        with self.assertRaisesRegex(local.Failure, "payload mismatch"):
            local.verify_archive(archive)

    def test_preview_and_dirty_sources_cannot_pass_publication_verification(self):
        for options in ({"preview": True}, {"dirty": True}):
            archive = self.archive(**options)
            with self.assertRaisesRegex(local.Failure, "Preview or uncommitted"):
                local.verify_archive(archive)
            local.verify_archive(archive, allow_preview=True)

    def test_linux_archive_must_preserve_execute_permission(self):
        archive = self.archive(local.TARGETS["linux"], executable=False)
        with self.assertRaisesRegex(local.Failure, "not executable"):
            local.verify_archive(archive)

    def test_skipped_tests_are_accepted_only_in_explicit_preview_packages(self):
        for target in local.TARGETS.values():
            with self.subTest(target=target):
                archive = self.archive(target, preview=True, skip_tests=True)
                local.verify_archive(archive, allow_preview=True)
                with self.assertRaisesRegex(local.Failure, "Preview or uncommitted"):
                    local.verify_archive(archive)
                archive = self.archive(target, preview=False, skip_tests=True)
                with self.assertRaisesRegex(local.Failure, "inconsistent validation report"):
                    local.verify_archive(archive, allow_preview=True)

    def test_skipped_tests_still_require_the_matching_linux_baseline(self):
        archive = self.archive(local.TARGETS["linux"], preview=True, skip_tests=True, report_baseline="2.43")
        with self.assertRaisesRegex(local.Failure, "inconsistent validation report"):
            local.verify_archive(archive, allow_preview=True)

    def test_release_api_rejects_skipping_and_reusing_tests_together(self):
        with self.assertRaisesRegex(local.Failure, "cannot be combined"):
            local.release(None, reuse=True, skip_tests=True)

    def test_two_platforms_must_have_the_same_revision(self):
        self.archive()
        self.archive(local.TARGETS["linux"], revision="c" * 40)
        self.archive(local.TARGETS["linux"], baseline="2.43")
        with self.assertRaisesRegex(local.Failure, "different sources"):
            local.verify(self.directory, require_both=True)

    def test_missing_platform_is_reported(self):
        self.archive()
        with self.assertRaisesRegex(local.Failure, "exactly one Windows"):
            local.verify(self.directory, require_both=True)

    def test_stale_test_report_cannot_be_reused(self):
        target = local.TARGETS["win32"]
        state = {"sourceRevision": "a" * 40, "sourceDirty": False, "sourceFingerprint": "b" * 64}
        folder = self.directory / "local-ci" / target
        folder.mkdir(parents=True)
        report = {**state, "passed": True, "target": target, "rustc": "test compiler",
                  "checks": [{"command": command, "exitCode": 0} for command in local.check_commands(target)]}
        local.write_json(folder / "validation.json", report)
        context = ({}, target, "1.2.3", self.directory, "test compiler")
        with patch.object(local, "check_python"), patch.object(local, "source_state", return_value=state):
            self.assertEqual(local.reuse_tests(context), report)
        with patch.object(local, "check_python"), patch.object(local, "source_state", return_value={**state, "sourceFingerprint": "c" * 64}):
            with self.assertRaisesRegex(local.Failure, "stale or incomplete"):
                local.reuse_tests(context)

    def test_partial_test_report_cannot_be_reused(self):
        target = local.TARGETS["win32"]
        folder = self.directory / "local-ci" / target
        folder.mkdir(parents=True)
        local.write_json(folder / "validation.json", {"passed": False})
        with patch.object(local, "check_python"):
            with self.assertRaisesRegex(local.Failure, "stale or incomplete"):
                local.reuse_tests(({}, target, "1.2.3", self.directory, "test compiler"))

    def test_release_packages_current_sources_into_custom_output(self):
        self.package_current_sources()

    def test_skip_tests_packages_without_running_or_reusing_checks(self):
        self.package_current_sources(skip_tests=True)

    def package_current_sources(self, *, skip_tests=False):
        from test_linux_abi import elf

        root = self.directory / "source"
        (root / "syntaxes").mkdir(parents=True)
        (root / "themes").mkdir()
        for number in range(226):
            (root / f"syntaxes/grammar-{number}.sublime-syntax").write_bytes(b"grammar")
        (root / "themes/dark.json").write_bytes(b"{}")
        for name in ("README.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "vendor/crossterm/LICENSE"):
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"license or documentation")
        state = {"sourceRevision": "a" * 40, "sourceDirty": True, "sourceFingerprint": "b" * 64}

        def run(command, **kwargs):
            if command[0] != "cargo":
                cache = kwargs["cwd"] / "syntaxes" / ".cache"
                cache.mkdir()
                (cache / "default_newlines.packdump").write_bytes(b"cache")
                (cache / "source.fingerprint").write_bytes(b"fingerprint")

        target = local.TARGETS[sys.platform]
        baseline = "2.28" if sys.platform == "linux" else None
        binary = "vtcode.exe" if sys.platform == "win32" else "vtcode"
        build = self.directory / "cache"
        binary_path = build / target / "release" / binary
        binary_path.parent.mkdir(parents=True)
        binary_path.write_bytes(elf() if baseline else b"executable")
        env = {"VTCODE_GLIBC_BASELINE": baseline} if baseline else {}
        report = {**state, "passed": True, "target": target,
                  "checks": [{"command": command, "exitCode": 0}
                             for command in local.check_commands(target)]}
        if baseline:
            report["glibcBaseline"] = baseline
        output = self.directory / "custom output"
        cached_report = build / "local-ci" / target / "validation.json"
        cached_report.parent.mkdir(parents=True)
        cached_report.write_bytes(b"existing cached test report")
        with (patch.object(local, "ROOT", root), patch.object(local, "source_state", return_value=state),
              patch.object(local, "test", return_value=report) as tests,
              patch.object(local, "reuse_tests") as reuse, patch.object(local, "run", side_effect=run)):
            local.release((env, target, "1.2.3", build, "test compiler"), preview=not skip_tests,
                          output_dir=output, skip_tests=skip_tests)
        if skip_tests:
            tests.assert_not_called()
        else:
            tests.assert_called_once()
        reuse.assert_not_called()
        self.assertEqual(cached_report.read_bytes(), b"existing cached test report")
        extension = ".zip" if sys.platform == "win32" else ".tar.gz"
        archive = output / "preview/v1.2.3" / (local.archive_name("1.2.3", target, baseline) + extension)
        manifest = local.verify_archive(archive, allow_preview=True)
        self.assertTrue(manifest["sourceDirty"])
        self.assertTrue(manifest["preview"])
        self.assertEqual(manifest["target"], target)
        self.assertEqual(manifest.get("testsSkipped", False), skip_tests)
        self.assertFalse((root / "dist").exists())

    def test_unicode_diagnostics_work_with_a_windows_ansi_output_stream(self):
        output = io.BytesIO()
        stream = io.TextIOWrapper(output, encoding="cp1252")
        with patch.object(sys, "stdout", stream), patch.object(sys, "stderr", stream):
            local.configure_output()
            local.run([sys.executable, "-c", "print('\\u2500')"], env=local.environment(), log=io.StringIO())
            stream.flush()
        self.assertIn("\u2500".encode(), output.getvalue())
        stream.detach()

    @unittest.skipUnless(sys.platform == "linux", "Unix virtualenv interpreters use symlinks")
    def test_existing_python_environment_preserves_its_virtualenv_symlink(self):
        interpreter = self.directory / "venv" / "bin" / "python"
        interpreter.parent.mkdir(parents=True)
        interpreter.symlink_to(sys.executable)
        with patch.dict(os.environ, {"VTCODE_ACP_TEST_PYTHON": str(interpreter)}):
            self.assertEqual(local.python_path(), interpreter)
            self.assertNotEqual(local.python_path(), interpreter.resolve())


if __name__ == "__main__":
    unittest.main()
