#!/usr/bin/env python3
"""Validate the product's pinned Harness and model SDK source identities."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[2]
HARNESS_URL = "https://github.com/lingxi-coder/harness-runtime"
SDK_URL = "https://github.com/lingxi-coder/llm-client"
FULL_COMMIT = re.compile(r"[0-9a-f]{40}\Z")
REQUIRED_HARNESS_PACKAGES = frozenset({"core", "secret", "telemetry"})
RUNTIME_LAYOUT = Path("crates/llm-runtime/Cargo.toml")


class SourceIdentityError(ValueError):
    """A source pin or its resolved Cargo identity is unsafe or inconsistent."""


def _repo_identity(source):
    if not isinstance(source, str):
        return None
    value = source[4:] if source.startswith("git+") else source
    value = value.split("?", 1)[0].split("#", 1)[0]
    parsed = urlsplit(value)
    hostname = parsed.hostname
    path = parsed.path
    if hostname is None and "@" in value and ":" in value:
        # Also recognize Git's SCP-style spelling when checking for an
        # alternate transport to one of the pinned repositories.
        authority, path = value.split(":", 1)
        hostname = authority.rsplit("@", 1)[-1]
    if hostname is None:
        return None
    path = path.rstrip("/")
    if path.endswith(".git"):
        path = path[:-4]
    return hostname.lower(), path


def _is_repo_family(value, canonical_url):
    return _repo_identity(value) == _repo_identity(canonical_url)


def _check_harness_package_path(package, source_root):
    expected = source_root / "crates" / package["name"] / "Cargo.toml"
    actual = Path(package.get("manifest_path", "")).resolve()
    if actual != expected.resolve():
        raise SourceIdentityError(
            f"Harness package {package['name']} is outside the expected checkout layout"
        )


def _pin(workspace_dependencies, name, canonical_url):
    dependency = workspace_dependencies.get(name)
    if not isinstance(dependency, dict):
        raise SourceIdentityError(f"workspace dependency {name} must be a Git pin")
    if dependency.get("path") is not None:
        raise SourceIdentityError(f"workspace dependency {name} must not use a path override")
    if dependency.get("git") != canonical_url:
        raise SourceIdentityError(
            f"workspace dependency {name} must use canonical Git URL {canonical_url}"
        )
    revision = dependency.get("rev")
    if not isinstance(revision, str) or FULL_COMMIT.fullmatch(revision) is None:
        raise SourceIdentityError(f"workspace dependency {name} must pin a full lowercase commit SHA")
    if dependency.get("branch") is not None or dependency.get("tag") is not None:
        raise SourceIdentityError(f"workspace dependency {name} cannot combine rev with branch/tag")
    return revision, f"git+{canonical_url}?rev={revision}#{revision}"


def _single_package(packages_by_name, name, expected_source):
    matches = packages_by_name.get(name, [])
    if len(matches) != 1:
        raise SourceIdentityError(
            f"Cargo metadata must contain exactly one {name} package identity; found {len(matches)}"
        )
    package = matches[0]
    if package.get("source") != expected_source:
        raise SourceIdentityError(
            f"{name} resolved from {package.get('source')!r}, expected {expected_source!r}"
        )
    return package


def _runtime_root(package):
    manifest_path = Path(package.get("manifest_path", "")).resolve()
    root = manifest_path.parents[len(RUNTIME_LAYOUT.parts) - 1]
    try:
        actual_layout = manifest_path.relative_to(root)
    except ValueError as error:
        raise SourceIdentityError("llm-runtime manifest is outside its resolved Harness checkout") from error
    if actual_layout.as_posix() != RUNTIME_LAYOUT.as_posix():
        raise SourceIdentityError(
            f"unexpected llm-runtime checkout layout: {actual_layout.as_posix()!r}"
        )
    return root


def _reachable(nodes_by_id, starts, packages_by_id):
    reached = set()
    pending = list(starts)
    while pending:
        package_id = pending.pop()
        if package_id in reached:
            continue
        node = nodes_by_id.get(package_id)
        if node is None:
            raise SourceIdentityError(f"resolved dependency graph is missing package node {package_id!r}")
        if package_id not in packages_by_id:
            raise SourceIdentityError(f"resolved dependency graph references unknown package {package_id!r}")
        reached.add(package_id)
        for dependency in node.get("deps", []):
            target = dependency.get("pkg")
            if not isinstance(target, str):
                raise SourceIdentityError(f"malformed resolved dependency edge from {package_id!r}")
            pending.append(target)
    return reached


def inspect_metadata(manifest, metadata):
    """Validate pins, resolved package sources, identity uniqueness and graph reachability."""
    try:
        workspace_dependencies = manifest["workspace"]["dependencies"]
    except (KeyError, TypeError) as error:
        raise SourceIdentityError("Cargo.toml is missing workspace.dependencies") from error

    harness_revision, harness_source = _pin(
        workspace_dependencies, "llm-runtime", HARNESS_URL
    )
    sdk_revision, sdk_source = _pin(
        workspace_dependencies, "lingxi-llm-client", SDK_URL
    )

    # No other direct dependency may introduce a second pin for either source.
    for name, dependency in workspace_dependencies.items():
        if not isinstance(dependency, dict):
            continue
        url = dependency.get("git")
        revision = dependency.get("rev")
        if _is_repo_family(url, HARNESS_URL):
            if url != HARNESS_URL or revision != harness_revision:
                raise SourceIdentityError(
                    f"workspace dependency {name} mixes Harness source URLs or revisions"
                )
        if _is_repo_family(url, SDK_URL):
            if url != SDK_URL or revision != sdk_revision:
                raise SourceIdentityError(
                    f"workspace dependency {name} mixes SDK source URLs or revisions"
                )

    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    if not isinstance(packages, list) or not isinstance(resolve, dict):
        raise SourceIdentityError("Cargo metadata must include packages and a resolved dependency graph")

    packages_by_id = {}
    packages_by_name = {}
    for package in packages:
        if not isinstance(package, dict):
            raise SourceIdentityError("Cargo metadata contains a malformed package record")
        package_id = package.get("id")
        name = package.get("name")
        if not isinstance(package_id, str) or not isinstance(name, str):
            raise SourceIdentityError("Cargo metadata contains a package without an id or name")
        if package_id in packages_by_id:
            raise SourceIdentityError(f"Cargo metadata repeats package id {package_id!r}")
        packages_by_id[package_id] = package
        packages_by_name.setdefault(name, []).append(package)

    runtime = _single_package(packages_by_name, "llm-runtime", harness_source)
    sdk = _single_package(packages_by_name, "lingxi-llm-client", sdk_source)
    source_root = _runtime_root(runtime)

    harness_packages = []
    for package in packages:
        if not _is_repo_family(package.get("source"), HARNESS_URL):
            continue
        if package.get("source") != harness_source:
            raise SourceIdentityError(
                f"Harness package {package['name']} has a mixed source pin: {package.get('source')!r}"
            )
        _check_harness_package_path(package, source_root)
        harness_packages.append(package)

    for package in packages:
        source = package.get("source")
        if _is_repo_family(source, SDK_URL) and source != sdk_source:
            raise SourceIdentityError(
                f"SDK package {package['name']} has a mixed source pin: {source!r}"
            )

    if not harness_packages:
        raise SourceIdentityError("Cargo metadata has no resolved Harness source packages")
    harness_names = {package["name"] for package in harness_packages}
    missing = REQUIRED_HARNESS_PACKAGES - harness_names
    if missing:
        raise SourceIdentityError(
            "Cargo metadata is missing required llm-runtime dependencies from Harness: "
            + ", ".join(sorted(missing))
        )

    # A name used from the pinned Harness source cannot also resolve to a local,
    # registry, or second Git identity in the same product build.
    for name in sorted(harness_names | {"llm-runtime", "lingxi-llm-client"}):
        matches = packages_by_name.get(name, [])
        if len(matches) != 1:
            raise SourceIdentityError(
                f"duplicate Cargo package identity for {name}: found {len(matches)} packages"
            )
        expected = sdk_source if name == "lingxi-llm-client" else harness_source
        if matches[0].get("source") != expected:
            raise SourceIdentityError(
                f"{name} has a mixed source identity: {matches[0].get('source')!r}"
            )

    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        raise SourceIdentityError("Cargo metadata has no resolved dependency nodes")
    nodes_by_id = {}
    for node in nodes:
        if not isinstance(node, dict):
            raise SourceIdentityError("Cargo metadata contains a malformed dependency node")
        node_id = node.get("id")
        if not isinstance(node_id, str) or node_id in nodes_by_id:
            raise SourceIdentityError(f"Cargo metadata repeats or omits resolved node id {node_id!r}")
        nodes_by_id[node_id] = node

    runtime_id = runtime["id"]
    sdk_id = sdk["id"]
    workspace_members = metadata.get("workspace_members")
    if not isinstance(workspace_members, list) or not workspace_members:
        raise SourceIdentityError("Cargo metadata has no workspace members for reachability checks")
    workspace_reachable = _reachable(nodes_by_id, workspace_members, packages_by_id)
    if runtime_id not in workspace_reachable:
        raise SourceIdentityError("llm-runtime is listed but unreachable from product workspace members")

    runtime_reachable = _reachable(nodes_by_id, [runtime_id], packages_by_id)
    if sdk_id not in runtime_reachable:
        raise SourceIdentityError("lingxi-llm-client is not reachable from the resolved llm-runtime")
    for package in harness_packages:
        if package["id"] not in runtime_reachable:
            raise SourceIdentityError(
                f"Harness package {package['name']} is listed but unreachable from llm-runtime"
            )

    # A reachable path package from the model-runtime graph is a path overlay,
    # even when a precomputed metadata file omits it from the workspace pins.
    for package_id in runtime_reachable:
        package = packages_by_id[package_id]
        source = package.get("source")
        if source is None:
            raise SourceIdentityError(
                f"path overlay {package['name']} is reachable from llm-runtime"
            )
        if _is_repo_family(source, HARNESS_URL):
            if source != harness_source:
                raise SourceIdentityError(
                    f"reachable Harness package {package['name']} uses a mixed revision"
                )
            _check_harness_package_path(package, source_root)
        if _is_repo_family(source, SDK_URL) and source != sdk_source:
            raise SourceIdentityError(
                f"reachable SDK package {package['name']} uses a mixed revision"
            )

    return {
        "harness_revision": harness_revision,
        "sdk_revision": sdk_revision,
        "harness_package_names": sorted(harness_names),
        "harness_checkout": str(source_root),
    }


def _read_metadata(path):
    if path is not None:
        try:
            return json.loads(Path(path).read_text(encoding="utf-8")), True
        except (OSError, json.JSONDecodeError) as error:
            raise SourceIdentityError(f"cannot read supplied Cargo metadata JSON: {error}") from error

    command = [
        "cargo",
        "metadata",
        "--locked",
        "--offline",
        "--format-version",
        "1",
        "--all-features",
        "--manifest-path",
        str(ROOT / "Cargo.toml"),
    ]
    try:
        result = subprocess.run(
            command, cwd=ROOT, check=True, capture_output=True, text=True, encoding="utf-8"
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = getattr(error, "stderr", None)
        if detail:
            detail = detail.strip()
        raise SourceIdentityError(
            "offline cargo metadata failed without fetching dependencies"
            + (f": {detail}" if detail else "")
        ) from error
    try:
        return json.loads(result.stdout), False
    except json.JSONDecodeError as error:
        raise SourceIdentityError(f"cargo metadata returned invalid JSON: {error}") from error


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--metadata",
        metavar="JSON",
        help="validate an existing cargo metadata JSON file without invoking Cargo",
    )
    args = parser.parse_args(argv)
    try:
        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        metadata, supplied = _read_metadata(args.metadata)
        result = inspect_metadata(manifest, metadata)
    except (OSError, tomllib.TOMLDecodeError, SourceIdentityError, KeyError, TypeError) as error:
        print(f"model-source: FAIL: {error}", file=sys.stderr)
        print("model-source: local metadata cannot prove remote publication.", file=sys.stderr)
        return 1

    print(
        "model-source: PASS: "
        f"Harness {result['harness_revision']} ({len(result['harness_package_names'])} reachable packages); "
        f"SDK {result['sdk_revision']}"
    )
    if supplied:
        print("model-source: used supplied metadata JSON; Cargo was not invoked")
    else:
        print("model-source: metadata came from cargo metadata --locked --offline --all-features")
    print("model-source: remote publication is not verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
