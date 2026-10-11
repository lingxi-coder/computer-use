"""Pure parser tests for the Route B model-source identity gate."""

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from model_source import (  # noqa: E402
    HARNESS_URL,
    REQUIRED_HARNESS_PACKAGES,
    SDK_URL,
    SourceIdentityError,
    inspect_metadata,
    main,
)


HARNESS_REVISION = "a" * 40
SDK_REVISION = "b" * 40
HARNESS_SOURCE = f"git+{HARNESS_URL}?rev={HARNESS_REVISION}#{HARNESS_REVISION}"
SDK_SOURCE = f"git+{SDK_URL}?rev={SDK_REVISION}#{SDK_REVISION}"
HARNESS_ROOT = "/cargo/git/checkouts/harness-runtime-hash/revision"
SDK_ROOT = "/cargo/git/checkouts/llm-client-hash/revision"
PRODUCT_ROOT = "/workspace/computer-use"


def _manifest_text(manifest):
    dependencies = manifest["workspace"]["dependencies"]
    rows = ["[workspace.dependencies]"]
    for name, dependency in dependencies.items():
        rows.append(f'{name} = {{ git = "{dependency["git"]}", rev = "{dependency["rev"]}" }}')
    return "\n".join(rows) + "\n"


def _package(name, source, manifest_path, dependencies=()):
    source_part = source or f"path+file://{manifest_path.rsplit('/', 1)[0]}"
    package_id = f"{source_part}#{name}@0.1.0"
    return {
        "id": package_id,
        "name": name,
        "version": "0.1.0",
        "source": source,
        "manifest_path": manifest_path,
        "dependencies": list(dependencies),
    }


def _node(package, *dependencies):
    return {"id": package["id"], "deps": [{"pkg": item["id"]} for item in dependencies]}


def fixture():
    harness = {}
    for name in sorted(REQUIRED_HARNESS_PACKAGES | {"llm-runtime", "branding", "jsonrpc", "telemetry-macros"}):
        harness[name] = _package(
            name,
            HARNESS_SOURCE,
            f"{HARNESS_ROOT}/crates/{name}/Cargo.toml",
        )
    sdk = _package("lingxi-llm-client", SDK_SOURCE, f"{SDK_ROOT}/Cargo.toml")
    product = _package("harness-adapter", None, f"{PRODUCT_ROOT}/crates/harness-adapter/Cargo.toml")

    harness["llm-runtime"]["dependencies"] = [
        {"name": "core", "source": HARNESS_SOURCE},
        {"name": "secret", "source": HARNESS_SOURCE},
        {"name": "telemetry", "source": HARNESS_SOURCE},
        {"name": "lingxi-llm-client", "source": SDK_SOURCE},
    ]

    packages = [*harness.values(), sdk, product]
    nodes = [
        _node(product, harness["llm-runtime"], sdk),
        _node(
            harness["llm-runtime"],
            harness["core"],
            harness["secret"],
            harness["telemetry"],
            sdk,
        ),
        _node(harness["core"], harness["branding"]),
        _node(harness["secret"], harness["jsonrpc"]),
        _node(harness["telemetry"], harness["telemetry-macros"]),
        *[_node(harness[name]) for name in ("branding", "jsonrpc", "telemetry-macros")],
        _node(sdk),
    ]
    manifest = {
        "workspace": {
            "dependencies": {
                "llm-runtime": {"git": HARNESS_URL, "rev": HARNESS_REVISION},
                "lingxi-llm-client": {"git": SDK_URL, "rev": SDK_REVISION},
            }
        }
    }
    metadata = {
        "packages": packages,
        "workspace_members": [product["id"]],
        "resolve": {"root": None, "nodes": nodes},
    }
    return manifest, metadata


