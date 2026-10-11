# 登记窗口激活与真实 Operator 执行

2026-10-02。自有 GuardFixture 首次从真实 Operator 前台完成任务：启动只读、原生窗口同意、普通目标确认、选择 Execute、登记窗口激活、一次文本写入、两次页面导航、新来源读回、根完成和撤销。规划仍为显式登记参考后端，未使用真实模型；该证据不代表完整一期验收。

## 激活权限与普通输入隔离

`ObservationPurpose::WindowActivation` 只授权一个 `ActivateWindow` 原语，目标必须为完整标准非模态 AXWindow 根证明，依赖严格限定为 `authorization` 与 `window`。`TargetInput` 保留原语的普通前台、binding、geometry 等依赖。ExecutionCore 在许可、提交和更新证据时核对用途；窗口元数据不能变成后台 click、AXPress、SetValue 或多步输入。变更 window 依赖立即撤销排队激活。

StateCore 的窗口元数据要求已登记 app/PID/窗口、完整 AX、标准窗口、没有模态窗口、当前几何和最多 2 秒年龄。AXWindow 不套用普通控件的 AXEnabled 属性；普通控件仍要求 enabled。直接 AX 失效后，健康恢复不能复活旧窗口证据，必须接纳新的观察。背景窗口元数据从不使普通编辑证据变成 usable；激活结束后仍等待前台和新像素证据。

固定窗口范围由 main 同意路径交给 host/device。Engine 仅在 execute、已确认根、已建立租约、设备已注册根且窗口证据当前时建立导航 Candidate，经过同一效果门、根预算、决策消费、唯一 Attempt、admission outbox 和许可路径。`ui.activate_window` 有独立技能及后置谓词；每个根启动时处理一次，已在前台则不产生输入。没有范围、过期或模态窗口不触发激活。

Swift 独立核对固定 launch 范围、内核进程起始身份、CG 窗口所有权、AX 标准窗口角色、非模态、标题、几何及 AXRaise 能力。在同一有界、可撤销提交门内设置应用 AXFrontmost，再 Raise 选定窗口；两个 OS 调用中任何一个开始后失败都为 unknown，不能重放。窗口引用只存在于 worker 栈内；没有点位查询外部遮挡应用，也没有使用“所有窗口激活”。权限、双租约、停止、撤销、原始固定期限和独立 watchdog 继续成立。

OS 返回成功不能证明前台切换成功。Verification 从输入结束后的完整 AX 观察计算 `window.active@1`，写入独立不可变 EffectResult，确认范围为 `current_ui`，因果 unknown。该范围只用于导航后置条件；GoalCompiler/GoalSpec 拒绝它作为根任务确认范围。业务根仍保持冻结的 `ui_revisit` 或 `commit`。

## 不遮挡的执行指示

main 在原生任务执行时隐藏完整控制台，将现有窗口收为 280×124 的指示，禁用 focus 和 resize，仅使用 showInactive。投影提供的全局显示点用于在显示器 workArea 四角寻找与整个目标窗口不相交的位置，支持负坐标显示器；没有可信几何或无空位时保持隐藏，托盘和全局 Stop 仍在。该几何不进入输入授权。

指示显示任务状态和 Stop。停止、暂停、用户确认或根终态恢复完整控制台，使用 showInactive，避免自动抢回前台。菜单/第二实例唤起执行指示也不抢焦点。直接点击、托盘和热键仍走独立设备停止通道。

激活期间屏幕证据可能尚未到达；连续原生流先等待新证据，原有限期限到达后才刷新捕获，避免每次短暂缺失都销毁刚恢复的流。不延长旧像素时间、权限或动作期限。现场人工鼠标移动曾正确触发暂停，未关闭或忽略人工输入监听；有限事件诊断只记录 CGEvent 类型，不记录按键内容、坐标或文本。

## 当前证据

```sh
npm run smoke:macos-operator-authorization -- --runtime /absolute/path/Electron.app/Contents/MacOS/Electron --execute
npm run smoke:macos-host-engine
npm run smoke:macos-device-pipeline
npm run smoke:macos-text-values
npm run smoke:macos-device
bash scripts/check-all.sh
```

[GUI 执行记录](../../artifacts/m3-native-operator-execution.json)包含实际固定 Electron、构建前哈希、真实原生对话框结果、精确窗口、指示 bounds、fixture 输入计数、根终态和清理前导出的 decisions/attempt_map/attempts/outbox/EffectResult。当前任务有一个激活 Attempt，业务输入只有一次 setter、两次导航；窗口效果与根效果分别落账。新来源谓词成立后根 completed，撤销后输入关闭、旧根不恢复、调用数不增加。测试驱动器只在自己的 Electron 父进程按指定原生同意按钮，不替代 NSAlert；线上 StateCore 不消费 fixture stats 或独立验收账本。

全量 537 Rust、100 TypeScript、159 Swift（55 source + 104 device）通过，失败和忽略均为零；格式、Clippy、生成类型、真实 IPC、类型检查及生产构建通过。跨语言 fixtures 增加激活用途的正/负样本，原 I0 golden 保持不变。实际 NSAlert 和指示另经截图检查。

一期仍缺其它原语/技能、真实教师/S2、Laya、MediaPipe、TextEdit 保存/重开、持久历史/重启恢复、完整故障/环境/性能矩阵、固定任务集和签名安装。全 GUI 闭环当前只验证登记参考任务；普通原生任务没有模型时仍无法通用规划。M2/M3/MVP 保持进行中，完整范围见[主设计](../design/complete-design.md)与[MVP 跟踪](../mvp-status.md)。
