#!/usr/bin/env python3
"""Build the PyPI wheels for a release from its tarballs (docs/RELEASING.md):

    python3 tools/release/pypi.py VERSION DIST OUT [--target TRIPLE ...]

DIST holds alephgrid-VERSION-<target>.tar.gz (tools/release/package.sh). OUT receives one
binary-only wheel per target, alephgrid-<pep440 version>-py3-none-<platform>.whl, holding the
binary as alephgrid-<v>.data/scripts/alephgrid, so `pip install alephgrid`, `pipx install
alephgrid` and `uvx alephgrid` put the same `alephgrid` command on the PATH. No sdist: there is
nothing to build from source on the installing machine.

The platform tag is read from the binary, never below the release build's baseline: glibc
2.31 (Debian 11) for Linux, macOS 11.0 (MACOSX_DEPLOYMENT_TARGET) for macOS. A binary that
needs more (newer glibc symbol versions, a newer minimum macOS) gets the newer tag; a Linux
binary linking a library outside the manylinux set is refused.

Without --target every release target is required. Python 3.9+, standard library only. Zip
timestamps come from SOURCE_DATE_EPOCH (else 1980-01-01), so a re-run builds identical bytes.
"""

import argparse
import base64
import hashlib
import os
import re
import struct
import sys
import tarfile
import time
import zipfile

NAME = "alephgrid"
REPO = "https://github.com/alephgrid/alephgrid"
RAW = "https://raw.githubusercontent.com/alephgrid/alephgrid"
TARGETS = {
    # target triple: (binary format, CPU name in platform tags)
    "x86_64-unknown-linux-gnu": ("elf", "x86_64"),
    "aarch64-unknown-linux-gnu": ("elf", "aarch64"),
    "aarch64-apple-darwin": ("macho", "arm64"),
    "x86_64-apple-darwin": ("macho", "x86_64"),
}
# The release builds (.circleci/config.yml): Linux on debian:bullseye, macOS with
# MACOSX_DEPLOYMENT_TARGET=11.0.
LINUX_GLIBC_BASELINE = (2, 31)
MACOS_BASELINE = (11, 0)
# Libraries a manylinux wheel may link without bundling them (PEP 600 / auditwheel's policy).
MANYLINUX_LIBS = {
    "libc.so.6", "libm.so.6", "libdl.so.2", "librt.so.1", "libpthread.so.0", "libutil.so.1",
    "libresolv.so.2", "libnsl.so.1", "libcrypt.so.1", "libgcc_s.so.1", "libstdc++.so.6",
    "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1",
}
NOTICES = ["LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.md"]
SUMMARY = ("Open-source spreadsheet for data that doesn't fit in a spreadsheet: hundreds of "
           "millions of rows, Excel formulas, SQL when you want it.")
KEYWORDS = "spreadsheet,duckdb,big-data,sql,excel,csv,parquet,self-hosted"
CLASSIFIERS = [
    "Environment :: Web Environment",
    "Intended Audience :: Developers",
    "Intended Audience :: End Users/Desktop",
    "Intended Audience :: Science/Research",
    "Operating System :: MacOS",
    "Operating System :: POSIX :: Linux",
    "Programming Language :: Rust",
    "Topic :: Database",
    "Topic :: Office/Business :: Financial :: Spreadsheet",
]


def pep440(version):
    """The PyPI version for a release version (Cargo.toml / tag vX.Y.Z): 1.2.3 stays 1.2.3,
    1.2.3-rc.1 becomes 1.2.3rc1 (alpha -> a, beta -> b). Anything else is refused."""
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta|rc|a|b)\.?(\d+))?", version)
    if not m:
        raise SystemExit(f"cannot map release version {version!r} to a PyPI version "
                         "(expected X.Y.Z or X.Y.Z-alpha.N / -beta.N / -rc.N)")
    major, minor, patch, pre, num = m.groups()
    out = f"{int(major)}.{int(minor)}.{int(patch)}"
    if pre:
        out += {"alpha": "a", "a": "a", "beta": "b", "b": "b", "rc": "rc"}[pre] + str(int(num))
    return out


# ------------------------------------------------------------------ reading the binary

