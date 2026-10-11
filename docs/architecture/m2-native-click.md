# 原子指针点击与释放确认

2026-10-03。本轮在生产 ExecutionCore/native backend 路径接入左、右、中键与修饰标志点击。默认 Operator 仍为 observe，输入仍须独立窗口授权和目标确认。该接线不代表剩余原语、通用技能或一期验收完成。

## 固定范围与提交

复用 main 固定的登记 app/PID/window 范围、内核进程起始身份、双租约、停止序号和原始绝对期限。私有 NativeMouse 请求保留 button 与四个布尔修饰位，拒绝未知字段；不新增 desktop-v1 消息。坐标必须在原始显示点框中，当前 proof 仍为同一实例。Source 和模型不能通过这个数据结构授予输入。

同一 worker 在当前前台/标准非模态窗口做实际点位命中、完整 fp1、几何、标题、祖先/分组/同名序号、enabled 与进程复验。指针输入不要求元素声明 AXPress；已声明的 AX 动作仍由原有 AX 路径执行。普通编辑元素只限登记 app、普通 subrole；secure/未知字段在读标签之前拒绝。

`macos-atomic-click@1` 固定一对同点 down/up，间隔为 0，不产生独立修饰键 down/up，也不开放拖拽/按住。事件源使用 private state，关闭硬件事件抑制，两个事件携带现有非零输入 marker 与相同明确的单调时间戳。同一有序提交门连续发出两次 CGEventPost；门内不等待来源、网络或回执。输入结束、业务效果仍是不同事实。

新 helper 在启动时只读检查 combined-session 与 HID 表中的左、右、中键状态。任一未释放就关闭该会话的输入；这个启动状态不可由迟到 snapshot 改为 clear，需要复位后显式重建会话。每次点击提交前也检查当前按钮状态和 posting 权限。这不证明任意 AppKit 追踪状态已恢复，设备死亡后的 UI/系统矩阵继续开放。

## API 返回与 release 分开

CGEventPost 只入队，没有 UI 送达回执。早先只等两次 API 返回的现场记录出现未收到预期 up，以及相同 marker/时间戳的额外事件，原始事件与控件回调都已保留。helper/tap 过早回收是待进一步隔离的因素，不能仅凭一次修复宣称根因全部关闭。

当前路径为每对事件登记一个不复用的单调 stamp，在自己的 session event tap 精确匹配 marker、up 类型与 stamp。发出后至多等待 150 ms，且不超过原始权限截止时间；等待不持有正常输入锁，不提交其它输入。确认到来才返回 applied 与实际返回时间，随后仍须新来源验证 UI 效果。确认缺失、运输失败、死亡或钟域异常保持 unknown；ExecutionCore 隔离通道，原意图缓存查询，不换 ID 重放。单步原子输入使用实际返回时间结束，不以固定 command_end 冒充输入结束。

独立构建的 `CU_OWNED_POINTER_DROP_ACK` helper 只允许 GuardFixture 输入，用于释放已到应用、确认被丢弃的验收；普通生产构建不启用该分支，也没有 IPC 开关。验收驱动另提交新意图核对 isolated 通道拒绝输入。夹具的原始 AppKit 事件、控件回调和 AX 读回是独立真值，线上 source/Execution 不消费这些测试计数。

## 验证入口与剩余工作

```sh
npm run smoke:macos-click
npm run smoke:macos-click-death
RUST_TEST_THREADS=1 bash scripts/check-all.sh
```

[现场记录](../../artifacts/m2-native-click.json)保存构建哈希、固定自有目标、输入回执、原始事件/控件计数、遮挡拒绝和释放确认丢失的 unknown。左/右/中键、组合修饰位、Control-click、遮挡和确认丢失七个设备边界场景通过；最后一个场景实际仅一对事件，旧意图不重放、通道隔离且新意图被拒绝。全量 577 Rust、121 TypeScript、518 Swift（55 source、456 device、7 Keychain）通过；Rust 测试运行器为串行，不构成共同负载报告。事件构造覆盖三种按钮和 16 种修饰位组合；时间戳/确认代次、实际结束时间、unknown 去重、启动非 clear 状态不可被更新覆盖另有回归。

这是设备边界的真实输入验收。Host/Engine 通用指针技能/AX 回退路径、直接杀实际输入 helper 的矩阵、系统/UI 复位、多屏/权限/锁屏与共同负载、TextEdit 保存/重开、真实模型及签名产品仍需完成。M2/M3/MVP 保持进行中，完整范围沿用[主设计](../design/complete-design.md)和[MVP 跟踪](../mvp-status.md)。

