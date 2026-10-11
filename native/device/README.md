# macOS 被动设备守卫

Swift 6、strict concurrency complete、warnings as errors，macOS 14+ / Apple Silicon。
此目录与 `native/macos` 的截图/AX/OCR source helper 独立编译。默认会话只读；受信启动器固定 PID/bundle 后可以使用 AXPress/登记 AXSetValue 单次提交入口。没有截图、OCR 或 CGEvent posting。

`GuardState.swift` 持有跨线程的 Sendable 状态；`DeviceMain.swift` 管理控制 reader、监听 run loop、独立 watchdog 与单请求 AX worker；`HitTest.swift` 与 `BindingProof.swift` 提供完整 fp1 字段的有界只读诊断和查询结束复验。`Authority.swift` 固定核心证明、时钟锚点、截止时间、撤销记录和意图 ID；探测还核对焦点、enabled、非模态与 AXPress 能力。诊断 matched 和探测 eligible 的 input_authorized 始终 false。所有 AX refs 在 worker 内，tap context 只在其 run loop 访问并在注销前保持存活。

构建、自检、管道协议、实际夹具证据和未验证范围见[设备守卫说明](../../docs/architecture/m2-device-guards.md)与[只读权限探测](../../docs/architecture/m2-native-authority.md)。

`AtomicSubmission.swift` 使用同一命中 AX 句柄和有序提交门；独立硬 watchdog 不依赖输入锁。受信固定目标的生产后端已接入，见[真实 cu-device 验证](../../docs/architecture/m2-native-device-pipeline.md)。默认 Operator 仍只读。[提交与未知回执验证](../../docs/architecture/m2-native-atomic-submit.md)仅操作本次启动的自有夹具。

登记文本的实际焦点、字节预算、隐私读取与返回读回边界见[文本参考工作流](../../docs/architecture/m3-native-value-workflow.md)。回执的实际调用结束与权限结束分别记录，未知结果不重放。
