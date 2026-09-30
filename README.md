# Computer User

通用电脑与手机交互系统。一期开发 macOS 上的独立 Electron 桌面产品，使用 Rust 交互内核、MediaPipe 神经感知、S1 快速决策槽位（教师模型起步，本地 Laya 微调晋级后接管）与 Harness Runtime S2 规划。后续 S0 神经控制沿用 MediaPipe。

## 当前状态

**M1 架构骨架已实现，全部基于模拟后端。** 七个模块在进程内、也在三个真实进程（`cu-host`、`cu-device`、`cu-source`）之间跑通同一条闭环，包括停止、暂停、重启恢复与设备死亡；I0 原型已迁入，并由生产用的执行状态机通过原 36 项回归。Electron 壳通过了类型检查、生产构建与 Node 级测试，**从未在真实窗口中启动过**。

**没有做**：真实 macOS 输入与采集（M2）、Laya 与教师模型、云端 S2、MediaPipe、签名与 TCC、真机时延与性能。S1 离线决策质量实验尚未开始，它是整个架构最大的未知数（设计 §8.6）。完整边界见[设计 §28.1](docs/design/complete-design.md#sec28)与 [ADR-0004](docs/architecture/adr-0004-simulated-world-and-its-limits.md)。文档中的目标能力和性能指标不表示已实现或已通过真实设备验收。

## 主设计

[完整设计文档 v0.6](docs/design/complete-design.md) 是唯一设计维护入口，包含产品范围、七个模块、技术栈、进程与通信、工程目录、完整执行与恢复契约、评测门禁和 M0–M5 开发路线。游戏、空间与 WoW 的后续扩展在[扩展卷](docs/design/extensions.md)。v0.6 在 v0.5 之上加入 M1 实现的反馈（§3.3、§4.6、§13.5、§27.4、§28.1），尚未经过独立复核。

- [整体模块与状态所有权](docs/design/complete-design.md#sec02)
- [技术栈、进程和文件结构](docs/design/complete-design.md#sec03)
- [MediaPipe 感知](docs/design/complete-design.md#sec07)、[S1 与 Laya](docs/design/complete-design.md#sec08)、[Harness S2](docs/design/complete-design.md#sec12)
- [验收标准](docs/design/complete-design.md#sec26)、[实施计划](docs/design/complete-design.md#sec28)

## 开发

需要 `rust-toolchain.toml` 固定的 Rust（1.94.0）和 Node 22 或更新版本。

```sh
ELECTRON_SKIP_BINARY_DOWNLOAD=1 npm ci   # 一次即可；不需要 Electron 二进制，所有检查都不会打开窗口
./scripts/check-all.sh                   # 全部检查，由便宜到昂贵
```

`check-all.sh` 依次运行：`cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace`（含真实进程测试）、生成类型与已发布 schema 的一致性、`tsc` 类型检查、`npm test`（会启动真实的原生进程）、`npm run build`。任一步失败即停止。

单项命令：

```sh
cargo test --workspace --locked                        # Rust：单元、契约、场景、真实进程
cargo run --locked -p execution-core --bin i0-demo     # I0 演示；输出与 contracts/i0-v1/i0-demo.golden.json 逐字节一致
npm run gen-types                                      # 由 contracts/desktop-v1/schema.json 重新生成 TypeScript 类型
npm run typecheck && npm test && npm run build
```

磁盘紧张时，给 cargo 加上 `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0` 可以明显缩小 `target/`。

## 结构

```text
crates/
  interaction-contracts   I0（冻结）与 desktop v1 契约、帧、握手、schema 导出
  execution-core          执行状态机、停止锁存、输入账本与模拟后端；I0 的 Simulator 外观
  interaction-engine      七个模块与 Engine 编排：同步状态机，不含 IO，时间由调用方传入
  harness-adapter         S2 与教师模型的单次结构化调用回合逻辑，与集成路线无关
  sim-world               模拟的设置应用与 device、source 共享的世界日志
  desktop-ipc             帧读写、私有 socket、握手、时钟
  device-macos            cu-device 进程外壳（目前只有模拟后端）
  source-macos            cu-source 进程外壳（目前只有夹具来源）
  desktop-host            cu-host：组装 Engine 与 SQLite，连接 device 与 source
  desktop-supervisor      测试用 supervisor：扮演 Electron main，启动并监管三个进程
packages/
  desktop-api             由 schema 生成的类型、Ajv 校验、帧编解码
  decision-worker         决策 worker 协议与脚本化教师
apps/desktop              Electron main、preload、renderer
contracts/i0-v1           冻结的 I0 schema、fixtures、demo golden 与来源哈希
contracts/desktop-v1      desktop v1 的 schema 与 golden fixtures，Rust 与 TypeScript 共用
docs/architecture         ADR 与历史设计稿
```

架构决定记录：

- [ADR-0001：I0 冻结与执行核心](docs/architecture/adr-0001-i0-freeze-and-execution-core.md)
- [ADR-0002：同步状态机与进程外壳](docs/architecture/adr-0002-synchronous-state-machines.md)
- [ADR-0003：监督通道与本地 IPC](docs/architecture/adr-0003-supervision-and-local-ipc.md)
- [ADR-0004：模拟世界及其局限](docs/architecture/adr-0004-simulated-world-and-its-limits.md)

## 评审与历史

- [R4：外部前提、一期主路径与实施顺序](docs/reviews/2026-09-30-architecture-review-r4.md)
- [R3：统一设计复审](docs/reviews/2026-09-30-architecture-review-r3.md)
- [R2：MediaPipe、接纳与恢复契约](docs/reviews/2026-09-30-architecture-review-r2.md)
- [R1：功能模块与快速反应闭环](docs/reviews/2026-09-30-architecture-review-r1.md)
- 历史稿：[通用设计 v0.3](Generic_Interaction_System_Complete_Design_v0.3.md)、[桌面模块化补充](docs/architecture/desktop-modular-design.md)
