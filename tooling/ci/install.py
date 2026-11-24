#!/usr/bin/env python3
"""Install checksum-pinned Linux/x86-64 validators; no package manager or root needed."""

import argparse
import hashlib
import json
from pathlib import Path
import os
import platform
import re
import shlex
import shutil
import sys
import tarfile
import urllib.request
import zipfile


HERE = Path(__file__).resolve().parent
DEFAULT_PREFIX = Path.home() / ".local/share/rift-ci-tools/public-ci-v1"


def checked_download(spec, directory):
    archive = directory / spec["url"].rsplit("/", 1)[1]
    if not archive.exists():
        temporary = archive.with_suffix(archive.suffix + ".part")
        try:
            with urllib.request.urlopen(spec["url"], timeout=120) as response:
                with temporary.open("wb") as output:
                    shutil.copyfileobj(response, output)
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != spec["sha256"]:
        raise ValueError(f"Checksum mismatch: {archive}; remove it before retrying")
    return archive


def install(prefix):
    if sys.platform != "linux" or platform.machine() not in ("x86_64", "AMD64"):
        raise ValueError("Pinned releases support Linux x86-64 (glibc >= 2.28) only")
    try:
        libc = os.confstr("CS_GNU_LIBC_VERSION")
    except (OSError, ValueError):
        libc = None
    version = re.fullmatch(r"glibc (\d+)\.(\d+)", libc or "")
    if version is None or tuple(map(int, version.groups())) < (2, 28):
        raise ValueError("Pinned releases require a detected host glibc >= 2.28")
    tools = json.loads((HERE / "tools.json").read_text())["tools"]
    binaries = prefix / "bin"
    downloads = prefix / "downloads"
    binaries.mkdir(parents=True, exist_ok=True)
    downloads.mkdir(parents=True, exist_ok=True)
    for name, spec in tools.items():
        archive = checked_download(spec, downloads)
        destination = binaries / name
        if name == "pyflakes":
            packages = prefix / "python-packages"
            packages.mkdir(exist_ok=True)
            with zipfile.ZipFile(archive) as wheel:
                for member in wheel.namelist():
                    # Extract only the import package, never wheel entrypoint metadata.
                    if member.startswith("pyflakes/") and not member.endswith("/"):
                        relative = Path(member)
                        if ".." in relative.parts:
                            raise ValueError("Unsafe wheel member")
                        target = packages / relative
                        target.parent.mkdir(parents=True, exist_ok=True)
                        target.write_bytes(wheel.read(member))
            destination.write_text(
                "#!/bin/sh\nexec " + shlex.quote(sys.executable)
                + " -I -c " + shlex.quote(
                    "import sys; sys.path.insert(0, " + repr(str(packages))
                    + "); from pyflakes.api import main; main()"
                ) + ' "$@"\n'
            )
        elif archive.suffix == ".whl":
            with zipfile.ZipFile(archive) as wheel:
                destination.write_bytes(wheel.read(spec["member"]))
        else:
            with tarfile.open(archive, "r:gz") as bundle:
                member = bundle.getmember(spec["member"])
                if not member.isfile():
                    raise ValueError(f"Not a regular executable: {spec['member']}")
                with bundle.extractfile(member) as source, destination.open("wb") as output:
                    shutil.copyfileobj(source, output)
        destination.chmod(0o755)
        print(f"Installed {name} {spec['version']}")
    print(f"Validator binaries: {binaries}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, default=DEFAULT_PREFIX)
    args = parser.parse_args()
    install(args.prefix.expanduser().resolve())


if __name__ == "__main__":
    main()