def elf_info(data):
    """(machine, highest GLIBC_x.y symbol version or None, DT_NEEDED libraries) of a
    little-endian 64-bit ELF executable."""
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError("not a little-endian 64-bit ELF file")
    machine = {62: "x86_64", 183: "aarch64"}.get(struct.unpack_from("<H", data, 0x12)[0])
    shoff, = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x3A)
    sections = []
    for i in range(shnum):
        (_name, typ, _flags, _addr, offset, size, link, info, _align, _entsize) = \
            struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * shentsize)
        sections.append((typ, offset, size, link, info))

    def string(section, at):
        start = sections[section][1] + at
        return data[start:data.index(b"\0", start)].decode()

    needed, glibc = [], []
    for typ, offset, size, link, info in sections:
        if typ == 6:  # SHT_DYNAMIC
            for at in range(offset, offset + size, 16):
                tag, val = struct.unpack_from("<qQ", data, at)
                if tag == 0:
                    break
                if tag == 1:  # DT_NEEDED
                    needed.append(string(link, val))
        elif typ == 0x6FFFFFFE:  # SHT_GNU_verneed
            at = offset
            for _ in range(info):
                _ver, cnt, _file, aux, nxt = struct.unpack_from("<HHIII", data, at)
                a = at + aux
                for _ in range(cnt):
                    _hash, _flags, _other, name, anext = struct.unpack_from("<IHHII", data, a)
                    m = re.fullmatch(r"GLIBC_(\d+)\.(\d+)(?:\.\d+)?", string(link, name))
                    if m:
                        glibc.append((int(m.group(1)), int(m.group(2))))
                    a += anext
                at += nxt
    return machine, (max(glibc) if glibc else None), needed


def macho_info(data):
    """(cpu, minimum macOS (major, minor)) of a thin 64-bit Mach-O executable."""
    if data[:4] != b"\xcf\xfa\xed\xfe":
        raise ValueError("not a thin little-endian 64-bit Mach-O file")
    cputype, _sub, _filetype, ncmds, _size = struct.unpack_from("<iiIII", data, 4)
    cpu = {0x01000007: "x86_64", 0x0100000C: "arm64"}.get(cputype)
    at, minos = 32, None
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", data, at)
        if cmd == 0x32:  # LC_BUILD_VERSION: platform, minos, sdk, ntools
            platform, version = struct.unpack_from("<II", data, at + 8)
            if platform == 1:  # PLATFORM_MACOS
                minos = version
        elif cmd == 0x24:  # LC_VERSION_MIN_MACOSX: version, sdk
            minos, = struct.unpack_from("<I", data, at + 8)
        at += cmdsize
    if minos is None:
        raise ValueError("no minimum macOS version in the binary")
    return cpu, (minos >> 16, (minos >> 8) & 0xFF)


def platform_tag(target, data):
    try:
        return _platform_tag(target, data)
    except (ValueError, IndexError, struct.error) as e:
        raise SystemExit(f"{target}: cannot read the binary: {e}")


def _platform_tag(target, data):
    fmt, arch = TARGETS[target]
    if fmt == "elf":
        machine, glibc, needed = elf_info(data)
        if machine != arch:
            raise SystemExit(f"{target}: the binary is for {machine}, not {arch}")
        extra = sorted(set(needed) - MANYLINUX_LIBS)
        if extra:
            raise SystemExit(f"{target}: links {', '.join(extra)}, which a manylinux wheel may "
                             "not assume is installed")
        major, minor = max(glibc or LINUX_GLIBC_BASELINE, LINUX_GLIBC_BASELINE)
        tag = f"manylinux_{major}_{minor}_{arch}"
        if (major, minor) <= (2, 17):
            tag += f".manylinux2014_{arch}"
        return tag, f"glibc {major}.{minor} (binary needs {'%d.%d' % glibc if glibc else 'none'}); " \
                    f"links {', '.join(needed) or 'nothing'}"
    cpu, minos = macho_info(data)
    if cpu != arch:
        raise SystemExit(f"{target}: the binary is for {cpu}, not {arch}")
    major, minor = max(minos, MACOS_BASELINE)
    if major >= 11:
        minor = 0  # from macOS 11, installers match the major version only
    return f"macosx_{major}_{minor}_{arch}", f"macOS {major}.{minor} (binary needs {minos[0]}.{minos[1]})"


# ------------------------------------------------------------------ the wheel

def long_description(readme, tag):
    """The README with relative links made absolute (at the release tag), so they work on
    pypi.org: images to raw files, other links to the file on GitHub."""
    def absolute(path, image):
        if re.match(r"^([a-z][a-z0-9+.-]*:|#|/)", path, re.I):
            return path
        return f"{RAW}/{tag}/{path}" if image else f"{REPO}/blob/{tag}/{path}"

    readme = re.sub(r'(<(?:img|source)\b[^>]*?\b(?:src|srcset)=")([^"]+)"',
                    lambda m: m.group(1) + absolute(m.group(2), True) + '"', readme)
    readme = re.sub(r'(<a\b[^>]*?\bhref=")([^"]+)"',
                    lambda m: m.group(1) + absolute(m.group(2), False) + '"', readme)
    readme = re.sub(r"(!?)\[([^\]]*)\]\(([^)\s]+)\)",
                    lambda m: f"{m.group(1)}[{m.group(2)}]({absolute(m.group(3), m.group(1) == '!')})",
                    readme)
    return readme


