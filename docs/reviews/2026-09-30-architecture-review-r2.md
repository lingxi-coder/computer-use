# 架构评审 R2：MediaPipe 与模块化架构的独立复审

- 日期：2026-09-30
- 对象：[一期桌面模块化架构](../architecture/desktop-modular-design.md)，对照[通用设计 v0.3](../../Generic_Interaction_System_Complete_Design_v0.3.md) 与当前 Harness 源码。
- 方法：独立架构评审代理审查完整新稿；MediaPipe 官方文档/源码和本地 Harness 能力由另两条只读调查交叉核验。作者负责修订，评审代理负责复核。
- 初次结论：REVISE；4 项 P1、1 项 P2。
- 最终结论：APPROVED（架构设计）。独立评审代理复核修订稿，5 项设计问题均已关闭，未发现新的阻塞矛盾。
- 修订状态：以下修订已写入架构稿并通过关闭复核；不表示实现、性能或真实设备已验收。
- 验证范围：设计与源码接口核验；没有构建 MediaPipe、运行神经模型、调用真实云端模型或进行真机停止/签名安装包验收。

## 保留的设计

七模块及单写 owner、MediaPipe 候选观测到 StateCore 的接纳、原始采集时间与变换链、有界队列、CPU 基线与 GPU 路径区分、ImageEmbedder 不冒充 GUI 语义、Memory 的人工 revision/release 和候选经验均可保留。

MediaPipe 用于 Environment 神经感知及未来 S0；Laya S1 和 Harness S2 独立，这是用户已确认的边界。

## R2-01 [P1] 跨 TaskRuntime / Reaction 的预算与取消接纳

初稿位置：架构稿 §2 原第 43 行。Reaction 对不可变 TaskControlContext 的检查，不能保证 TaskRuntime 尚未取消，或预算没有被其他 attempt 消耗；“原子检查上下文”没有跨 owner 协议。

修订：TaskRuntime 串行、幂等处理 ReserveAttempt，绑定 root cancellation epoch、任务/计划版本、子目标和申请额度；Reaction 持久写唯一决策映射；TaskRuntime 的 ActivateAttempt 再复验后才允许正常输入意图。Execution 对 root/permit epoch 独立复验。未激活预留可幂等回收，已激活/未知额度保守保留，恢复不重置预算/期限。

验收：在预留、映射持久化、激活和输入提交之间分别取消/崩溃，检查无重复扣账、无新 ID 重放、旧 epoch 不准入和未知额度不被重新获得。

## R2-02 [P1] 一个 root 对应真实独立的 Harness runtime

初稿位置：架构稿 §7 原第 134 行。Harness::session() 只是当前挂载会话的访问句柄；共享 DesktopRuntime 会共享 orchestrator、transcript、累计费用和输出 slot。

修订：每个 root 创建独立 DesktopRuntime + Harness + slot + OutputStream recorder，固定 session identity、独占 writer lease 和持久目录，仅共享 credential stack。一期一个 active root，其余排队。结束时停止准入、取消并 await 在途 S2，再 shutdown；不完整 shutdown 保持清理状态并串行重试，完成后释放资源。

源码依据：`harness-runtime/crates/runtime/src/api.rs` 的 session()；`desktop/sdk.rs` 的 SessionService/LifecycleService 委托；`desktop/assembly.rs` 的 build_with_credential_stack。

验收：两个 root 先后装配不能共享 transcript/cost/slot；覆盖在途取消、writer lease、shutdown.complete=false、启动/驻留成本和资源回收。

## R2-03 [P1] Schema 校验后的 slot 不保证恰好一次提交

初稿位置：架构稿 §7 原第 136 行。StructuredOutputTool 是 concurrency-safe；每个通过 Schema 校验的 call 都会覆盖同一 Option<Value>。slot 不携带 request/generation 或提交次数，不能只读取最后一个值。

修订：回合锁覆盖清空、scope 建立、运行、事件收尾、取值和最终接纳；按实际成功 StructuredOutput tool-result 的不同 tool-use ID 计数，绑定当前 request/generation。只接纳恰好 1 次；0 次为缺失，超过 1 次为冲突并拒绝。模型文本、未验证 tool-call 和 slot 内容变化不作为计数依据。取消、迟到或无归属事件隔离，不流入下一回合。

源码依据：`harness-runtime/crates/orchestrator/src/structured_output.rs` 的 is_concurrency_safe、validate_output 和 captured slot；`crates/core/src/host/orchestrator.rs` 的 OutputStream tool-call / tool-result 事件。

验收：同回合两个有效提交、重复事件、取消后 slot、旧回合迟到结果、回调归属不明以及有界重试。

## R2-04 [P1] 设备 SIGKILL 后的输入清理

初稿位置：架构稿 §6 原第 128 行。仅在设备内存的 held ledger 无法在 SIGKILL 后被 helper 读取；清理全部键也违背仅释放本系统输入的契约。

修订：down 前持久预记账，绑定 device epoch、进程启动身份、输入身份和操作序号；确认记录后才正常提交。外部 supervisor 确认旧进程退出及旧代次封闭，使用绑定账本的 cleanup authorization 启动 recovery-only helper，只允许释放登记键/按钮，不接受 down、移动或业务动作。释放不明、账本损坏或旧进程退出未确认时隔离输入。

