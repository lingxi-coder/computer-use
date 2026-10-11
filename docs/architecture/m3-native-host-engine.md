# 原生 Host/Engine 接纳与 Operator 窗口授权

2026-10-02。自有 GuardFixture 的真实任务已通过 `Supervisor → cu-host → Engine → Reaction/Attempt → cu-device/ExecutionCore → NativeAxBackend`。Operator 的窗口授权、原生确认对话框和撤销另有真实 Electron UI 验证。两份证据分别覆盖任务执行和产品授权，不将它们合并宣称完整 GUI 执行验收。规划仍为显式登记参考后端，真实 S1/S2 未接线。

## 观察与同意

默认启动仍为 observe，原生输入 unavailable，所有文本输入字段私有。没有获准读取文本时，Swift watch 保留 AX 会话，持续更新窗口元数据，像素仍被扣留。完整 AX 读取和 observer 通知覆盖分别报告健康状态。

`ObservationView.observed_target` 是只读元数据：原生设备时钟/会话、完整且未过期的 AX 快照、已知标准非模态窗口，或者唯一标准后台窗口。直接 AX 不可用、窗口歧义、读取不完整或超过 2 秒时没有候选。这些元数据不要求像素租约，也不提供输入权限。

main 的 `NativeSession` 从当前投影选择登记 bundle、PID、窗口；renderer 只能传 `snapshot_id`，不能提交任意进程或窗口配置。main 弹出原生对话框，默认 Cancel，显示实际窗口、应用和 PID。确认后再次检查新鲜元数据中的同一进程/窗口；改变、过期、停止、撤销或子进程退出会拒绝迟到同意。有未结束根任务或未确认输入清理时不能授权替换。

同意后关闭旧进程和管道，再启动固定范围的新会话，仍处于 observe。`native_target_window_id=window-N` 进入 Rust backend 和 Swift `--execute-window`；Swift 独立核对登记应用、进程起始身份、窗口所有者和每个动作的原始证明。source 的登记文本读取策略也限定同一 PID/bundle/窗口。secure/未知 subrole 和其它窗口保持私有。文字读取和输入许可仍是不同的门。

撤销立即关闭新任务准入并向设备直达取消，等待旧进程退出，再回到无输入的观察会话。没有恢复旧任务、许可、绑定、mode 或未完成输入。会话替换隔离旧投影、回执和模型结果，决策 worker 丢弃旧请求并为新 host 建立新的去重空间；停止界面丢弃旧设备的已确认状态，但不抹掉 unknown 清理。

授权只在本次产品进程内存在，启动选项与环境变量不能保留输入同意。`CU_NATIVE_REFERENCE_PLANNER=1` 仅选择 GuardFixture 的登记诊断规划器，不能授权窗口，也不能用于 TextEdit。未配置模型的原生 host 不回退模拟规划器。

## 在线任务与回执

真实测试通过普通 `CreateTask → ConfirmGoal → SetMode(execute)` 进入任务，确认后的根在 observe 保持 Ready。参考编译器精确保留 `Fill document: ` 后的中文、换行和 Emoji，生成登记 EnterText/Revisit 计划及冻结 `ui_revisit` GoalSpec。实际动作仍经效果门、根预算、决策消费、唯一 Attempt 映射、admission outbox、当前证明、许可与设备准入。

本轮修复了真实管道暴露的边界：

