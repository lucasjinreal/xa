"""PEP 517 backend for a wheel that contains one native Rust executable.

Setuptools' ``scripts`` support reads every script as Python source, which is
not valid for an ELF, Mach-O, or PE executable.  This tiny backend writes the
standard wheel layout directly, putting the executable in ``.data/scripts``.
Pip then copies that exact file to the active environment's scripts directory.
"""

from __future__ import annotations

import base64
import hashlib
import os
import re
import stat
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
NAME = "xacli"


def _version() -> str:
    cargo_toml = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.M)
    if not match:
        raise RuntimeError("could not read package version from Cargo.toml")
    return match.group(1)


def _inputs() -> tuple[Path, str]:
    binary = os.environ.get("XA_BINARY")
    platform = os.environ.get("XA_WHEEL_PLAT")
    if not binary or not platform:
        raise RuntimeError("XA_BINARY and XA_WHEEL_PLAT must be set")
    binary_path = Path(binary).resolve()
    if not binary_path.is_file():
        raise RuntimeError(f"XA_BINARY does not exist or is not a file: {binary_path}")
    return binary_path, platform


def _metadata(version: str) -> bytes:
    return (
        "Metadata-Version: 2.3\n"
        f"Name: {NAME}\n"
        f"Version: {version}\n"
        "Summary: xa — a fast, native Rust coding-agent CLI\n"
        "Requires-Python: >=3.9\n"
        "License-Expression: MIT\n"
        "Project-URL: Homepage, https://github.com/jinfagang/xa\n"
        "Project-URL: Repository, https://github.com/jinfagang/xa\n"
        "\n"
    ).encode()


def _wheel_metadata(platform: str) -> bytes:
    return (
        "Wheel-Version: 1.0\n"
        "Generator: xacli-native-wheel\n"
        "Root-Is-Purelib: false\n"
        f"Tag: py3-none-{platform}\n"
    ).encode()


def _record_row(path: str, content: bytes) -> list[str]:
    digest = base64.urlsafe_b64encode(hashlib.sha256(content).digest()).rstrip(b"=")
    return [path, f"sha256={digest.decode()}", str(len(content))]


def _write(zf: zipfile.ZipFile, path: str, content: bytes, *, executable: bool = False) -> None:
    info = zipfile.ZipInfo(path)
    info.compress_type = zipfile.ZIP_DEFLATED
    mode = stat.S_IFREG | (0o755 if executable else 0o644)
    info.external_attr = mode << 16
    zf.writestr(info, content)


def build_wheel(
    wheel_directory: str, config_settings: dict[str, object] | None = None,
    metadata_directory: str | None = None,
) -> str:
    del config_settings, metadata_directory
    binary, platform = _inputs()
    version = _version()
    dist_info = f"{NAME}-{version}.dist-info"
    script_name = binary.name
    wheel_name = f"{NAME}-{version}-py3-none-{platform}.whl"
    output = Path(wheel_directory) / wheel_name
    output.parent.mkdir(parents=True, exist_ok=True)

    files = [
        (f"{NAME}-{version}.data/scripts/{script_name}", binary.read_bytes(), True),
        (f"{dist_info}/METADATA", _metadata(version), False),
        (f"{dist_info}/WHEEL", _wheel_metadata(platform), False),
    ]
    record = [_record_row(path, content) for path, content, _ in files]
    record.append([f"{dist_info}/RECORD", "", ""])
    record_bytes = "".join(",".join(row) + "\n" for row in record).encode("utf-8")

    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        for path, content, executable in files:
            _write(zf, path, content, executable=executable)
        _write(zf, f"{dist_info}/RECORD", record_bytes)
    return wheel_name


def prepare_metadata_for_build_wheel(
    metadata_directory: str, config_settings: dict[str, object] | None = None,
) -> str:
    del config_settings
    version = _version()
    dist_info = f"{NAME}-{version}.dist-info"
    directory = Path(metadata_directory) / dist_info
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "METADATA").write_bytes(_metadata(version))
    return dist_info


def get_requires_for_build_wheel(
    config_settings: dict[str, object] | None = None,
) -> list[str]:
    del config_settings
    return []
