# 登记文本与真实 UI 返回读回

2026-10-02。固定目标的生产 `cu-device → ExecutionCore → NativeAxBackend → Swift worker` 已支持 AXSetValue。自有 GuardFixture 上完成真实文本写入、离开页面、返回并重新绑定读回，生成不可变 EffectResult 后由 TaskRuntime 完成根任务。本次没有使用模拟输入或模型自评，验证范围为 `ui_revisit`。

## 输入与读取范围

受信配置仍名为 `macos-ax-press`，沿用固定 PID、登记 bundle、完整 fp1、实际焦点应用/窗口、双租约、守卫版本与原始权限期限。它现在接受单个 AXPress 或 AXSetValue。Checkbox/RadioButton 只接受 `on`/`off` 并转成布尔值；正常 TextField/TextArea 只开放给 GuardFixture 与 TextEdit，要求 nil/空 subrole、实际 AXFocused=true 和 AXValue 可写。安全或未知 subrole、失焦、超长值与不支持的控制字符均拒绝。文本上限是 2048 UTF-8 字节；允许换行、回车与 tab。TextEdit 仅完成登记，尚无真实编辑任务验收。

source 默认仍将所有文本输入字段视为敏感。受信启动器明确设置 `native_text_policy=registered_editor` 与 `native_text_window_id=window-N`，并固定登记 bundle/PID 后，只有这个 CG 匹配窗口内正常 TextField/TextArea 的标签和值可读。其它窗口、secure/未知 subrole、combo box 等仍受原策略限制。普通标签上限保持 512 字节；登记文本值上限为 2048，精确保留 Unicode、回车和格式字符，超过上限明确标记 AX 读取不完整，不能将截断值用作完整事实。整体响应大小与遍历预算继续有界。

AX 完整即时读取与通知覆盖分别报告为 `ax` 和 `ax_observer`。缺少 AXUIElementDestroyed 等必需通知时，observer 降级，source 每次读取生成新实例 ID，旧绑定不能复活；完整当前读取仍可供重新绑定使用。页面识别只使用已观察到的活动窗口，避免把 macOS 共享提示窗口当成页面。

## 时间和未知结果

Receipt 的 `command_end_ms` 仍是固定动作权限结束；新增可选 `input_end_ms` 表示原生调用的实际返回时间，不声称应用业务效果已完成。核心检查开始/返回的顺序、回执时钟和固定最多 500 ms 回执界限；未来时间不能拖延该界限。Verification 以实际输入结束作为新证据的时间下限。

未知或部分结果另外推进证据下限，不捏造已知输入范围，也不把因果关系改成 confirmed。后续真实读回可以证明目标当前已满足，因果关系仍可为 unknown；丢回执或超时均不授权重放输入。

## 可重跑证据

```sh
npm run smoke:macos-text-values
npm run smoke:macos-value-workflow
bash scripts/check-all.sh
```

[文本边界记录](../../artifacts/m2-native-text-values.json)覆盖成功、2048 字节边界（含 ZWJ Emoji/回车）、实际 AX 超时、失焦、2049 字节值和敏感 subrole 请求。正常、边界与超时共调用三次 setter，重复请求没有额外调用；默认读取与错误窗口策略都不暴露文本。直接 source 读回不单独宣称完整业务效果。

[UI 返回记录](../../artifacts/m3-native-value-workflow.json)使用实际 SCStream/AX/Vision、要求真实帧的 StateCore、DeviceView 的完整执行证明、登记 `ui.enter_text`/`ui.read_value`/`ui.revisit` 技能、冻结 GoalSpec 和 SQLite Verification。输入后旧来源不能证明新效果；离开页面后文本节点真实移出 AX children，返回后使用新的绑定与 UI 值。最终三次动作分别为一次 setter、两次页面导航，根完成只发生在 `ui_revisit` 谓词成立且 EffectResult 已持久化之后。

跨 UI 变化的捕获若未通过隐私复验，原会话关闭并发布不可用状态。参考驱动器每次取证最多重新启动三个来源会话，保留失败下限，只消费新会话实际捕获的证据；没有延长旧证据或重试输入。提交前还要求证据覆盖本次动作的全部剩余时长。

最终全量检查为 524 Rust、81 TypeScript、154 Swift（55 source + 99 device），失败与忽略均为零，格式、Clippy、生成类型、真实 IPC、类型检查与生产构建通过。AXPress 的超时/停止/死亡路径和默认只读守卫分别另行现场回归。

## 产品边界

本节是首次参考驱动器的历史验证范围；之后已新增[真实 Host/Engine 接纳与 Operator 授权](m3-native-host-engine.md)，相应证据和当前边界以该文及 MVP 跟踪为准。参考驱动器已修正跨 helper 的快照序号连续性，最近现场回归通过，早先来源失败保留在 artifact 的 `failure_history`。

这是登记参考工作流：测试驱动器提供 GoalDraft/PlanBundle 与确认，通过 TaskRuntime 预算消费执行动作，尚未通过产品 Operator 的目标授权、在线 Reaction/DecisionWorker 接纳与完整 Attempt outbox。旧 EnterText attempt 不会被静默换成新绑定；读回使用新绑定上的 ReadValue 与 Verification。Artifact 保留实际回执和不可变效果，但参考流程的因果结论为 unknown。

默认 Operator 继续只读。真实 S1 教师/S2、Laya、MediaPipe、其余一期原语、TextEdit 保存/重开、故障与环境矩阵、签名产品和 20×5 固定任务报告仍需交付。[首个 MVP 跟踪](../mvp-status.md)保持完整验收范围，M2、M3 和首个 MVP 均未宣布完成。

短 watchdog 的来源 helper 进程测试使用共享测试通道，避免同时启动其它 fixture 消耗协议测试的期限。生产 worker 仍并发，完整性能负载保持待验收。
