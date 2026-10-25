# LingXi Control · 灵犀操控

通用电脑与手机交互系统。一期开发 macOS 上的独立 Electron 桌面产品，使用 Rust 交互内核、MediaPipe 神经感知、S1 快速决策槽位（教师模型起步，本地 Laya 微调晋级后接管）与 Harness Runtime S2 规划。后续 S0 神经控制沿用 MediaPipe。

## 当前状态

**M1 八项审查问题已修复；M2 窗口授权/激活、原生输入和执行指示已接入；M3 自有夹具已从真实 Operator 前台完成登记 GUI 任务。** 目标确认/Execute、窗口激活、一次文本写入、两次导航和新 UI 证据经 Host/Engine/Reaction/Attempt，使根任务完成；决策/outbox/预算与窗口/根 EffectResult 已核对。该 GUI 记录仍使用显式登记参考后端。[S2 模型层](docs/architecture/m4-model-runtime.md)已固定 SHA 接线并通过 SDK/host 生命周期检查，[产品模型设置、原生出站同意与 Keychain](docs/architecture/m4-model-authorization.md) 已用唯一假密钥在真实 Electron 验证；真实云端任务尚未验收。I0 原 36 项与 golden 保留，完整 M2/M3/MVP 继续进行。

M2 已实现 Swift 6 helper、被动权限预检、AXObserver 与实例失效处理、ScreenCaptureKit SCStream 和 Vision OCR，经有界 worker 接入 `cu-source`，帧/健康元数据进入 StateCore 与桌面投影。idle 保留原像素时间，不能伪造动作后的新证据；PNG 通过独立有界通道进入只读预览，OCR 暂以私有 manifest 输出。默认原生观察输入保持 unavailable，受信固定目标配置可启动 AXPress 后端。[原生观察入口与限制](docs/architecture/m2-native-observation.md)说明运行方式及完整 M2 尚需完成的工作。

**尚未验收**：其它一期输入原语和完整停止/故障矩阵、复杂坐标/权限/睡眠环境矩阵、Laya 与教师模型质量、云端 S2、MediaPipe、发行签名与公证、真机时延与性能。S1 离线诊断已运行，质量门尚未通过；Apple Development 签名的本地应用包已通过包内资源启动、Laya/ONNX 初始化与 Stop 清理，完整 MVP 仍未完成。文档中的目标能力和性能指标不表示已通过真实设备或完整 M2 验收。

M2 的[被动设备守卫](docs/architecture/m2-device-guards.md)已接入人工输入监听、点位命中、双租约与故障回收。[原生只读权限探测](docs/architecture/m2-native-authority.md)、[AX 单次提交](docs/architecture/m2-native-atomic-submit.md)、[生产异步后端](docs/architecture/m2-native-device-pipeline.md)和[登记文本/返回读回](docs/architecture/m3-native-value-workflow.md)均有独立证据，未知输入不重放。最新[Host/Engine 与 Operator 授权](docs/architecture/m3-native-host-engine.md)接入真实决策消费和窗口同意/撤销；默认 observe，同意后仍须确认目标和选择 Execute。产品历史持久化、真实模型调用及完整剩余工作见[首个 MVP 跟踪](docs/mvp-status.md)。

## 主设计

[完整设计文档 v0.22](docs/design/complete-design.md) 是唯一设计维护入口，包含产品范围、七个模块、技术栈、进程与通信、工程目录、完整执行与恢复契约、评测门禁和 M0–M5 开发路线。游戏、空间与 WoW 的后续扩展在[扩展卷](docs/design/extensions.md)。§28.2–28.17 记录审查修复、持续来源、预览、设备守卫、原生输入、Host/Engine 接纳、Operator 授权及[窗口激活与 GUI 执行](docs/architecture/m3-native-operator-execution.md)、[S2 模型层接线](docs/architecture/m4-model-runtime.md)；实施状态与完整阶段验收分别记录。