同一原生 helper 的既有 Operator 参考 GUI 任务另通过 21 项检查，仍为一次 AX setter、两次导航、completed 和撤销后不重放。这个回归验证新增指针代码保留已有授权/AX 闭环，不能替代通用指针技能或真实模型业务验收。原生人工鼠标移动曾触发 Partial/暂停；该失败保留且人工检测未关闭。

API 行为依据：[Apple CGEventSource state](https://developer.apple.com/documentation/coregraphics/cgeventsource)、[buttonState](https://developer.apple.com/documentation/coregraphics/cgeventsource/buttonstate(_:button:)) 与 [userData](https://developer.apple.com/documentation/coregraphics/cgeventsource/userdata)，并核对本机 SDK 头文件。硬件/窗口行为以现场证据限定，未验证情形不填入通过项。

## cu-device 三点 SIGKILL（2026-10-03）

生产输入由 cu-device 管理的独立 Swift helper 发出。本矩阵杀 cu-device，保留这个进程边界；不能据此填写直接杀实际输入 helper 的通过项。

`CU_OWNED_POINTER_DEATH_POINTS` 是独立测试构建，输入范围仅 GuardFixture。启动路径选择检查点，写入自有临时目录后有界等候精确父进程死亡；down 前还等候控制关闭。检查点超时直接失败，生产构建没有这个开关或暂停。驱动核对 helper 路径、PID 和 cu-device 父 PID，再仅向这个自有 cu-device 发 SIGKILL，并核对真实退出信号。

| cu-device 死亡位置 | 该场景实际 down/up | 旧连接与结果 | 新代次查询 |
| --- | --- | --- | --- |
| down 前 | 0 / 0 | 连接关闭，无最终设备回执，观察端 unknown | 无记录，输入 unavailable，计数不变 |
| down/up 之间 | 1 / 1 | 原 helper 完成已经开始的释放并退出，无最终设备回执，观察端 unknown | 无记录，输入 unavailable，计数不变 |
| up 后 | 1 / 1 | 原 helper 有界退出，无最终设备回执，观察端 unknown | 无记录，输入 unavailable，计数不变 |

每次恢复使用新代次 2 的 macos-observe，只查询旧意图；实际 Ready 清单必须输入 unavailable。旧 helper 在 300 ms 检查时已退出，原始 AppKit 事件和控件计数均没有额外输入。测试驱动保留已收到的 Accepted/Running，连接关闭不丢弃同批帧；最终回执缺失时独立观察结果为 unknown，不冒充死去设备的 Receipt，也未实现产品持久恢复。

[第一次失败记录](../../artifacts/m2-native-click-death-before-shutdown-drain.json)保留了一个 down、两个 up。由此修正退出竞态：准入/提交继续使用立即封闭的 inputClosed 状态；生命周期 closing 为已发出的 release 保留最多 250 ms 的 tap drain。两次 post 仍连续执行，等待在正常输入门外；关闭期间只运行 tap，不发送正常输入或向死亡父进程写输出。输出失败也直接封闭准入，避免 EPIPE 先于控制 EOF 时立即退出。独立硬 watchdog 不等待控制锁。新 stamp 清除旧 drain，防止未提交的下一次请求继承退出窗口。

[最终矩阵](../../artifacts/m2-native-click-death.json)三项通过，保存每个故障构建哈希、检查点、真实退出、只读 Ready 校验与独立计数。全量 577 Rust、121 TypeScript、518 Swift（55 source、456 device、7 Keychain）通过；最后输出管道修复另通过原生重建检查。Rust 运行器仍为串行，源不完整的只读重采集保留，未重试输入。直接 helper SIGKILL、任意 AppKit 追踪状态复位和产品提示仍开放。

另有一次[检查点未达到](../../artifacts/m2-native-click-death-injection-not-reached.json)失败：诊断显示夹具失去前台且 AX 没有可读窗口，输入计数为零，未完成因果隔离。最终驱动在每个独立场景的准入前显式置前自己的夹具并核对前台状态，再取新证明；运行中的人工输入/焦点失效继续按生产门禁暂停或拒绝，不恢复或重试旧意图。驱动同时保存未达到检查点时的子进程输出与退出状态，不能把普通拒绝当作成功注入。
