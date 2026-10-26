"""ELF fixtures model loader-visible data, including stripped executables."""

from pathlib import Path
import struct
import sys
import unittest

sys.path.insert(0, str(Path(__file__).parents[1]))
import linux_abi


def elf(glibc="2.28", library="libc.so.6", interpreter=linux_abi.INTERPRETER):
    # An ELF with no section headers: PT_LOAD covers the file and PT_DYNAMIC
    # points at one version-need record. Arbitrary strings are not requirements.
    data = bytearray(1024)
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    struct.pack_into("<16sHHIQQQIHHHHHH", data, 0, ident, 3, 62, 1, 0, 64, 0, 0, 64, 56, 3, 0, 0, 0)
    base = 0x400000
    strings = b"\0" + library.encode() + b"\0GLIBC_" + glibc.encode() + b"\0"
    data[512:512 + len(strings)] = strings
    interp = interpreter.encode() + b"\0"
    data[256:256 + len(interp)] = interp
    tags = [(1, 1), (5, base + 512), (10, len(strings)), (0x6FFFFFFE, base + 640), (0x6FFFFFFF, 1), (0, 0)]
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 0, base, base, len(data), len(data), 4096)
    struct.pack_into("<IIQQQQQQ", data, 120, 3, 4, 256, base + 256, 0, len(interp), len(interp), 1)
    struct.pack_into("<IIQQQQQQ", data, 176, 2, 6, 320, base + 320, 0, len(tags) * 16, len(tags) * 16, 8)
    for index, tag in enumerate(tags):
        struct.pack_into("<qQ", data, 320 + index * 16, *tag)
    struct.pack_into("<HHIII", data, 640, 1, 1, 1, 16, 0)
    struct.pack_into("<IHHII", data, 656, 0, 0, 2, len(library) + 2, 0)
    data[800:810] = b"GLIBC_99.99"
    return bytes(data)


class LinuxAbiTests(unittest.TestCase):
    def test_stripped_elf_uses_import_versions_not_arbitrary_strings(self):
        abi = linux_abi.verify(elf(), "2.28")
        self.assertEqual(abi["requiredGlibc"], "2.28")
        self.assertEqual(abi["neededLibraries"], ["libc.so.6"])

    def test_version_comparison_is_numeric(self):
        self.assertEqual(linux_abi.verify(elf("2.9"), "2.28")["requiredGlibc"], "2.9")

    def test_new_glibc_requirement_is_rejected_on_old_baseline(self):
        with self.assertRaisesRegex(ValueError, "newer than"):
            linux_abi.verify(elf("2.43"), "2.28")
        linux_abi.verify(elf("2.43"), "2.43")

    def test_private_glibc_symbols_are_not_portable(self):
        with self.assertRaisesRegex(ValueError, "Unsupported glibc version"):
            linux_abi.verify(elf("PRIVATE"), "2.43")

    def test_unbundled_native_library_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "Unexpected Linux shared libraries"):
            linux_abi.verify(elf(library="libgit2.so.1.9"), "2.28")

    def test_nonstandard_loader_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "Unexpected Linux interpreter"):
            linux_abi.verify(elf(interpreter="/build/lib/ld.so"), "2.28")

    def test_truncated_or_non_linux_binary_is_rejected(self):
        for data in (b"MZ" + bytes(200), elf()[:100], elf()[:650]):
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                linux_abi.verify(data, "2.28")


if __name__ == "__main__":
    unittest.main()