def metadata(version, readme):
    lines = [
        "Metadata-Version: 2.4",
        f"Name: {NAME}",
        f"Version: {version}",
        f"Summary: {SUMMARY}",
        f"Keywords: {KEYWORDS}",
        "Author: Alephgrid contributors",
        "License-Expression: Apache-2.0",
        *[f"License-File: {f}" for f in NOTICES],
        "Project-URL: Homepage, https://alephgrid.dev",
        "Project-URL: Documentation, https://alephgrid.dev/docs/getting-started/",
        f"Project-URL: Source, {REPO}",
        f"Project-URL: Issues, {REPO}/issues",
        f"Project-URL: Changelog, {REPO}/blob/main/CHANGELOG.md",
        *[f"Classifier: {c}" for c in CLASSIFIERS],
        "Description-Content-Type: text/markdown; charset=UTF-8; variant=GFM",
    ]
    return ("\n".join(lines) + "\n\n" + readme).encode()


def build_wheel(out, version, tag, files, epoch):
    """files: [(archive path, bytes, executable)] -> path of the written wheel."""
    dist_info = f"{NAME}-{version}.dist-info"
    wheel = ("Wheel-Version: 1.0\nGenerator: alephgrid tools/release/pypi.py\n"
             "Root-Is-Purelib: false\n" + "".join(f"Tag: py3-none-{t}\n" for t in tag.split("."))).encode()
    files = files + [(f"{dist_info}/WHEEL", wheel, False)]
    record = []
    for path, data, _ in files:
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        record.append(f"{path},sha256={digest},{len(data)}")
    record.append(f"{dist_info}/RECORD,,")
    files.append((f"{dist_info}/RECORD", ("\n".join(record) + "\n").encode(), False))

    stamp = time.gmtime(max(epoch, 315532800))[:6]  # zip cannot store dates before 1980
    path = os.path.join(out, f"{NAME}-{version}-py3-none-{tag}.whl")
    with zipfile.ZipFile(path, "w") as z:
        for name, data, executable in files:
            info = zipfile.ZipInfo(name, date_time=stamp)
            info.create_system = 3  # Unix, so installers honour the permission bits
            info.external_attr = (0o100755 if executable else 0o100644) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data, compresslevel=9)
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("version", help="the release version, as in Cargo.toml (X.Y.Z)")
    parser.add_argument("dist", help="folder with alephgrid-VERSION-TARGET.tar.gz")
    parser.add_argument("out", help="folder for the wheels (created)")
    parser.add_argument("--target", action="append", choices=sorted(TARGETS),
                        help="build only these targets (default: all, each required)")
    args = parser.parse_args()

    py_version = pep440(args.version)
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
    targets = args.target or list(TARGETS)
    for target in targets:
        tarball = os.path.join(args.dist, f"{NAME}-{args.version}-{target}.tar.gz")
        if not os.path.exists(tarball):
            raise SystemExit(f"missing {tarball}")
    os.makedirs(args.out, exist_ok=True)
    for old in os.listdir(args.out):  # never upload a wheel left over from another run
        if old.startswith(f"{NAME}-") and old.endswith(".whl"):
            os.remove(os.path.join(args.out, old))
    built = []
    for target in targets:
        tarball = os.path.join(args.dist, f"{NAME}-{args.version}-{target}.tar.gz")
        folder = f"{NAME}-{args.version}-{target}"
        with tarfile.open(tarball, "r:gz") as tar:
            def read(name):
                member = tar.getmember(f"{folder}/{name}")
                if not member.isfile():
                    raise SystemExit(f"{tarball}: {name} is not a regular file")
                return tar.extractfile(member).read()
            binary = read(NAME)
            notices = {f: read(f) for f in NOTICES}
            readme = read("README.md").decode()
        tag, why = platform_tag(target, binary)
        files = [(f"{NAME}-{py_version}.data/scripts/{NAME}", binary, True),
                 (f"{NAME}-{py_version}.dist-info/METADATA",
                  metadata(py_version, long_description(readme, f"v{args.version}")), False)]
        files += [(f"{NAME}-{py_version}.dist-info/licenses/{f}", notices[f], False) for f in NOTICES]
        path = build_wheel(args.out, py_version, tag, files, epoch)
        built.append(path)
        print(f"{os.path.basename(path)}  ({os.path.getsize(path) / 1e6:.1f} MB; {why})")
    print(f"{len(built)} wheel(s) for {NAME} {py_version} in {args.out}")


if __name__ == "__main__":
    sys.exit(main())
