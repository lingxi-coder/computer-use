# Computer User

通用电脑与手机交互系统。一期开发 macOS 上的独立 Electron 桌面产品，使用 Rust 交互内核、MediaPipe 神经感知、S1 快速决策槽位（教师模型起步，本地 Laya 微调晋级后接管）与 Harness Runtime S2 规划。后续 S0 神经控制沿用 MediaPipe。

## 主设计

[完整设计文档 v0.5](docs/design/complete-design.md) 是唯一设计维护入口，包含产品范围、七个模块、技术栈、进程与通信、工程目录、完整执行与恢复契约、评测门禁和 M0–M5 开发路线。游戏、空间与 WoW 的后续扩展在[扩展卷](docs/design/extensions.md)。

- [整体模块与状态所有权](docs/design/complete-design.md#sec02)
- [技术栈、进程和文件结构](docs/design/complete-design.md#sec03)
- [MediaPipe 感知](docs/design/complete-design.md#sec07)、[S1 与 Laya](docs/design/complete-design.md#sec08)、[Harness S2](docs/design/complete-design.md#sec12)
- [验收标准](docs/design/complete-design.md#sec26)、[实施计划](docs/design/complete-design.md#sec28)

当前仓库以设计资料为主，外部 I0 模拟原型尚未迁入。文档中的目标能力和性能指标不表示已实现或已通过真实设备验收。v0.5 依据 R4 评审修订，尚未经过独立复核。

## 评审与历史

- [R4：外部前提、一期主路径与实施顺序](docs/reviews/2026-09-30-architecture-review-r4.md)
- [R3：统一设计复审](docs/reviews/2026-09-30-architecture-review-r3.md)
- [R2：MediaPipe、接纳与恢复契约](docs/reviews/2026-09-30-architecture-review-r2.md)
- [R1：功能模块与快速反应闭环](docs/reviews/2026-09-30-architecture-review-r1.md)
- 历史稿：[通用设计 v0.3](Generic_Interaction_System_Complete_Design_v0.3.md)、[桌面模块化补充](docs/architecture/desktop-modular-design.md)
