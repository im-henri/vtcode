"""Inspect Linux release requirements without running the binary or needing readelf.

Read ELF program headers and the dynamic version-need table, which survive
stripping. Searching arbitrary binary strings would also find unused versions.
"""

import re
import struct


BASELINES = ("2.28", "2.43")
INTERPRETER = "/lib64/ld-linux-x86-64.so.2"
SYSTEM_LIBRARIES = {
    "libc.so.6", "libm.so.6", "libgcc_s.so.1", "ld-linux-x86-64.so.2",
    "libpthread.so.0", "libdl.so.2", "librt.so.1", "libutil.so.1",
    "libresolv.so.2", "libanl.so.1",
}


def version_tuple(version):
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", version):
        raise ValueError(f"Unsupported glibc version: {version}")
    return tuple(map(int, version.split(".")))


def inspect(data):
    """Return the runtime requirements of a dynamically linked x86_64 ELF."""
    def unpack(fmt, offset):
        if offset < 0 or offset + struct.calcsize(fmt) > len(data):
            raise ValueError("Truncated Linux ELF binary")
        return struct.unpack_from(fmt, data, offset)

    def string(offset, limit):
        if offset < 0 or limit > len(data) or offset >= limit:
            raise ValueError("Invalid ELF string offset")
        end = data.find(b"\0", offset, limit)
        if end == -1:
            raise ValueError("Unterminated ELF string")
        return data[offset:end].decode("ascii")

    header = unpack("<16sHHIQQQIHHHHHH", 0)
    if (header[0][:7] != b"\x7fELF\x02\x01\x01" or header[1] not in (2, 3)
            or header[2] != 62 or header[9] != 56):
        raise ValueError("Expected a little-endian x86_64 Linux ELF binary")
    segments = [unpack("<IIQQQQQQ", header[5] + index * header[9]) for index in range(header[10])]

    def address(value, size=1):
        for kind, _, offset, virtual, _, length, _, _ in segments:
            if kind == 1 and virtual <= value and value + size <= virtual + length:
                result = offset + value - virtual
                if result + size <= len(data):
                    return result
        raise ValueError("ELF dynamic data is outside its load segments")

    interpreters = [string(segment[2], segment[2] + segment[5]) for segment in segments if segment[0] == 3]
    dynamic = [segment for segment in segments if segment[0] == 2]
    if len(interpreters) != 1 or len(dynamic) != 1:
        raise ValueError("Expected a dynamically linked Linux executable")
    entries = {}
    segment = dynamic[0]
    for offset in range(segment[2], segment[2] + segment[5], 16):
        tag, value = unpack("<qQ", offset)
        if tag == 0:
            break
        entries.setdefault(tag, []).append(value)
    else:
        raise ValueError("Unterminated ELF dynamic table")

    def single(tag):
        values = entries.get(tag, [])
        if len(values) != 1:
            raise ValueError(f"Missing or duplicate ELF dynamic tag: {tag}")
        return values[0]

    strings_size = single(10)  # DT_STRSZ
    strings = address(single(5), strings_size)  # DT_STRTAB

    def dynamic_string(offset):
        return string(strings + offset, strings + strings_size)

    libraries = sorted(set(dynamic_string(value) for value in entries.get(1, [])))  # DT_NEEDED
    versions = set()
    count = single(0x6FFFFFFF)  # DT_VERNEEDNUM
    if not 0 < count <= 4096:
        raise ValueError("Invalid ELF version-need count")
    need_address = single(0x6FFFFFFE)  # DT_VERNEED
    for index in range(count):
        version, aux_count, _, aux_offset, next_offset = unpack("<HHIII", address(need_address, 16))
        if version != 1 or not 0 < aux_count <= 4096 or aux_offset < 16:
            raise ValueError("Invalid ELF version-need entry")
        aux_address = need_address + aux_offset
        for aux_index in range(aux_count):
            _, _, _, name, next_aux = unpack("<IHHII", address(aux_address, 16))
            versions.add(dynamic_string(name))
            if aux_index + 1 < aux_count and next_aux < 16:
                raise ValueError("Invalid ELF version auxiliary chain")
            aux_address += next_aux
        if index + 1 < count and next_offset < 16:
            raise ValueError("Invalid ELF version-need chain")
        need_address += next_offset
    glibc = sorted({name.removeprefix("GLIBC_") for name in versions if name.startswith("GLIBC_")},
                   key=version_tuple)
    if not glibc:
        raise ValueError("Linux binary has no glibc version requirements")
    if 15 in entries or 29 in entries:  # DT_RPATH, DT_RUNPATH
        raise ValueError("Linux release must not embed a library search path")
    return {"interpreter": interpreters[0], "neededLibraries": libraries,
            "glibcVersions": glibc, "requiredGlibc": glibc[-1]}


def verify(data, baseline):
    if baseline not in BASELINES:
        raise ValueError(f"Unsupported Linux release baseline: {baseline}")
    abi = inspect(data)
    if abi["interpreter"] != INTERPRETER:
        raise ValueError(f"Unexpected Linux interpreter: {abi['interpreter']}")
    extra = set(abi["neededLibraries"]) - SYSTEM_LIBRARIES
    if extra or "libc.so.6" not in abi["neededLibraries"]:
        raise ValueError(f"Unexpected Linux shared libraries: {abi['neededLibraries']}")
    if version_tuple(abi["requiredGlibc"]) > version_tuple(baseline):
        raise ValueError(f"Binary requires glibc {abi['requiredGlibc']}, newer than its {baseline} baseline")
    return abi