class ModelSourceTests(unittest.TestCase):
    def test_accepts_locked_reachable_harness_closure_and_exact_sdk(self):
        manifest, metadata = fixture()

        result = inspect_metadata(manifest, metadata)

        self.assertEqual(result["harness_revision"], HARNESS_REVISION)
        self.assertEqual(result["sdk_revision"], SDK_REVISION)
        self.assertGreaterEqual(len(result["harness_package_names"]), 4)

    def test_rejects_short_or_noncanonical_manifest_pins(self):
        for key, value in (
            ("llm-runtime", {"git": HARNESS_URL, "rev": "a" * 39}),
            ("lingxi-llm-client", {"git": SDK_URL + ".git", "rev": SDK_REVISION}),
            ("llm-runtime", {"path": "../harness-runtime", "rev": HARNESS_REVISION}),
        ):
            with self.subTest(key=key, value=value):
                manifest, metadata = fixture()
                manifest["workspace"]["dependencies"][key] = value
                with self.assertRaises(SourceIdentityError):
                    inspect_metadata(manifest, metadata)

    def test_rejects_mixed_harness_and_sdk_revisions(self):
        manifest, metadata = fixture()
        changed = "git+" + HARNESS_URL + "?rev=" + "c" * 40 + "#" + "c" * 40
        next(package for package in metadata["packages"] if package["name"] == "core")["source"] = changed
        with self.assertRaisesRegex(SourceIdentityError, "mixed source pin"):
            inspect_metadata(manifest, metadata)

        manifest, metadata = fixture()
        changed_sdk = "git+" + SDK_URL + "?rev=" + "d" * 40 + "#" + "d" * 40
        next(package for package in metadata["packages"] if package["name"] == "lingxi-llm-client")["source"] = changed_sdk
        with self.assertRaises(SourceIdentityError):
            inspect_metadata(manifest, metadata)

        manifest, metadata = fixture()
        manifest["workspace"]["dependencies"]["another-harness-crate"] = {
            "git": "ssh://git@github.com/lingxi-coder/harness-runtime.git",
            "rev": "c" * 40,
        }
        with self.assertRaisesRegex(SourceIdentityError, "mixes Harness"):
            inspect_metadata(manifest, metadata)

    def test_rejects_duplicate_identity_and_reachable_path_overlay(self):
        manifest, metadata = fixture()
        duplicate = _package("core", None, f"{PRODUCT_ROOT}/crates/core/Cargo.toml")
        metadata["packages"].append(duplicate)
        with self.assertRaisesRegex(SourceIdentityError, "duplicate Cargo package identity"):
            inspect_metadata(manifest, metadata)

        manifest, metadata = fixture()
        runtime = next(package for package in metadata["packages"] if package["name"] == "llm-runtime")
        overlay = _package("core-path-overlay", None, f"{PRODUCT_ROOT}/crates/core-path-overlay/Cargo.toml")
        metadata["packages"].append(overlay)
        core_edge = next(node for node in metadata["resolve"]["nodes"] if node["id"] == runtime["id"])
        core_edge["deps"].append({"pkg": overlay["id"]})
        metadata["resolve"]["nodes"].append(_node(overlay))
        with self.assertRaisesRegex(SourceIdentityError, "path overlay"):
            inspect_metadata(manifest, metadata)

    def test_rejects_fake_top_level_records_without_required_resolved_closure(self):
        manifest, metadata = fixture()
        metadata["packages"] = [
            package for package in metadata["packages"]
            if package["name"] in {"llm-runtime", "lingxi-llm-client", "harness-adapter"}
        ]
        metadata["resolve"]["nodes"] = [
            node for node in metadata["resolve"]["nodes"]
            if node["id"] in {package["id"] for package in metadata["packages"]}
        ]
        with self.assertRaises(SourceIdentityError):
            inspect_metadata(manifest, metadata)

    def test_requires_runtime_and_harness_closure_to_be_reachable(self):
        manifest, metadata = fixture()
        product = next(package for package in metadata["packages"] if package["name"] == "harness-adapter")
        product_node = next(node for node in metadata["resolve"]["nodes"] if node["id"] == product["id"])
        runtime_id = next(package["id"] for package in metadata["packages"] if package["name"] == "llm-runtime")
        product_node["deps"] = [edge for edge in product_node["deps"] if edge["pkg"] != runtime_id]
        with self.assertRaisesRegex(SourceIdentityError, "unreachable from product workspace"):
            inspect_metadata(manifest, metadata)

        manifest, metadata = fixture()
        runtime = next(package for package in metadata["packages"] if package["name"] == "llm-runtime")
        runtime_node = next(node for node in metadata["resolve"]["nodes"] if node["id"] == runtime["id"])
        runtime_node["deps"] = [edge for edge in runtime_node["deps"] if edge["pkg"] != next(
            package["id"] for package in metadata["packages"] if package["name"] == "lingxi-llm-client"
        )]
        with self.assertRaisesRegex(SourceIdentityError, "not reachable from the resolved llm-runtime"):
            inspect_metadata(manifest, metadata)

    def test_metadata_argument_does_not_invoke_cargo(self):
        manifest, metadata = fixture()
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            (root / "Cargo.toml").write_text(_manifest_text(manifest), encoding="utf-8")
            metadata_path = root / "metadata.json"
            metadata_path.write_text(json.dumps(metadata), encoding="utf-8")
            with mock.patch("model_source.ROOT", root), mock.patch(
                "model_source.subprocess.run", side_effect=AssertionError("Cargo must not run")
            ), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(["--metadata", str(metadata_path)]), 0)

    def test_default_uses_locked_offline_cargo_metadata(self):
        manifest, metadata = fixture()
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            (root / "Cargo.toml").write_text(_manifest_text(manifest), encoding="utf-8")
            completed = mock.Mock(stdout=json.dumps(metadata))
            with mock.patch("model_source.ROOT", root), mock.patch(
                "model_source.subprocess.run", return_value=completed
            ) as run, contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main([]), 0)

        command = run.call_args.args[0]
        self.assertEqual(command[:2], ["cargo", "metadata"])
        self.assertIn("--locked", command)
        self.assertIn("--offline", command)
        self.assertIn("--all-features", command)
        self.assertEqual(command[-1], str(root / "Cargo.toml"))
        self.assertTrue(run.call_args.kwargs["check"])


if __name__ == "__main__":
    unittest.main()
