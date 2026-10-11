# M2 原生被动设备守卫

2026-10-01。`cu-device` 的 `macos-observe` 后端现在可以运行独立的 `cu-macos-device-guard`。此批次交付被动监听、只读命中诊断和租约/进程失效处理。**原生 `input` 与执行用 `hit_test` 能力仍为 unavailable**，没有 CGEvent posting、AX action 或按住原语。

## 运行与能力

```sh
npm run build:native
npm run build:macos-source
npm run build:macos-device
npm run check:macos-device
node --import tsx scripts/smoke-macos-device.mjs
```

最后一项自行构建只读权限探测示例，打开和清理本项目自己的临时窗口，检查系统范围 AX 命中；不操作用户应用。需要已有 Accessibility 权限，不请求权限。被动监听只使用 Input Monitoring 预检和 listen-only tap；监听不可用时报告 unavailable，不自动重启或重新启用被系统禁用的 tap。

原生观察会话优先使用 `NativeSourceOptions.guardHelperPath` / `CU_NATIVE_DEVICE_GUARD` 指定的绝对文件路径，否则使用 `cu-device` 相邻的已构建守卫。没有守卫时继续只读观察，人工输入能力 unavailable。路径在 supervisor 启动进程前验证；Rust 启动失败和运行中失效也保持关闭。模拟会话不启动监听。

`CapabilityManifest.human_input` 依据真实 tap 状态发布变化。人工输入计数不会让只读观察在每次鼠标移动时暂停。原生本地停止热键与 Operator 一致（Command/Control + Shift + Escape），会触发设备停止；硬件热键和合成标记在签名包内的可靠性仍待独立验证。

## 所有权、协议和限额

- Swift 守卫由 `cu-device` 启动，继承私有 stdin/stdout 管道；不提供公开 socket。每条 NDJSON 携带版本 1 和当前 session epoch，单条输入/输出不超过 8192 字节，未知字段或错误 epoch 关闭会话。
- 控制 reader 使用短读立即处理 stop 和独立的 host/supervisor lease。Foundation 的填充式 pipe read 已替换，短停止命令无需后续字节。Rust 的控制 writer 使用容量 16 的队列，满队列视为失效。
- tap 的 CFRunLoop 独占 tap、callback context 和移动阈值锚点。跨线程只有 `OSAllocatedUnfairLock` 保护的 Sendable 值。只输出人工输入、热键、租约损失和能力损失的单调计数，不输出键盘内容、键码或指针轨迹。
- 未标记的键盘/按钮/滚动事件视为人工输入；鼠标微动使用 3 display points 的累计阈值。本会话的非零专用 marker 被排除。当前验证了分类逻辑，尚未通过实际事件 posting 验证 marker 的端到端传播。
- supervisor 每 250 ms 更新自己的 750 ms 租约；host 租约独立转发，上限 1500 ms。续约不清除 stop revision；一条租约恢复后，已有停止仍锁存。错误会话的 host LeaseRenew 被拒绝，不能延长设备当前租约。
- watchdog 在独立 Dispatch 队列每 10 ms 检查租约、AX worker 的 500 ms 硬期限和主 run loop 的 750 ms 活性。回调在入口 MainActor 之外创建，避免 Swift 6 后台队列断言。stdout 非阻塞写入的总预算为 20 ms，失败退出；输出阻塞不能占用控制 reader 或 AX worker。
- 状态最多每 100 ms 输出一次。Rust reader 有界解析并合并为单个最新状态，安全计数保证事件合并后仍可发现 lease/tap/hotkey 损失。首条状态预算 1500 ms，之后 750 ms 无新状态即关闭能力、停止设备、杀死并回收守卫，不自动重启。
- 管道 EOF 使守卫自行退出。设备死亡关闭继承管道，存活的守卫也退出。SIGSTOP 整个守卫时，依靠存活设备进程的检查回收；设备与守卫同时无法调度时不声称存在独立清理保证。本批次没有任何可遗留的物理输入。

## 只读命中诊断

AX worker 使用系统范围 `AXUIElementCopyElementAtPosition`，查询顶层命中，跨进程遮挡立即拒绝且不读取遮挡应用的标题。所有 AX refs 在该 worker 内创建和销毁，不经 IPC 传递。

请求包含 `request_id/pid/bundle_id/window_id/x/y/frame/binding/binding_digest`。`binding` 使用 Rust `InstanceFingerprint` 的全部 11 个字段；nullable 字段必须显式给出 null，未知字段、错误摘要、PID/window 不一致、非法 frame 或超限证明使私有控制会话关闭。摘要采用固定字段顺序、紧凑 UTF-8 JSON 与 SHA-256，格式为 `fp1:<64 hex>`。这是独立 helper 的诊断协议；后续 desktop v1 的完整 BindingProof 接线见[执行权限](m2-binding-authority.md)，I0 未改变。

诊断只接受按钮、复选框、单选框和菜单项，不读取编辑/安全字段或值。核对 PID、唯一 CG/AX 正常层窗口几何映射、角色、规范化标签和 0.5 point 内的 frame。沿目标到已匹配窗口的父级路径读取 ancestor roles、最近有标签的 AXGroup 与 subrole，读取直接父级的 children 顺序来核对 sibling index。路径最多 32 层，每个父级最多 256 个 child refs；每一层必须包含且只包含一次当前 child，同进程、无环且窗口一致。只有 group 的 title/description 和目标标签被读取，不读取其他兄弟内容、不遍历子树、不做截图或 OCR。父级/children 无法相互核实时返回 context_unresolved。

