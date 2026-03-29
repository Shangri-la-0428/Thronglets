"""Thronglets — P2P shared memory substrate for AI agents."""

import platform
import stat
import subprocess
import sys
import urllib.request
from pathlib import Path

import os

__version__ = "0.4.1"

VERSION = os.environ.get("THRONGLETS_INSTALL_VERSION", __version__)
REPO = os.environ.get("THRONGLETS_INSTALL_REPO", "Shangri-la-0428/Thronglets")

PLATFORMS = {
    ("Darwin", "arm64"): "thronglets-mcp-darwin-arm64",
    ("Linux", "x86_64"): "thronglets-mcp-linux-amd64",
}


def _bin_dir() -> Path:
    return Path(__file__).parent / "bin"


def _bin_path() -> Path:
    return _bin_dir() / "thronglets-bin"


def _download_binary() -> Path:
    system = platform.system()
    machine = platform.machine()
    key = (system, machine)

    asset = PLATFORMS.get(key)
    if not asset:
        print(f"Unsupported platform: {system}-{machine}", file=sys.stderr)
        print("Install from source: cargo install thronglets", file=sys.stderr)
        sys.exit(1)

    url = f"https://github.com/{REPO}/releases/download/v{VERSION}/{asset}"
    dest = _bin_path()
    dest.parent.mkdir(parents=True, exist_ok=True)

    print(f"Downloading thronglets v{VERSION} for {system}-{machine}...")
    urllib.request.urlretrieve(url, dest)
    dest.chmod(dest.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    print("Thronglets installed successfully.")
    return dest


def _ensure_binary() -> Path:
    bin_path = _bin_path()
    if not bin_path.exists():
        _download_binary()
    return bin_path


def main():
    """Entry point for the thronglets CLI."""
    binary = _ensure_binary()
    result = subprocess.run([str(binary)] + sys.argv[1:])
    sys.exit(result.returncode)