- task 文本按 UTF-8 字节有界，允许换行、回车和 tab，拒绝其它控制字符；标识符规则不变。
- native host 租约为 750 ms，有界动作 250 ms，续约频率符合设备上限，不能用模拟设备的 5 秒租约周期。
- 原生结构更新后的短静默期只推迟新决策，不延长证据或许可。非活动窗口增加/重排不改变实际输入层 revision；活动窗口和模态状态改变仍撤销依赖。
- 单个异步 AX 调用在已知实际返回时可形成机械 Completed；固定 `command_end_ms` 保留，业务效果另由新来源验证。未知、取消、排队和多步输入不采用这条捷径。
- 旧来源失效仍撤销绑定与许可；已有确定终态且没有未决提交的技能可以等待新证据。提交中、运行中或机械 unknown 仍暂停并查询，不重放。
- 连续原生来源的 AwaitEvidence 优先等待流，超时才重建捕获。已知 EnterText 成功为后续 UI 返回验证设立新的时间下限。
- 匹配设备回执后，Reaction 写入 outbox 的 `delivered=1`，与机械/业务成功无关。未产生输入就终止的 Attempt 保留未投递并记录终态和预算退款，不伪造设备投递。

旧参考驱动器此前漏掉跨 helper 的快照序号连续性，重建后新快照被 StateCore 拒绝。本轮按生产 cu-source 的同一会话规则递增序号，并检查接纳错误；真实观察时间、快照 ID、帧来源和失败下限不变。现场失败保留在参考 workflow artifact 的 `failure_history`。

## 证据和复跑

```sh
npm run smoke:macos-host-engine
npm run smoke:macos-operator-authorization -- --runtime /absolute/path/Electron.app/Contents/MacOS/Electron
npm run smoke:macos-device
npm run smoke:macos-text-values
npm run smoke:macos-value-workflow
npm run smoke:macos-device-pipeline
bash scripts/check-all.sh
```

[Host/Engine 记录](../../artifacts/m3-native-host-engine.json)在执行前记录源码集合、schema 和实际二进制哈希，导出逐条 decisions、attempt_map、attempts、outbox、冻结根和 EffectResult。一次 setter、两次导航、四个终态 Attempt（其中一个未产生输入并退款），最终谓词 satisfied 后根 completed。效果证据非空且满足实际输入结束下限，因果 unknown。模拟 world log 为空。拒绝窗口同意不换会话；获准窗口范围真实启动；撤销后观察恢复且调用数不增加。

[Operator 记录](../../artifacts/m3-native-operator-authorization.json)使用实际固定版本 Electron、renderer/preload、main 和原生 NSAlert。parent-only 测试驱动器仅能在自身 Electron 父进程内找到指定同意文字并按 Cancel/Allow；没有替换原生对话框。UI 验证启动只读、默认 Cancel、精确窗口授权、普通目标确认在 observe 保持 Ready、撤销关闭旧根与输入、热键保留和零 renderer/CSP 错误。该测试不启用 Execute，夹具输入计数为零。

全量检查：532 Rust、98 TypeScript、154 Swift（55 source + 99 device），无失败或忽略；格式、Clippy、生成类型、真实 IPC、类型检查和生产构建通过。ConsentDialogDriver 另经 Swift 6 严格构建及真实 UI 执行。既有文本边界、设备 unknown/停止/死亡及只读守卫分别现场回归。

## 未完成的产品验收

本节是 v0.17 当时的边界；v0.18 已补齐[登记窗口激活与真实 GUI 任务](m3-native-operator-execution.md)。新的效果范围、执行指示和现场证据以该文及 MVP 跟踪为准。

普通 Operator 当前需配置原生目标。窗口激活原语和执行期间不抢焦点的指示尚未交付，因此这次授权 UI 与实际任务证据分开；完整 GUI 输入任务仍待验证。TextEdit 仅登记，不代表已完成编辑、保存和重开。其它原语、真实 S1/S2、Laya、MediaPipe、固定任务集、故障/环境/性能矩阵及签名安装仍待完成。

Supervisor 的会话数据库仍位于临时目录，替换/退出后清理；artifact 在清理前导出测试账本，不等于产品持久历史或重启恢复。M4 的重启 revalidation、停止状态跨设备重启和不重放验收仍开放。完整范围继续由主设计 §26、§28 和[首个 MVP 跟踪](../mvp-status.md)管理，M2/M3/MVP 未宣布完成。
