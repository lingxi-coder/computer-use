#!/usr/bin/env python3
"""Tokenized CPU parity check for the pinned Laya multilingual ONNX package."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from datetime import datetime, timezone


ROOT = Path(__file__).resolve().parent.parent
REVISION = "e4e9ddf21a7b1903b7acffd8814ad4307bf63a67"
SOURCE_COMMIT = "2e4d9c87e8b1621deb344eac7de5c7258f32f849"
PACKAGE_DIR = ROOT / "target/model-exports/laya-multilingual" / REVISION
SOURCE_DIR = ROOT / "target/model-sources/laya-2e4d9c87"
CHECKPOINT_DIR = ROOT / "target/model-resources/laya" / REVISION
BUNDLE_DIR = PACKAGE_DIR / "bundle"
PACKAGE_MANIFEST = PACKAGE_DIR / "package-manifest.json"
OUTPUT = ROOT / "artifacts/m0-laya-tokenized-parity.json"
EXPECTED_PACKAGE = {
    "encoder.onnx": (2771288, "85080d470855319def232f3927d1d8d8794c23a2fae943b3f4564a1e8d3204d0"),
    "encoder.onnx.data": (1227816960, "f1e49a027352064d72831863456be6160748854296bff8332df14130dbf79834"),
    "head.onnx": (244928, "66d3710bedbfa1043411e8de7c60927f2a279105d18e4f1b9db175e06452a02a"),
    "head.onnx.data": (59899904, "0292a4eb478c2c3ec0b6555b4a4093bc802e965654d7aeb4ac22907a17f261d3"),
    "tokenizer.json": (34363188, "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f"),
    "rl_agent_config.json": (472, "25061739243b617ad88d1219ba6f8a9c86c5881ca28df024fa2d9b3b2fcc30c6"),
}
EXPECTED_CHECKPOINT = {
    "model.safetensors": (643835514, "9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204"),
    "tokenizer/tokenizer.json": (34363188, "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f"),
    "rl_agent_config.json": (472, "25061739243b617ad88d1219ba6f8a9c86c5881ca28df024fa2d9b3b2fcc30c6"),
    "encoder/config.json": (1938, "83f6916d13ef0f556ac461f28308dc2bffa7ebeadee8ec9e2db5812020ea5bb4"),
}


def sha256_file(path: Path) -> tuple[int, str]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            size += len(block)
            digest.update(block)
    return size, digest.hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def verify_package() -> dict:
    manifest = json.loads(PACKAGE_MANIFEST.read_text(encoding="utf-8"))
    require(manifest.get("kind") == "laya_onnx_package_manifest", "unexpected package manifest kind")
    require(manifest.get("model_revision") == REVISION, "package model revision mismatch")
    require(manifest.get("source_commit") == SOURCE_COMMIT, "package source commit mismatch")
    require(manifest.get("cpu_parity_verified") is True, "package export parity was not marked successful")
    declared = manifest.get("package_files", {})
    require(set(declared) == set(EXPECTED_PACKAGE), "package manifest does not declare the exact six-file bundle")
    for name, (size, digest) in EXPECTED_PACKAGE.items():
        path = BUNDLE_DIR / name
        require(path.is_file() and not path.is_symlink(), f"package file missing or not regular: {name}")
        actual = sha256_file(path)
        require(actual == (size, digest), f"package hash/size mismatch: {name}")
        record = declared[name]
        require(record.get("bytes") == size and record.get("sha256") == digest, f"package manifest hash/size mismatch: {name}")
    require(manifest.get("external_data_references") == {
        "encoder.onnx": {"external_tensor_count": 134, "locations": ["encoder.onnx.data"]},
        "head.onnx": {"external_tensor_count": 33, "locations": ["head.onnx.data"]},
    }, "package external data references differ from the reviewed inventory")
    return manifest


def verify_checkpoint() -> dict:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=SOURCE_DIR, check=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    require(result.stdout.strip() == SOURCE_COMMIT, "source checkout is not at pinned commit")
    exporter = SOURCE_DIR / "laya-ts/scripts/export_onnx.py"
    exporter_blob = hashlib.sha1(f"blob {exporter.stat().st_size}\0".encode())
    exporter_blob.update(exporter.read_bytes())
    require(exporter_blob.hexdigest() == "d82f7f28c70cd884ea0c48e0dddf4648612bc3cf", "upstream exporter blob changed")
    verified = {}
    for name, (size, digest) in EXPECTED_CHECKPOINT.items():
        result = sha256_file(CHECKPOINT_DIR / name)
        require(result == (size, digest), f"checkpoint asset changed: {name}")
        verified[name] = {"bytes": size, "sha256": digest}
    return verified


def build_examples() -> list[dict]:
    long_boundary_text = (
        "系统状态与处理记录：用户报告结账失败，线上支付已中断，需要检查影响范围和恢复方案。 "
        "Operations note: payment processing is blocked; review incident severity, affected users, "
        "mitigation status, and follow-up ownership. "
    ) * 90
    return [
        {
            "batch": "mixed_short",
            "name": "zh_choice_three_options",
            "state": "生产支付服务完全不可用，订单无法提交，当前有多位用户受影响，需要马上升级处理。",
            "question": {"t": "choice", "ins": "应该采取哪个处理动作？", "crit": {
                "urgent": "立即升级生产故障并恢复服务",
                "routine": "记录普通问题并按队列稍后处理",
                "defer": "暂不处理，等待下个工作日",
            }},
            "expected_action": "urgent",
        },
        {
            "batch": "mixed_short",
            "name": "en_choice_two_options",
            "state": "The release is healthy and the requested copy edit has a workaround. No user is blocked, so handle it in the next routine review.",
            "question": {"t": "choice", "ins": "Which handling priority is supported by the report?", "crit": {
                "urgent": "interrupt the current incident response",
                "defer": "queue for routine follow-up",
            }},
            "expected_action": "defer",
        },
        {
            "batch": "mixed_short",
            "name": "zh_boolean_noul",
            "state": {"tool": "save", "result": "success", "saved": True},
            "question": {"t": "noul", "ins": "工具报告本次保存成功吗？", "crit": {
                "false": "工具结果明确失败",
                "true": "工具结果明确成功",
            }},
            "expected_action": True,
        },
        {
            "batch": "mixed_boundary",
            "name": "zh_boundary_choice_four_options",
            "state": long_boundary_text,
            "question": {"t": "choice", "ins": "从报告中选择合适的操作类别。", "crit": {
                "incident": "需要调查的服务故障",
                "request": "普通用户支持请求",
                "maintenance": "计划内维护工作",
                "unknown": "信息不足，无法判断",
            }},
            "expected_action": "incident",
        },
        {
            "batch": "mixed_boundary",
            "name": "en_boundary_choice_two_options",
            "state": "A teammate asks where a non-blocking icon should move in the next design review.",
            "question": {"t": "choice", "ins": "Select the next step.", "crit": {
                "review": "add it to the next routine design review",
                "incident": "declare a production incident",
            }},
            "expected_action": "review",
        },
        {
            "batch": "single_sample",
            "name": "en_boolean_noul_single",
            "state": "The command completed and returned exit code zero.",
            "question": {"t": "noul", "ins": "Did the command report success?", "crit": {
                "false": "the command failed",
                "true": "the command succeeded",
            }},
            "expected_action": True,
        },
    ]


def max_abs(array) -> float:
    return float(array.max()) if array.size else 0.0


def run() -> dict:
    require(not OUTPUT.exists(), f"refusing to overwrite prior artifact: {OUTPUT}")
    started = datetime.now(timezone.utc).isoformat()
    manifest = verify_package()
    checkpoint_before = verify_checkpoint()
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    os.environ["USE_TF"] = "0"
    os.environ["USE_TORCH"] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"

    sys.path.insert(0, str(SOURCE_DIR))
    import numpy as np
    import onnxruntime as ort
    import torch
    from safetensors.torch import load_file
    from tokenizers import Tokenizer
    from transformers import PreTrainedTokenizerFast
    from laya.common import build_head, build_model, build_sequence, collate_items, render_options, serialize_state

    torch.set_num_threads(min(4, max(1, torch.get_num_threads())))
    config = json.loads((CHECKPOINT_DIR / "rl_agent_config.json").read_text(encoding="utf-8"))
    load_started = time.perf_counter()
    model = build_model(config, encoder_dir=str(CHECKPOINT_DIR / "encoder"), pretrained=False)
    state_dict = load_file(str(CHECKPOINT_DIR / "model.safetensors"), device="cpu")
    incompat = model.load_state_dict(state_dict, strict=True)
    require(not incompat.missing_keys and not incompat.unexpected_keys, "strict checkpoint load reported mismatches")
    model.eval()
    model_load_seconds = time.perf_counter() - load_started

    backend = Tokenizer.from_file(str(BUNDLE_DIR / "tokenizer.json"))
    hf_tok = PreTrainedTokenizerFast(
        tokenizer_object=backend,
        bos_token="<bos>", eos_token="<eos>",
        cls_token="<bos>", sep_token="<eos>",
        mask_token="<mask>", pad_token="<pad>", unk_token="<unk>",
        clean_up_tokenization_spaces=False,
    )
    ids = {
        "cls": hf_tok.cls_token_id,
        "sep": hf_tok.sep_token_id,
        "mask": hf_tok.mask_token_id,
        "pad": hf_tok.pad_token_id,
        "unk": hf_tok.unk_token_id,
    }
    require(ids == {"cls": 2, "sep": 1, "mask": 4, "pad": 0, "unk": 3}, "tokenizer special token ids changed")

    examples = build_examples()
    examples_by_batch: dict[str, list[dict]] = {}
    token_rows = []
    for example in examples:
        sequence, markers = build_sequence(
            hf_tok,
            example["state"],
            example["question"],
            max_len=512,
            head_max_len=192,
        )
        state_text = serialize_state(example["state"]).replace(hf_tok.mask_token, " ")
        reencoded_state_ids = hf_tok(state_text, add_special_tokens=False)["input_ids"]
        head_ids, _, _ = build_head(hf_tok, example["question"], head_max_len=192)
        state_room = max(0, 512 - len(head_ids) - 1)
        state_tokens_used = min(len(reencoded_state_ids), state_room)
        require(sequence and len(sequence) <= 512, f"invalid source sequence length: {example['name']}")
        require(markers and all(sequence[pos] == ids["mask"] for pos in markers), f"marker ids mismatch: {example['name']}")
        item = {
            "ids": sequence,
            "markers": markers,
            "qtype": {"choice": 0, "score": 1, "noul": 2}[example["question"]["t"]],
            "name": example["name"],
        }
        examples_by_batch.setdefault(example["batch"], []).append({"example": example, "item": item})
        token_rows.append({
            "name": example["name"],
            "qtype": example["question"]["t"],
            "qtype_id": item["qtype"],
            "sequence_length": len(sequence),
            "sequence_sha256": hashlib.sha256(json.dumps(sequence, separators=(",", ":")).encode()).hexdigest(),
            "input_ids": sequence,
            "attention_mask": [1] * len(sequence),
            "marker_positions": markers,
            "marker_token_ids": [sequence[pos] for pos in markers],
            "rendered_options": render_options(example["question"]),
            "state_token_count_before_sequence_clamp": len(reencoded_state_ids),
            "state_tokens_used": state_tokens_used,
            "question_prefix_token_count": len(head_ids),
            "truncated_to_max_len": state_tokens_used < len(reencoded_state_ids),
        })

    for batch_name, entries in examples_by_batch.items():
        if batch_name.startswith("mixed_"):
            require(len(entries) >= 2, f"{batch_name} is not mixed")
            require(len({len(entry["item"]["ids"]) for entry in entries}) > 1, f"{batch_name} did not exercise padding")
            require(len({len(entry["item"]["markers"]) for entry in entries}) > 1, f"{batch_name} did not vary marker counts")
    require(any(len(entry["item"]["ids"]) == 512 for entry in examples_by_batch["mixed_boundary"]), "no sequence reached the 512-token boundary")
    require(len(examples_by_batch["single_sample"]) == 1, "single-sample batch shape changed")

    started_sessions = time.perf_counter()
    session_options = ort.SessionOptions()
    session_options.intra_op_num_threads = min(4, max(1, os.cpu_count() or 1))
    session_options.inter_op_num_threads = 1
    encoder_session = ort.InferenceSession(str(BUNDLE_DIR / "encoder.onnx"), session_options, providers=["CPUExecutionProvider"])
    head_session = ort.InferenceSession(str(BUNDLE_DIR / "head.onnx"), session_options, providers=["CPUExecutionProvider"])
    session_setup_seconds = time.perf_counter() - started_sessions
    require(encoder_session.get_providers() == ["CPUExecutionProvider"], "encoder session is not CPU-only")
    require(head_session.get_providers() == ["CPUExecutionProvider"], "head session is not CPU-only")

    batch_results = []
    max_logit_error = 0.0
    max_act_probability_error = 0.0
    expected_actions = []
    timing = {"model_load_seconds": model_load_seconds, "ort_session_setup_seconds": session_setup_seconds, "batches": []}
    for batch_name, entries in examples_by_batch.items():
        items = [entry["item"] for entry in entries]
        batch = collate_items([items], hf_tok.pad_token_id)
        require(batch["input_ids"].shape[0] == len(entries), f"batch row count mismatch: {batch_name}")
        require(batch["marker_pos"].shape == batch["marker_mask"].shape, f"marker tensor shape mismatch: {batch_name}")
        require(np.array_equal(batch["qtype"].numpy(), np.array([item["qtype"] for item in items])), f"qtype tensor mismatch: {batch_name}")
        for row, item in enumerate(items):
            row_len = len(item["ids"])
            require(batch["input_ids"][row, :row_len].tolist() == item["ids"], f"input_ids collate mismatch: {batch_name}/{item['name']}")
            require(batch["attention_mask"][row, :row_len].tolist() == [1] * row_len, f"attention mask mismatch: {batch_name}/{item['name']}")
            require(batch["attention_mask"][row, row_len:].sum().item() == 0, f"padding mask mismatch: {batch_name}/{item['name']}")
            count = len(item["markers"])
            require(batch["marker_pos"][row, :count].tolist() == item["markers"], f"marker positions mismatch: {batch_name}/{item['name']}")
            require(batch["marker_mask"][row, :count].tolist() == [True] * count, f"marker mask mismatch: {batch_name}/{item['name']}")
            require(batch["marker_mask"][row, count:].sum().item() == 0, f"marker padding mismatch: {batch_name}/{item['name']}")

        np_ids = batch["input_ids"].numpy()
        np_mask = batch["attention_mask"].numpy()
        np_positions = batch["marker_pos"].numpy()
        np_marker_mask = batch["marker_mask"].numpy()
        np_qtype = batch["qtype"].numpy()
        torch_started = time.perf_counter()
        with torch.inference_mode():
            ref_logits_t, ref_act_t = model(
                batch["input_ids"], batch["attention_mask"], batch["marker_pos"],
                batch["marker_mask"], batch["qtype"],
            )
        torch_seconds = time.perf_counter() - torch_started
        ort_started = time.perf_counter()
        (hidden,) = encoder_session.run(None, {"input_ids": np_ids, "attention_mask": np_mask})
        head_outputs = head_session.run(
            None,
            {
                "hidden_states": hidden.astype(np.float32, copy=False),
                "marker_pos": np_positions,
                "marker_mask": np_marker_mask,
                "qtype": np_qtype.reshape(-1, 1),
                "attention_mask": np_mask,
            },
        )
        ort_seconds = time.perf_counter() - ort_started
        onnx_logits, onnx_act = head_outputs
        ref_logits = ref_logits_t.detach().cpu().numpy()
        ref_act = ref_act_t.detach().cpu().numpy()
        logits_error = max_abs(np.abs(ref_logits - onnx_logits)[np_marker_mask])
        torch_probs = torch.softmax(ref_act_t, dim=-1).detach().cpu().numpy()
        onnx_exp = np.exp(onnx_act - onnx_act.max(axis=-1, keepdims=True))
        onnx_probs = onnx_exp / onnx_exp.sum(axis=-1, keepdims=True)
        act_error = max_abs(np.abs(torch_probs - onnx_probs))
        max_logit_error = max(max_logit_error, logits_error)
        max_act_probability_error = max(max_act_probability_error, act_error)

        predictions = []
        for index, entry in enumerate(entries):
            example = entry["example"]
            count = len(entry["item"]["markers"])
            probs = torch.softmax(torch.from_numpy(onnx_logits[index, :count]), dim=-1).numpy().tolist()
            if example["question"]["t"] == "noul":
                predicted = probs[1] >= 0.5
                expected = example["expected_action"]
            else:
                options = list(example["question"]["crit"].keys())
                predicted = options[int(np.argmax(probs))]
                expected = example["expected_action"]
            expected_actions.append({
                "name": example["name"],
                "prediction": predicted,
                "expected_action": expected,
                "matches_expectation": predicted == expected,
                "quality_gate": False,
            })
            predictions.append({"name": example["name"], "probabilities": probs, "prediction": predicted, "expected_action": expected})

        passed = logits_error <= 1e-4 and act_error <= 1e-4
        batch_results.append({
            "name": batch_name,
            "rows": len(entries),
            "sequence_lengths": [len(item["ids"]) for item in items],
            "marker_counts": [len(item["markers"]) for item in items],
            "padded_sequence_length": int(batch["input_ids"].shape[1]),
            "input_ids_sha256": hashlib.sha256(np_ids.tobytes()).hexdigest(),
            "attention_mask_sha256": hashlib.sha256(np_mask.tobytes()).hexdigest(),
            "marker_positions": [item["markers"] for item in items],
            "qtype_ids": np_qtype.tolist(),
            "max_abs_active_logit_error": logits_error,
            "max_abs_act_softmax_error": act_error,
            "parity_within_1e_4": passed,
            "predictions_and_expectations": predictions,
        })
        timing["batches"].append({"name": batch_name, "torch_seconds": torch_seconds, "onnx_cpu_seconds": ort_seconds})

    checkpoint_after = verify_checkpoint()
    require(checkpoint_before == checkpoint_after, "checkpoint assets changed during parity check")
    require(max_logit_error <= 1e-4 and max_act_probability_error <= 1e-4, "tokenized CPU parity exceeded 1e-4")
    artifact = {
        "schema_version": 1,
        "kind": "m0_laya_tokenized_cpu_parity",
        "status": "tokenized_cpu_parity_passed",
        "started_at": started,
        "finished_at": datetime.now(timezone.utc).isoformat(),
        "model_id": "convaiinnovations/laya-multilingual",
        "model_revision": REVISION,
        "source_commit": SOURCE_COMMIT,
        "package_manifest_sha256": sha256_file(PACKAGE_MANIFEST)[1],
        "checker_sha256": sha256_file(Path(__file__).resolve())[1],
        "package_files_verified": EXPECTED_PACKAGE,
        "checkpoint_files_verified": checkpoint_before,
        "tokenizer_backend": "upstream tokenizer.json via tokenizers.Tokenizer + PreTrainedTokenizerFast; upstream laya.common.build_sequence/render_options",
        "special_token_ids": ids,
        "checkpoint_load": {"strict": True, "missing_keys": list(incompat.missing_keys), "unexpected_keys": list(incompat.unexpected_keys)},
        "onnx_head_qtype_shape": "[batch, 1] (matching the exported graph); Torch reference uses [batch]",
        "offline": True,
        "providers": {"encoder": encoder_session.get_providers(), "head": head_session.get_providers()},
        "cases": token_rows,
        "batches": batch_results,
        "max_abs_active_logit_error": max_logit_error,
        "max_abs_act_softmax_error": max_act_probability_error,
        "threshold": 1e-4,
        "expected_action_observations_not_quality_gated": expected_actions,
        "timing_seconds": timing,
        "claims": {
            "tokenized_onnx_cpu_parity_verified": True,
            "task_quality_verified": False,
            "expected_action_observations_are_not_quality_evidence": True,
            "latency_benchmark_verified": False,
            "product_runtime_integrated": False,
        },
        "unknowns": [
            "The selected examples do not establish task accuracy, calibration, multilingual quality, or production readiness.",
            "Recorded timings are diagnostic observations, not an M0 latency benchmark or acceptance gate.",
        ],
    }
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    temp = OUTPUT.with_suffix(".json.part")
    with temp.open("x", encoding="utf-8") as stream:
        json.dump(artifact, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.link(temp, OUTPUT)
    temp.unlink()
    return artifact


def main() -> int:
    try:
        artifact = run()
        print(json.dumps({
            "status": artifact["status"],
            "artifact": str(OUTPUT),
            "batches": [{
                "name": batch["name"],
                "rows": batch["rows"],
                "max_abs_active_logit_error": batch["max_abs_active_logit_error"],
                "max_abs_act_softmax_error": batch["max_abs_act_softmax_error"],
            } for batch in artifact["batches"]],
            "timing_seconds": artifact["timing_seconds"],
        }, ensure_ascii=False, indent=2))
        return 0
    except Exception as error:
        print(f"check-laya-onnx-parity: {type(error).__name__}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