- [整体模块与状态所有权](docs/design/complete-design.md#sec02)
- [技术栈、进程和文件结构](docs/design/complete-design.md#sec03)
- [MediaPipe 感知](docs/design/complete-design.md#sec07)、[S1 与 Laya](docs/design/complete-design.md#sec08)、[Harness S2](docs/design/complete-design.md#sec12)
- [验收标准](docs/design/complete-design.md#sec26)、[实施计划](docs/design/complete-design.md#sec28)
- [后台 AX 与无窗口控制](docs/architecture/background-and-headless-control.md)：目标上下文与全局焦点解耦；[固定 headless checkbox Root](docs/architecture/headless-root-integration.md)已通过真实页内输入、新读验证与普通 Electron 入口。通用原生控件、任意网站与无 WindowServer 仍需验收。

## 开发

需要 `rust-toolchain.toml` 固定的 Rust（1.94.0）和 Node 22 或更新版本。

```sh
ELECTRON_SKIP_BINARY_DOWNLOAD=1 npm ci   # 一次即可；不需要 Electron 二进制，所有检查都不会打开窗口
./scripts/check-all.sh                   # 全部检查，由便宜到昂贵
```

`check-all.sh` 依次运行：`cargo fmt --check`、`clippy -D warnings`、`cargo test --workspace`（含真实进程测试）、生成类型一致性、`tsc`、`npm test`、生产构建；macOS 还构建 Swift 6 helper 并运行生成图片的真实 Vision 测试。任一步失败即停止。macOS 原生检查需要 Xcode SDK；受限沙箱可能拒绝 Unix socket 或 Vision 的系统服务，须在允许这些本地能力的环境运行。

日常开发按[开发推进流程](docs/development-workflow.md)选择受影响的检查，整合批次后再运行所需的完整回归。

单项命令：

```sh
cargo test --workspace --locked                        # Rust：单元、契约、场景、真实进程
cargo run --locked -p execution-core --bin i0-demo     # I0 演示；输出与 contracts/i0-v1/i0-demo.golden.json 逐字节一致
npm run gen-types                                      # 由 contracts/desktop-v2/schema.json 重新生成 TypeScript 类型
npm run typecheck && npm test && npm run build
```

磁盘紧张时，给 cargo 加上 `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0` 可以明显缩小 `target/`。

macOS arm64 的本地应用组装使用已安装的 Electron 43.1.1，不下载运行时。先准备完整 Electron 构建、`target/debug` 的七个原生程序和固定版本的 Laya 导出资源，再运行：

```sh
npm run build --workspace @computer-use/desktop
npm run package:mac:local --workspace @computer-use/desktop -- \
  --output /absolute/path/below/computer-use/new-package \
  --native-bin-dir /absolute/path/to/computer-use/target/debug
```

输出路径必须位于项目内且尚不存在。脚本校验原生和 ONNX 架构、模型哈希及资源清单，生成 `LingXi Control.app` 和完整文件哈希清单；应用最低 macOS 版本为 14.2，固定浏览器任务还需要标准位置安装的 Google Chrome。安装版仅使用自身 `Resources/bin`，资源缺失即失败。本命令生成本地开发包，使用统一的灵犀操控品牌图标；签名发行、模型质量与业务门禁的状态见[首个 MVP 跟踪](docs/mvp-status.md)。

已冻结的本地包可以复制到新的输出目录并使用本机 Apple Development 身份签名：

```sh
node scripts/sign-macos-local-app.mjs \
  --output /absolute/path/below/computer-use/artifacts/current-run/new-signing-output \
  --identity-sha1 YOUR_VALID_DEVELOPMENT_CERTIFICATE_SHA1
```

`current-run` 须为已经存在的独立 artifact 目录，输出子目录须尚不存在。脚本校验冻结包和原生架构，再签名嵌套代码、更新复制包的原生资源哈希并严格核验所有签名对象。原始冻结包保持不变；这是本地开发签名，发行仍需 Developer ID、公证与安装验收。

## 结构

```text
crates/
  interaction-contracts   I0（冻结）与 desktop v2 契约、帧、握手、schema 导出
  execution-core          执行状态机、停止锁存、输入账本与模拟后端；I0 的 Simulator 外观
  interaction-engine      七个模块与 Engine 编排：同步状态机，不含 IO，时间由调用方传入
  harness-adapter         S2 与教师模型的单次结构化调用回合逻辑，与集成路线无关
  sim-world               模拟的设置应用与 device、source 共享的世界日志
  desktop-ipc             帧读写、私有 socket、握手、时钟
  device-macos            cu-device：模拟输入、macOS 只读与受信固定目标 AX 后端
  source-macos            cu-source：夹具与原生 AX/SCStream/Vision 来源；私有 manifest
  desktop-host            cu-host：组装 Engine 与 SQLite，连接 device 与 source
  desktop-supervisor      测试用 supervisor：扮演 Electron main，启动并监管三个进程
packages/
  desktop-api             由 schema 生成的类型、Ajv 校验、帧编解码
  decision-worker         决策 worker 协议与脚本化教师
apps/desktop              Electron main、preload、renderer
contracts/i0-v1           冻结的 I0 schema、fixtures、demo golden 与来源哈希
contracts/desktop-v2      desktop v2 的 schema 与 golden fixtures，Rust 与 TypeScript 共用
docs/architecture         ADR 与历史设计稿
```

架构决定记录：

- [ADR-0001：I0 冻结与执行核心](docs/architecture/adr-0001-i0-freeze-and-execution-core.md)
- [ADR-0002：同步状态机与进程外壳](docs/architecture/adr-0002-synchronous-state-machines.md)
- [ADR-0003：监督通道与本地 IPC](docs/architecture/adr-0003-supervision-and-local-ipc.md)
- [ADR-0004：模拟世界及其局限](docs/architecture/adr-0004-simulated-world-and-its-limits.md)

## 评审与历史

- [M1 八项修复与 M2 原生观察验证](docs/reviews/2026-09-30-m1-fixes-and-m2-entry.md)
- [R4：外部前提、一期主路径与实施顺序](docs/reviews/2026-09-30-architecture-review-r4.md)
- [R3：统一设计复审](docs/reviews/2026-09-30-architecture-review-r3.md)
- [R2：MediaPipe、接纳与恢复契约](docs/reviews/2026-09-30-architecture-review-r2.md)
- [R1：功能模块与快速反应闭环](docs/reviews/2026-09-30-architecture-review-r1.md)
- 历史稿：[通用设计 v0.3](Generic_Interaction_System_Complete_Design_v0.3.md)、[桌面模块化补充](docs/architecture/desktop-modular-design.md)
