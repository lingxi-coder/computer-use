# I0 v1 来源记录

本目录和 `crates/interaction-contracts/src/i0.rs`、`crates/execution-core` 的 I0 部分来自外部历史原型 `interaction-core` 0.1.0。迁入时的来源是上级工作区目录 `lingxi/.local-checkouts/computer-use`，该快照没有 Git 元数据和独立 LICENSE，因此只能用内容哈希标识。

迁入日期：2026-09-30。迁入前在快照上运行 `cargo test --offline --locked --all-targets`，36 项测试通过。

## 来源文件与 SHA-256

| 来源文件 | SHA-256 | 迁入位置 | 改动 |
|---|---|---|---|
| `src/protocol.rs` | `461408da69f493b9685c98fd2c61f0e775e801b9f665234653275b81315c52d1` | `crates/interaction-contracts/src/i0.rs` | 无 |
| `src/simulator.rs` | `f0cd9023c270d2bd2e5480c50ce10173e583a1f96734c8d0b3346859f3289890` | `crates/execution-core/src/sim.rs` | 仅 import |
| `src/bin/i0_demo.rs` | `a626a26b9f9fda3ad5845ac5bafad0689a4935b1192f8d5f4e5cacbce70e6d25` | `crates/execution-core/src/bin/i0_demo.rs` | 仅 import |
| `src/bin/export_schema.rs` | `aba77f43ea44a81ee4e879c11d120f7bd67d85078fdd473f3c9fe6d4196f67ee` | `crates/interaction-contracts/src/bin/export_schema.rs` | 仅 import |
| `tests/simulator_contract.rs` | `ee51f29c6d427f29afe5ea9354f55d9ce8eeee47a2c68059cf9b95dc52f591aa` | `crates/execution-core/tests/simulator_contract.rs` | 仅 import |
| `tests/executor_regressions.rs` | `c00d8103f0799542dd1f7fbc77ee538fd4faedc437f2d5703fbcec9bc266411b` | `crates/execution-core/tests/executor_regressions.rs` | 仅 import |
| `tests/wire_contract.rs` | `829d831dd69e746046f81ced8ac0ff01eedebbdee52162fc8ba4f07598e99fb6` | `crates/interaction-contracts/tests/wire_contract.rs` | import 与 `include_str!` 路径 |
| `contracts/interaction.schema.json` | `ed9d51fbfa131fe9979f671b52970eba433126dd5843d2eca3880fd7f2f9bfb2` | `contracts/i0-v1/interaction.schema.json` | 无 |
| `contracts/fixtures.json` | `3615a34ee8cbc1970ae37846043b939d7f81ff78ef6954594ee73d81de61bbfd` | `contracts/i0-v1/fixtures.json` | 无 |
| `artifacts/i0-demo.json` | `921cd376c852570aad57808236ca366d2fd6602798e7f18ab29f0f6db2633367` | `contracts/i0-v1/i0-demo.golden.json` | 无 |

上表的哈希是迁入那一刻的来源内容。`sim.rs` 此后按主设计 §27.4 的第三步重构为 execution-core，不再与来源逐字节一致；`i0.rs`、schema、fixtures 和 demo golden 保持冻结。

## 冻结规则

- `i0.rs` 的序列化形式必须继续与 `interaction.schema.json` 和 `fixtures.json` 一致，`protocol_version` 保持为 1。
- `i0-demo` 的输出必须与 `i0-demo.golden.json` 逐字节一致。
- 新消息放进独立的协议命名空间，不修改这里的文件。
