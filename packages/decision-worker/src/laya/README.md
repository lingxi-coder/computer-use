# Laya TypeScript preprocessing

`vendor/laya-ts/src/tokenizer.ts` and `common.ts` are copied unmodified from
`NandhaKishorM/laya`, `laya-ts`, commit
`2e4d9c87e8b1621deb344eac7de5c7258f32f849` (Apache-2.0; license is included
beside the copied files). Their Git blob IDs are respectively
`3e460d0d4ba42b069b432a86e714b9b688e3cc59` and
`38f170ed69a0614315bf57a30e22e2dd6090aea7`.

The tokenizer asset and exported CPU ONNX bundle are pinned to
`convaiinnovations/laya-multilingual` revision
`e4e9ddf21a7b1903b7acffd8814ad4307bf63a67`. The backend accepts only that
export manifest and exact graph, external-data, config, and tokenizer hashes.
Preprocessing calls the pinned Laya `serializeState` and `buildSequence` code.
The checked-in `runtime/` JavaScript and declaration files are generated only
from those pinned TypeScript sources using TypeScript 5.9.3 with the compiler
options recorded in `build-pins.json`. The production backend imports this
runtime boundary; tests verify the source and generated-file hashes.
`node scripts/check-laya-preprocess-build.mjs` reproduces all four outputs in a
temporary directory and checks their hashes without changing the vendor files.

This module contains no model weights and does not download them. The product
packager must place the pinned `package-manifest.json` and all `package_files`
in the installed model resource; the manifest may be next to the files or next
to a fixed `bundle/` directory. `onnxruntime-node` is pinned to 1.30.0 in this
package and the workspace lockfile. Local Python/Node CPU parity and a real
Electron utility-process lifecycle check have passed; these are infrastructure
checks, not task-quality or installed-product acceptance. Failure is reported to the caller;
there is no Python or scripted-teacher fallback.

Laya's raw answer confidence must be at least 800/1000 and its action-head
probability must be at least 500/1000. Inputs that truncate, have collapsed
option encodings, or produce malformed/non-finite tensors are refused. These
thresholds are conservative plumbing defaults, not task-quality calibration.