验收：在预记账、down、up 两侧逐点 SIGKILL；核对 supervisor 可见性、专用 cleanup 权限、unknown/隔离及新 epoch 的重新授权。写盘压力不阻塞停止和已有输入释放。

## R2-05 [P2] 固定任务集、分母和停止测量

初稿位置：架构稿 §9 原第 205 行。百分比目标未配套冻结任务清单，停止延迟目标没有在本稿完整保留。

修订：M2 Max / 64 GB 基线；20 项 fixture × 5 个预置 seed，共 100 次；20 次 TextEdit 原生验收；语言/主题/布局和版本在实验前冻结。夹具、原生及故障注入分别报告。业务分母包含全部已接纳任务，unknown/超时/已接纳后 unsupported 不剔除。冷启动与热运行分别统计。

停止测试正常/共同负载各至少 100 次，记录可信停止事件、门关闭、最后新增输入、释放及实际停止证据，恢复 <=150 ms / <=250 ms / <=1 s 等初始目标。API 回执不能冒充 OS 实际接收或界面停止。未来实测最大值不构成硬保证上界。

验收：固定 fixture/Profile/artifact manifest、样本量和失败记录；不得通过删除困难样本或扩大 TTL 达标。

## MediaPipe 专项补充

- 原始 FramePacket 由设备产生，worker 不重建采集时间、不拥有 binding、任务或输入许可。
- Observation 保留 source frame、worker epoch、request generation、模型/graph/preprocessing revision、坐标变换和证据组；定向 ROI 绑定相关 task/binding generation。
- Graph timestamp 与可信源采集时间分别建模。异步 AX/OCR/神经流按来源与最大年龄融合，缺失流推进 timestamp bound 或明确缺失。
- ADD_IF_NOT_FULL 和宿主帧/字节上限形成真实背压；FlowLimiter timeout 不替代真实时钟 watchdog。
- Cancel/WaitUntilDone 后可能仍有输出回调，host 先失效 pending context 再取消图；输入撤销和释放独立于图。
- 首个神经分支是 MobileNetV3 small 的 ROI embedding，必须报告候选检索质量；相似度不能直接证明控件语义、身份、可操作性或业务成功。
- 当前已有官方 LiteRT inference calculator；macOS Metal 的具体 graph/模型/签名资源组合仍需 M0 验证，CPU 基线不宣称 GPU。

官方依据：

- [MediaPipe C++](https://developers.google.com/edge/mediapipe/framework/getting_started/cpp)
- [ImageEmbedder 与模型](https://developers.google.com/edge/mediapipe/solutions/vision/image_embedder)
- [ImageEmbedder C++ / ROI / 回调契约](https://github.com/google-ai-edge/mediapipe/blob/master/mediapipe/tasks/cc/vision/image_embedder/image_embedder.h)
- [CalculatorGraph 取消和背压](https://github.com/google-ai-edge/mediapipe/blob/master/mediapipe/framework/calculator_graph.h)
- [FlowLimiter](https://github.com/google-ai-edge/mediapipe/blob/master/mediapipe/calculators/core/flow_limiter_calculator.cc)
- [LiteRT C++ SDK](https://developers.google.com/edge/litert/next/cpp_sdk)
- [现成 LiteRT inference calculator](https://github.com/google-ai-edge/mediapipe/blob/master/mediapipe/calculators/tensor/inference_calculator_litert.cc)

## 验证证据与剩余门禁

独立复核关闭记录：

| 问题 | 状态 | 修订依据 |
|---|---|---|
| R2-01 | 已关闭 | 架构 §2：ReserveAttempt、唯一映射、ActivateAttempt、Execution 复验和未知额度保留 |
| R2-02 | 已关闭 | 架构 §7：每 root 独立 runtime/session/slot/recorder 及生命周期 |
| R2-03 | 已关闭 | 架构 §7：回合锁、有效提交 ID 计数、恰好一次接纳及迟到事件隔离 |
| R2-04 | 已关闭 | 架构 §6：down 前持久预记账、进程身份确认及 cleanup-only 授权 |
| R2-05 | 已关闭 | 架构 §9：硬件、任务清单、分母、冷热统计和停止测量 |

最终批准仅覆盖架构契约。MediaPipe/Laya 集成、Harness recorder 的真实可观测性、SIGKILL 清理、签名资源和性能目标必须经过 M0 / 后续实测，当前没有实现通过结论。

本轮已完成资料/源码核验、保存 R1、修订架构与入口。新文档的本地链接、Markdown 代码围栏和空白检查在修订后执行。历史 I0 的 36 测试在本会话此前通过；本轮文档工作没有重跑，也不将其提升为 MediaPipe/真实设备验收。

M0 必须实际验证 MediaPipe arm64 构建与模型、所选完整 revision/哈希、CPU 质量/共同负载、Harness 精确工具隔离/结构化事件/资源生命周期，以及设备预记账和真实停止。上述门禁未执行，不能标为通过。
