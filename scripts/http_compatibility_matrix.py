"""Exercise released Python/Rust/Node clients against released and source servers.

Each client set uses installed distributions, never imports Python from the
checkout. Baseline native archives are pinned by SHA-256. No release is published.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tomllib
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RELEASES = ROOT / "tests/compatibility/releases.json"


def run(command, log, *, cwd=None, env=None, timeout=1200):
    with log.open("w", encoding="utf-8") as output:
        result = subprocess.run([str(arg) for arg in command], cwd=cwd, env=env,
                                stdout=output, stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"Command failed ({result.returncode}); see {log}")


def capture(command, *, cwd=None, env=None):
    return subprocess.check_output([str(arg) for arg in command], cwd=cwd, env=env,
                                   text=True, encoding="utf-8", timeout=30).strip()


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def unpack(archive, destination):
    destination.mkdir()
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as bundle:
            for item in bundle.infolist():
                if not (destination / item.filename).resolve().is_relative_to(destination.resolve()):
                    raise ValueError("Unsafe archive member")
            bundle.extractall(destination)
    else:
        with tarfile.open(archive) as bundle:
            bundle.extractall(destination, filter="data")


def python_environment(directory, distribution):
    run([sys.executable, "-m", "venv", directory], directory.parent / "venv.log")
    python = directory / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    run([python, "-m", "pip", "install", distribution, "pytest", "numpy",
         "-r", ROOT / "api/requirements.txt"], directory.parent / "python-install.log")
    return python


def rust_consumer(directory, version, work, source=False):
    (directory / "src").mkdir(parents=True)
    dependency = {"path": str(ROOT / "crates/recern-vector-client")} if source else {"version": "=" + version}
    spec = ", ".join(f"{key} = {json.dumps(value)}" for key, value in dependency.items())
    (directory / "Cargo.toml").write_text(
        '[package]\nname = "recern-compatibility-consumer"\nversion = "0.0.0"\n'
        'edition = "2024"\n\n[dependencies]\n'
        f'recern-vector-client = {{ {spec} }}\nserde_json = "1"\n', encoding="utf-8")
    shutil.copyfile(ROOT / "tests/compatibility/rust-client.rs", directory / "src/main.rs")
    env = {**os.environ, "CARGO_TARGET_DIR": str(work / "rust-target")}
    run(["cargo", "generate-lockfile"], directory / "resolve.log", cwd=directory, env=env)
    lock = tomllib.loads((directory / "Cargo.lock").read_text(encoding="utf-8"))
    client = [p for p in lock["package"] if p["name"] == "recern-vector-client"]
    assert len(client) == 1 and client[0]["version"] == version
    if not source:
        assert client[0]["source"] == "registry+https://github.com/rust-lang/crates.io-index"
    run(["cargo", "build", "--locked"], directory / "build.log", cwd=directory, env=env)
    suffix = ".exe" if os.name == "nt" else ""
    binary = directory / ("portable" + suffix)
    shutil.copy2(work / "rust-target/debug" / ("recern-compatibility-consumer" + suffix), binary)
    return binary


def prepare(args, work):
    releases = json.loads(RELEASES.read_text(encoding="utf-8"))
    clients, servers = [], []
    suffix = ".exe" if args.platform == "win32-x64" else ""
    for version in ("0.4.0", "0.5.0"):
        print(f"Preparing published {version} ({args.platform})", flush=True)
        directory = work / version
        directory.mkdir()
        archives = releases[version][args.platform]
        for kind, item in archives.items():
            archive = directory / item["file"]
            url = f"https://github.com/recerndata/recern-vector/releases/download/v{version}/{item['file']}"
            run(["curl", "--fail", "--location", "--silent", "--show-error", "--retry", "2",
                 "--max-time", "180", "--output", archive, url], directory / (kind + "-download.log"))
            assert sha256(archive) == item["sha256"], item["file"]
            unpack(archive, directory / kind)
        server = directory / "native" / f"recern-vector-{version}-{args.platform}" / "bin" / ("recern-vector-server" + suffix)
        node = directory / "node/package"
        python = python_environment(directory / "python", f"recern-vector=={version}")
        rust = rust_consumer(directory / "rust", version, work)
        clients.append({"label": version, "version": version, "python": str(python), "rust": str(rust), "node": str(node)})
        servers.append({"label": version, "version": version, "binary": str(server)})
    if args.source_wheel:
        print("Preparing the current source candidate", flush=True)
        from release_version import python_version, workspace_version
        directory = work / "source"
        directory.mkdir()
        python = python_environment(directory / "python", str(Path(args.source_wheel).resolve()))
        rust = rust_consumer(directory / "rust", workspace_version(), work, source=True)
        clients.append({"label": "source", "version": workspace_version(), "python_version": python_version(),
                        "python": str(python), "rust": str(rust), "node": str(Path(args.source_node).resolve())})
        servers.append({"label": "source", "version": workspace_version(), "binary": str(Path(args.source_server).resolve())})
    matrix = {"platform": args.platform, "clients": clients, "servers": servers}
    (work / "matrix.json").write_text(json.dumps(matrix, indent=2) + "\n", encoding="utf-8")
    return matrix


def verify_installed(matrix, work):
    for server in matrix["servers"]:
        version = capture([server["binary"], "--version"]).split()[-1]
        assert version == server["version"], server["label"]
    for client in matrix["clients"]:
        code = "import importlib.metadata,json,recern_vector; print(json.dumps({'version':importlib.metadata.version('recern-vector'),'path':recern_vector.__file__}))"
        installed = json.loads(capture([client["python"], "-c", code], cwd=work))
        assert installed["version"] == client.get("python_version", client["version"])
        assert not Path(installed["path"]).resolve().is_relative_to(ROOT)
        code = "const p=require(process.argv[1]); process.stdout.write(p.version());"
        assert capture(["node", "-e", code, client["node"]], cwd=work) == client["version"]


def exercise(matrix, work):
    verify_installed(matrix, work)
    results = []
    for server in matrix["servers"]:
        for client in matrix["clients"]:
            name = f"client-{client['label']}-server-{server['label']}"
            directory = work / name
            directory.mkdir(exist_ok=True)
            print(f"Testing {name}: Python, Rust and Node", flush=True)
            env = {**os.environ, "PYTHONPATH": "", "PYTHONNOUSERSITE": "1", "PYTHONUTF8": "1",
                   "RECERN_TEST_SERVER_BIN": server["binary"], "RECERN_TEST_CLIENT_BIN": client["rust"],
                   "RECERN_TEST_NODE_PORTABLE": str(Path(client["node"]) / "portable")}
            remote_tests = [
                "test_remote_generation_noops_restart_and_reconnect",
                "test_remote_snapshot_lifetime_and_export_cleanup",
                "test_remote_snapshot_expiry",
                "test_import_rechecks_database_identity_before_any_write",
                "test_remote_transport_is_bounded_and_does_not_retry",
                "test_remote_configuration_rejected_before_network",
            ]
            command = [client["python"], "-m", "pytest", "-q", "-o", "addopts=",
                       "--basetemp", directory / "temporary", "--junitxml", directory / "results.xml",
                       *[str(ROOT / "api/tests/test_remote.py") + "::" + name for name in remote_tests],
                       ROOT / "api/tests/test_release_matrix.py",
                       str(ROOT / "api/tests/test_sdk_interop.py") + "::test_python_rust_node_data_and_migration[remote]"]
            run(command, directory / "pytest.log", cwd=work, env=env, timeout=180)
            suites = ET.parse(directory / "results.xml").getroot()
            count = sum(int(s.get("tests", "0")) for s in suites.iter("testsuite"))
            assert count >= 17, "Expected the complete public-API and cross-language contract suite"
            for suite in suites.iter("testsuite"):
                assert all(int(suite.get(key, "0")) == 0 for key in ("skipped", "failures", "errors"))
            results.append({"client": client["label"], "client_version": client["version"],
                            "server": server["label"], "server_version": server["version"],
                            "languages": ["Python", "Rust", "Node.js"], "tests": count, "result": "passed"})
    report = {"schema": 1, "platform": matrix["platform"],
              "source_commit": capture(["git", "rev-parse", "HEAD"], cwd=ROOT),
              "source_dirty": bool(capture(["git", "status", "--porcelain"], cwd=ROOT)),
              "scope": "HTTP profile v1; public Python remote contract and three-language data/migration roundtrip",
              "cells": results}
    (work / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Passed {len(results)} client/server cells, {sum(cell['tests'] for cell in results)} tests; no skips", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--platform", choices=["linux-x64", "darwin-arm64", "win32-x64"])
    phase = parser.add_mutually_exclusive_group()
    phase.add_argument("--run-only", action="store_true")
    phase.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--source-wheel")
    parser.add_argument("--source-server")
    parser.add_argument("--source-node")
    args = parser.parse_args()
    work = args.work.resolve()
    if args.run_only:
        matrix = json.loads((work / "matrix.json").read_text(encoding="utf-8"))
    else:
        if not args.platform:
            parser.error("--platform is required when preparing a matrix")
        host = capture(["node", "-p", "process.platform + '-' + process.arch"])
        if host != args.platform:
            parser.error(f"--platform {args.platform} does not match Node host {host}")
        if any([args.source_wheel, args.source_server, args.source_node]) and not all([args.source_wheel, args.source_server, args.source_node]):
            parser.error("the three --source-* options must be provided together")
        work.mkdir(parents=True, exist_ok=False)
        matrix = prepare(args, work)
    if not args.prepare_only:
        exercise(matrix, work)


if __name__ == "__main__":
    main()
