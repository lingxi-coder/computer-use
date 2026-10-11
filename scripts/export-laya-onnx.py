#!/usr/bin/env python3
"""Export the pinned Laya checkpoint using a verified upstream source checkout."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
from datetime import datetime, timezone


REVISION = "e4e9ddf21a7b1903b7acffd8814ad4307bf63a67"
SOURCE_COMMIT = "2e4d9c87e8b1621deb344eac7de5c7258f32f849"
EXPORTER_BLOB = "d82f7f28c70cd884ea0c48e0dddf4648612bc3cf"
COMMON_BLOB = "1cf74f2665e31a527f13d4ee0747b0bf73c6feb2"
STRICT_LOAD_LINE = "    model.load_state_dict(_load_safetensors(weights_path), strict=False)\n"
PINNED_FILES = {
    "model.safetensors": {
        "bytes": 643835514,
        "sha256": "9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204",
    },
    "tokenizer/tokenizer.json": {
        "bytes": 34363188,
        "sha256": "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f",
    },
    "rl_agent_config.json": {
        "bytes": 472,
        "git_blob_sha1": "00e35f88bb731bb9126a914666ab1cdac8a204c8",
    },
    "encoder/config.json": {
        "bytes": 1938,
        "git_blob_sha1": "0de0e2d30638873790cf962def52e2acf4db3eef",
    },
}


def sha256_file(path: Path) -> tuple[int, str]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            size += len(block)
            digest.update(block)
    return size, digest.hexdigest()


def git_blob_sha1(path: Path) -> str:
    size = path.stat().st_size
    digest = hashlib.sha1(f"blob {size}\0".encode("ascii"))
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def load_and_verify_manifest(root: Path) -> tuple[Path, dict]:
    manifest_path = root / "assets/model-manifests/laya-multilingual-base.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    require(manifest.get("model_id") == "convaiinnovations/laya-multilingual", "input manifest model id mismatch")
    require(manifest.get("revision") == REVISION, "input manifest revision mismatch")
    recorded = {item["path"]: item for item in manifest.get("files", [])}
    require(set(recorded) == set(PINNED_FILES), "input manifest file set mismatch")
    for name, expected in PINNED_FILES.items():
        item = recorded[name]
        require(item.get("bytes") == expected["bytes"], f"input manifest size mismatch: {name}")
        for key, value in expected.items():
            require(item.get(key) == value, f"input manifest {key} mismatch: {name}")

    input_dir = root / "target/model-resources/laya" / REVISION
    require(input_dir.is_dir() and not input_dir.is_symlink(), "pinned input directory missing or symlinked")
    for name, expected in PINNED_FILES.items():
        path = input_dir / name
        require(path.is_file() and not path.is_symlink(), f"pinned input is not a regular file: {name}")
        size, sha = sha256_file(path)
        require(size == expected["bytes"], f"pinned input size mismatch: {name}")
        if "sha256" in expected:
            require(sha == expected["sha256"], f"pinned input SHA-256 mismatch: {name}")
        if "git_blob_sha1" in expected:
            require(git_blob_sha1(path) == expected["git_blob_sha1"], f"pinned input Git blob mismatch: {name}")
    return input_dir, manifest


def verify_source(source_dir: Path) -> tuple[Path, str]:
    source_dir = source_dir.resolve(strict=True)
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=source_dir, check=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    ).stdout.strip()
    require(commit == SOURCE_COMMIT, "upstream source checkout commit mismatch")
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"], cwd=source_dir,
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    ).stdout
    require(not status, "upstream source checkout is dirty")
    exporter = source_dir / "laya-ts/scripts/export_onnx.py"
    require(exporter.is_file(), "pinned TypeScript ONNX exporter is missing")
    require(git_blob_sha1(exporter) == EXPORTER_BLOB, "pinned exporter Git blob mismatch")
    common = source_dir / "laya/common.py"
    require(common.is_file() and git_blob_sha1(common) == COMMON_BLOB, "pinned local model builder mismatch")
    return exporter, commit


def write_json(path: Path, value: dict) -> None:
    temporary = path.with_name(path.name + ".part")
    with temporary.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def patch_strict_load(source: Path, destination: Path) -> str:
    text = source.read_text(encoding="utf-8")
    require(text.count(STRICT_LOAD_LINE) == 1, "strict=False load statement did not occur exactly once")
    replacement = (
        "    incompatible = model.load_state_dict(_load_safetensors(weights_path), strict=False)\n"
        "    missing_keys = list(incompatible.missing_keys)\n"
        "    unexpected_keys = list(incompatible.unexpected_keys)\n"
        "    print(\"checkpoint_load_facts=\" + json.dumps({\"missing_keys\": missing_keys, \"unexpected_keys\": unexpected_keys}), flush=True)\n"
        "    if missing_keys or unexpected_keys:\n"
        "        raise RuntimeError(\"checkpoint learned-weight key mismatch: \" + json.dumps({\"missing_keys\": missing_keys, \"unexpected_keys\": unexpected_keys}))\n"
    )
    patched = text.replace(STRICT_LOAD_LINE, replacement, 1)
    destination.write_text(patched, encoding="utf-8")
    return hashlib.sha256(destination.read_bytes()).hexdigest()


def make_working_checkpoint(input_dir: Path, output_dir: Path) -> dict:
    checkpoint = output_dir / "checkpoint"
    checkpoint.mkdir(parents=True)
    (checkpoint / "encoder").mkdir()
    (checkpoint / "tokenizer").mkdir()
    weight_copy = output_dir / "readonly-weight-copy.safetensors"
    shutil.copyfile(input_dir / "model.safetensors", weight_copy)
    weight_copy.chmod(stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
    os.link(weight_copy, checkpoint / "model.safetensors")
    for name in ("rl_agent_config.json", "encoder/config.json", "tokenizer/tokenizer.json"):
        destination = checkpoint / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(input_dir / name, destination)
    return {
        "checkpoint_dir": checkpoint,
        "readonly_weight_copy": weight_copy,
        "weight_copy_sha256": sha256_file(weight_copy)[1],
    }


def export(args: argparse.Namespace) -> int:
    root = repo_root()
    input_dir, input_manifest = load_and_verify_manifest(root)
    exporter, commit = verify_source(Path(args.source_dir))
    python_path = Path(args.python).resolve(strict=True)
    require(python_path.is_file() and os.access(python_path, os.X_OK), "--python must name an executable interpreter")
    output_dir = Path(args.output_dir).resolve()
    target_root = (root / "target/model-exports").resolve()
    require(output_dir != target_root and target_root in output_dir.parents, "--output-dir must be below repository target/model-exports")

    if args.check_only:
        print(json.dumps({
            "status": "check_only_passed",
            "model_revision": REVISION,
            "source_commit": commit,
            "exporter_git_blob_sha1": EXPORTER_BLOB,
            "input_files": list(PINNED_FILES),
            "python": str(python_path),
            "export_ran": False,
        }, indent=2))
        return 0

    require(not output_dir.exists(), "refusing to reuse or overwrite --output-dir")
    output_dir.mkdir(parents=True)
    bundle_dir = output_dir / "bundle"
    bundle_dir.mkdir()
    logs_dir = output_dir / "logs"
    logs_dir.mkdir()
    work = make_working_checkpoint(input_dir, output_dir)
    patched_exporter = output_dir / "export_onnx.patched.py"
    patch_sha256 = patch_strict_load(exporter, patched_exporter)
    command = [
        str(python_path), str(patched_exporter),
        "--model-dir", str(work["checkpoint_dir"]),
        "--out-dir", str(bundle_dir),
        "--seq-len", "16", "--verify-len", "512",
    ]
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        "HOME": str(output_dir / "isolated-home"),
        "TMPDIR": str(output_dir / "tmp"),
        "HF_HOME": str(output_dir / "isolated-hf-home"),
        "HF_HUB_OFFLINE": "1",
        "TRANSFORMERS_OFFLINE": "1",
        "USE_TF": "0",
        "USE_TORCH": "1",
        "TOKENIZERS_PARALLELISM": "false",
        "PYTHONNOUSERSITE": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "PYTHONPATH": str(Path(args.source_dir).resolve()),
        "LC_ALL": "C",
        "LANG": "C",
    }
    for name in ("isolated-home", "isolated-hf-home", "tmp"):
        (output_dir / name).mkdir()

    started = datetime.now(timezone.utc).isoformat()
    result = subprocess.run(command, cwd=Path(args.source_dir).resolve(), env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    (logs_dir / "export.stdout.log").write_text(result.stdout, encoding="utf-8")
    (logs_dir / "export.stderr.log").write_text(result.stderr, encoding="utf-8")

    source_after, _ = load_and_verify_manifest(root)
    require(source_after == input_dir, "input root changed during export")
    load_facts = None
    for line in result.stdout.splitlines():
        if line.startswith("checkpoint_load_facts="):
            load_facts = json.loads(line.partition("=")[2])
    verify_lines = [line for line in result.stdout.splitlines() if line.startswith("verify ok: torch vs onnx match within 1e-4")]
    outputs = {}
    bundle_names = (
        "encoder.onnx",
        "encoder.onnx.data",
        "head.onnx",
        "head.onnx.data",
        "tokenizer.json",
        "rl_agent_config.json",
    )
    for name in bundle_names:
        path = bundle_dir / name
        if path.is_file() and not path.is_symlink():
            outputs[name] = {"bytes": path.stat().st_size, "sha256": sha256_file(path)[1]}
    succeeded = (
        result.returncode == 0
        and load_facts == {"missing_keys": [], "unexpected_keys": []}
        and len(verify_lines) == 3
        and set(outputs) == set(bundle_names)
    )
    record = {
        "schema_version": 1,
        "status": "export_and_cpu_parity_verified" if succeeded else "export_failed_or_incomplete",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "started_at": started,
        "model_id": input_manifest["model_id"],
        "model_revision": REVISION,
        "source_commit": commit,
        "exporter_git_blob_sha1": EXPORTER_BLOB,
        "patched_exporter_sha256": patch_sha256,
        "strict_load_policy": "strict=False result accepted only when both missing_keys and unexpected_keys are empty",
        "checkpoint_load_facts": load_facts,
        "command": command,
        "return_code": result.returncode,
        "cpu_parity_verification_lines": verify_lines,
        "onnx_external_data_dependencies": {
            "encoder.onnx": "encoder.onnx.data",
            "head.onnx": "head.onnx.data",
        },
        "input_assets_verified_before_and_after": True,
        "readonly_weight_copy_sha256": work["weight_copy_sha256"],
        "outputs": outputs,
        "stdout_log": "logs/export.stdout.log",
        "stderr_log": "logs/export.stderr.log",
        "unknowns": [
            "Task accuracy and multilingual quality were not evaluated.",
            "CPU latency and memory use were not benchmarked.",
            "The export is not integrated into the product runtime or authorization path.",
        ],
    }
    write_json(output_dir / "export-manifest.json", record)
    print(json.dumps({"status": record["status"], "output_dir": str(output_dir), "outputs": outputs}, indent=2))
    return 0 if succeeded else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", required=True, help="clean checkout at the pinned upstream commit")
    parser.add_argument("--python", required=True, help="isolated Python interpreter with pinned export dependencies")
    parser.add_argument("--output-dir", required=True, help="new directory under target/model-exports")
    parser.add_argument("--check-only", action="store_true", help="verify pins and inputs without running the exporter")
    args = parser.parse_args()
    try:
        return export(args)
    except Exception as error:
        print(f"export-laya-onnx: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