整条路径和指纹在最后同点重新命中后再查一次，同时复验 frame、CG 窗口 ID、进程内核启动时间与守卫 revision。窗口、标签或指纹不符报告 target_occluded；未知或过期报告 context_unresolved；守卫在查询中变化报告 stop_latched。只有 matched 回复携带复算的 binding_digest；其他结果没有摘要，所有结果的 input_authorized 固定为 false。查询中停止会清除迟到 matched 的摘要。

AX 单次 RPC timeout 为 25 ms，整次诊断预算 150 ms，500 ms 硬 watchdog 处理不服从平台超时的调用。邮箱仅允许一个待处理/执行/未消费回复的请求。Launch Services 的 launchDate 可缺失，所以 PID 生命周期使用内核 `proc_pidinfo` 启动时间。

**诊断已核对完整 fp1 字段，仍不是执行许可。** `InputBackend.hit_test` 继续 unresolved，原生 submit 继续 rejected。当前 `Observation/ActionIntent` 已携带[完整 BindingProof](m2-binding-authority.md)，Rust 核心固定来源实例并向 InputBackend 传递当前许可期限；此上下文已接入下述只读 worker 探测。fp1 不包含 source ObjectRef，字段完全相同的新实例可能具有同一摘要；source 实例失效与 revision 必须参与实际提交核对。一个只读 matched 不能提供 source 实例连续性、焦点/活动层许可或跨进程原子提交。AX 动作使用同一命中句柄、原子输入临界区、系统 held-state 与设备死亡清理仍待实现和验收。

[共享绑定样本](../../contracts/desktop-v1/native-binding/golden.json)由 Rust 和 Swift 自检，以及现场脚本共同使用，覆盖 nullable 字段、固定 JSON 顺序、Unicode/空白、引号/斜杠与负数半点舍入。Swift 使用明确的 POSIX locale 处理上下文大小写，避免原生 plain lowercased() 与 Rust 在希腊语词尾 sigma 上的摘要差异；样本验证不等于所有 Unicode/OS 版本的穷尽证明。

## 只读权限探测

新增私有 configure_authority、revoke_authority 与 authority_probe；GuardSnapshot 增加 authority_clock_ready，input_available 仍固定为 false。探测固定原始证明、许可作用域和 wall/uptime 锚点，拒绝重放、实例替换、revision 回退与已撤销 ID。期限在接收前已固定，查询前、结束后和发布前复验；焦点应用/窗口、enabled、非模态和 AXPress 能力也需已知且匹配。eligible 的 input_authorized 始终 false。协议、128 条记录上限、时钟异常和准确边界见[原生只读权限探测](m2-native-authority.md)。

## 实际证据与剩余工作

[夹具记录](../../artifacts/m2-native-device-guards.json)覆盖本机真实 tap 可用状态、从独立 source 投影生成的 fp1 命中、标签/分组/subrole/祖先/sibling 不符、真实分组改名、兄弟重排、同点同名按钮换位、父子关系不一致、重复 child、超限 children、同进程窗口遮挡和窗口替换，以及双租约到期、短 stop、错误 epoch、EOF、冻结回收和设备死亡退出。六类畸形绑定请求关闭进程且没有 crash/trap。仅冻结目标夹具期间，查询被有界拒绝，独立 stop 在查询返回后复验为 stop_latched。记录中的 report_ms 是诊断回复/状态报告耗时，不是实际输入停止时延。没有键鼠或 AX 输入；硬件热键、合成 marker 传播和签名权限明确未验证。

最新现场记录还覆盖核心实际生成的 SubmitContext 到真实 native worker、探测不消费 guard 通知、重放/实例不符/停止序号回退、延迟请求不获得新 TTL、查询前及查询中撤销、查询中到期与查询前停止。夹具显式实现相互一致的 AX children/parent/hitTest，声明并拒绝 AXPress，只从测试管道改动自身布局；最终 AX 动作调用次数为 0。不能由此声称任意 AppKit 控件均具备相同的 AX 句柄一致性。

完整 M2 还需要原子输入验收，以及权限撤销/锁屏/睡眠/多屏坐标矩阵、签名产品和 MediaPipe。此前图像与生产/开发 Electron 预览证据保持独立，见[图像通道](m2-image-channel.md)。

接口语义依据本机 SDK 头文件和 Apple 一手资料：[AX 点位命中](https://developer.apple.com/documentation/applicationservices/1462077-axuielementcopyelementatposition)、[AX 消息超时](https://developer.apple.com/documentation/applicationservices/1459345-axuielementsetmessagingtimeout)、[有界 children 读取](https://developer.apple.com/documentation/applicationservices/1462060-axuielementcopyattributevalues)、[AX child count](https://developer.apple.com/documentation/applicationservices/1459066-axuielementgetattributevaluecoun)、[AX 层级关系](https://developer.apple.com/documentation/appkit/nsaccessibilityprotocol)、[被动事件 tap](https://developer.apple.com/documentation/coregraphics/cgeventtapoptions)。

后续新增需受信启动器固定 PID/bundle 的 AX 单次提交；本页默认只读模式的动作计数仍为 0。该入口的有序门、独立硬 watchdog、未知回执和真实夹具证据见[原生 AX 单次提交](m2-native-atomic-submit.md)。生产原生输入仍 unavailable。
