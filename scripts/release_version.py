"""Read the workspace version and validate installed Python distributions."""
import argparse
import importlib.metadata
from pathlib import Path
import re
import tomllib


def workspace_version():
    root = Path(__file__).resolve().parents[1]
    return tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]


def python_version():
    version = workspace_version()
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(dev|alpha|beta|rc)\.(\d+))?", version)
    if not match:
        raise ValueError(f"unsupported release version: {version}")
    suffix = {"dev": ".dev", "alpha": "a", "beta": "b", "rc": "rc"}
    return match[1] + (suffix[match[2]] + match[3] if match[2] else "")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", action="store_true")
    parser.add_argument("--check-installed", action="store_true")
    args = parser.parse_args()
    if args.check_installed:
        actual = importlib.metadata.version("recern-vector")
        expected = python_version()
        if actual != expected:
            raise SystemExit(f"installed recern-vector {actual}, expected {expected}")
        print(actual)
    else:
        print(python_version() if args.python else workspace_version())
