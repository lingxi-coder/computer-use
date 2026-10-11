# M1 修复与 M2 原生观察入口

2026-09-30，基于 `m1-architecture` 的 `73a0543`。以下改动尚未提交。

## 八项审查发现

| 发现 | 修复与回归 |
|---|---|
| host 输出堵塞停止线程 | device 的 host/supervisor 输出经有界 writer，控制循环处理失败与关闭门；进程测试覆盖不读取端的停止与退出 |
| 空闲五秒丢失来源 | 监听端握手成功清除读超时；空闲超过原期限仍能完成任务 |
| 失效控件保留绑定 | 指纹含 PID；复验同一实例、活动层、enabled/in_view/secure；ensure_value 不在失效后发出回退输入 |
| 恢复查询超时卡死 | 同 intent 有界重试；再次失败保留 unknown、释放查询锁并等待显式恢复，不重放动作 |
| 无效重规划响应卡死 | 当前请求的阶段错误/过期结果结束后回退已有计划；其他请求的旧响应仍丢弃 |
| 窗口关闭后不能重开 | 关闭时隐藏，销毁时清空引用，发送前复验，保留托盘 |
| 新任务继承旧停止提示 | host 与桥接层按新任务/会话/显式恢复清除已结算确认；当前停止优先，未知释放继续提示 |
| 开发 CSP 阻断 React | 仅开发 loopback origin 使用 nonce 和限定 WebSocket 策略；生产 CSP 保持严格 |

首次进入 M2 时，复查发现未知 AXSubrole 被误当作非敏感的隐私缺陷，已改为对未登记安全策略的文本输入字段保守处理并加入自检。后续持续来源的复查补全了下面的恢复与时序边界。

## 首次进入 M2 的验证记录

`CARGO_INCREMENTAL=0 ./scripts/check-all.sh` 完整通过：410 项 Rust、65 项 TypeScript、12 项 Swift 原生自检；格式、Clippy `-D warnings`、schema/生成类型一致性、类型检查、生产构建均通过。I0 原 36 项与逐字节 demo golden 保留。

首次真实观察 smoke 启动项目自有 AppKit 窗口，经三个真实 native 进程运行：11 个 AX 对象、640×328 PNG、6 个 OCR 文本块及预期标题。初次 AX 树变化时拒绝图片，稳定后通过；窗口、进程和私有会话资产已清理。这是单次 probe 的历史记录；当前 smoke 脚本与 artifact 已由下节的持续 watch 验证替代，当前 artifact 不作为旧结果的证据。

上面是首次观察入口的历史记录。当前已经扩展为持续 SCStream，现状见下节。

## 持续来源与恢复边界（2026-10-01）

Swift helper 默认 watch，SCStream 回调只复制有界像素/元数据，AX、OCR、编码和输出独立处理；专用 AX Thread/CFRunLoop 持有 AXObserver 和有界实例注册表。通知支持不足时明确降级为每轮新实例，不声称稳定连续性。读取后再次检查 PID 启动身份、窗口身份/几何/顺序；文本超出输出预算会显式报告不完整。

帧协议保留原始 display ticks/uptime、真实回调时钟、图像身份、几何和丢弃；idle 只确认最近 Complete 的同一内容，不生成新像素。后台帧用于只读展示，执行仍需与真实活动层一致；Stopped 退休 stream，新 UUID 不能凭较早回调抢回权威；来源失效前的迟到 Complete 不能恢复执行。健康事件按 session/stream/sequence 校验，故障在有界队列中逐来源保留，时钟异常不能掩盖来源关闭。

独立复查还补上以下执行边界：

- 来源失效时关闭旧决策与提交前 dispatch，撤销绑定和设备依赖；已提交输入保留 Unknown 并暂停，恢复观察不自动重发。
- 未知/缺失/未结束的回执继续阻止恢复；只查询实际持久记录过的提交范围，多 intent 全部明确后才结算，未知输入不退款。
- 取消后清除该 dispatch 的命令关联，旧 Observe/Grant 回复不能套到新的 attempt；回复和许可期限均受 attempt deadline 约束；native lease 同时受 AX/pixel/heartbeat 期限限制，像素刷新不能延长旧 AX 的许可。

最终跨进程验证还明确区分机械结果与业务效果：弹窗消失引发 layer 撤销时，已提交 Press 保留 Unknown、暂停且不退款；显式 Resume 查询原 intent，之后用新证据完成目标。明确 Completed 但效果未确认的输入只进入目标验证，不自动再次 Press。真实模态用例检查唯一 Apply、原 intent 查询、配额保留和最终 Saved；引擎回归另外覆盖 Completed 的效果未知路径。

[真实 SCStream 记录](../../artifacts/m2-native-observation.json)由项目自有窗口经三个真实进程产生：13 个 AX 对象、640×328 PNG、5 个 OCR 块和标题识别；Complete→idle 的时间/校验和一致，帧进入 host 投影。暂停仅本次 helper 后，callback lease 失效原因可见，helper 被杀死并回收且不自动重启。窗口、三个进程和会话资产已清理。当前 fixture 的 AXUIElementDestroyed 通知不受支持，记录 observer_available=false 并降级实例策略。

上述源进程 smoke 已在本轮重跑并更新 artifact，新增了二进制接收、SHA-256 和冻结后取图拒绝。此前单纯进程级捕获的证明不替代 Electron 预览，本轮另做了真实 GUI 验收。

[真实 Electron 记录](../../artifacts/m2-electron-runtime.json)使用本机缓存且校验和匹配的 Electron 43.1.1；生产/开发窗口均验证沙箱、preload、CSP、关闭重开、目标确认和停止入口，renderer/preload/CSP 错误为零。开发真实运行额外发现 IPv6 literal WebSocket CSP 源无效，已改为限定 localhost/127.0.0.1。

原生来源降级状态的 Operator 展示、关闭重开和停止入口也通过。此前锁屏时，来源 guard 正确拒绝不可观察的窗口；恢复可观察会话后，本轮已完成健康原生帧和二进制 PNG 的生产 GUI 验收，历史失败记录保留。

上一批持续来源修订的 `CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true bash scripts/check-all.sh` 完整通过：**472 项 Rust、70 项 TypeScript、49 项 Swift 原生自检**，失败和忽略均为零。格式、workspace/all-targets Clippy `-D warnings`、schema/生成类型一致性、类型检查、生产构建与 Swift 6 严格编译均通过；I0 原 36 项与逐字节 demo golden 保留。最终检查包括真实 Unix socket 的模态恢复用例，原生自检使用生成图片。该批次受锁屏限制的健康帧 UI 已由本轮复验补齐。

完整 M2 仍未成立：MediaPipe、复杂权限/坐标/睡眠矩阵、原生命中/人工输入观察/停止看门狗和原子输入验收尚待完成。原生输入 unavailable，没有向用户应用注入输入。详见[持续观察与限制](../architecture/m2-native-observation.md)。

## 二进制图像与真实预览（2026-10-01 续接）

新增独立的认证 `images.sock`，FrameMeta + PNG 原字节经有界发送 worker 和 Node 消费 worker 到达只读预览；长度、总像素、PNG 容器、SHA-256、session/PID、重放与过期均检查。控制 JSON 和直接停止路径不承载 PNG。主进程匹配原图像身份、采集时间、几何与 host FrameView，idle 仅续期回调；renderer 只获得自己的字节副本并回收 Blob URL。详见[协议、所有权与限额](../architecture/m2-image-channel.md)。

两项真实环境问题已修复：Retina 的 contentRect 使用 surface points，要换算为 pixels；AXObserver 覆盖降级不应丢弃引擎确认的只读图像，AX/OCR 警告继续显示，执行准入保持独立。生产 GUI 真实解码了 640×328 PNG，AX 降级仍可见，renderer 错误为零。消费者滴流半包不能延长两秒总预算；后续失败也不能继承旧 UI 验收成功标记。

最终 `CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true bash scripts/check-all.sh` 完整通过：**477 项 Rust、80 项 TypeScript、52 项 Swift 自检**，失败与忽略均为零；格式、Clippy、schema/生成类型一致性、类型检查与生产构建通过。生产和开发 Electron 均真实解码 640×328 PNG，CSP/preload/renderer 错误为零，关闭重开和停止入口通过。源进程在最终代码上重跑二进制校验、冻结拒绝与回收。原生输入仍 unavailable；所有现场测试仅观察项目自有窗口，没有输入注入。

## 被动设备守卫（2026-10-01 续接）

`cu-device` 现在持有独立 Swift 守卫，真实 listen-only tap、短读控制和 host/supervisor 双租约不依赖 AX worker。监控失效触发停止、能力更新与进程回收；错误会话的 LeaseRenew 无法延长当前设备租约，并有真实跨进程回归。watchdog 的 Swift 6 MainActor 回调陷阱已通过实际启动暴露并修复；进程绑定使用内核启动时间而非可能缺失的 Launch Services launchDate。

[真实设备守卫夹具记录](../../artifacts/m2-native-device-guards.json)通过正常命中、标签/窗口不符及遮挡拒绝、双租约到期、短 stop、错误 epoch、EOF、冻结回收和设备死亡退出。完整检查为 **481 Rust、81 TypeScript、87 Swift（52 source + 35 device）**，失败与忽略均为零。短读修订另做 Swift 严格编译、自检和最终原生夹具复验。

该 tranche 没有键鼠 posting 或 AX 动作。只读命中描述符尚不含完整 fp1 证明，所以执行用 hit_test/input 保持 unavailable；硬件热键、合成 marker 传播、签名权限、原子输入与 MediaPipe 仍待交付。[设备守卫说明](../architecture/m2-device-guards.md)列出所有权、限额和准确边界。

## 完整 fp1 命中诊断（2026-10-01 续接）

只读命中核对现在包含全部 InstanceFingerprint 字段与固定 JSON/SHA-256 摘要，并在结束前再次复验有界父级路径和 CG 窗口身份。祖先角色、最近分组、subrole、直接兄弟序号、角色/标签、整点 frame、PID/bundle/window 均参与核对；每一层校验唯一 children 成员、同进程和无环。AX/CF refs 不离开单个 worker，没有增加设备 source 树。

共享样本揭示并修复了 Swift plain lowercased() 与 Rust 在希腊语词尾 sigma 上的差异，也核对 nullable 字段、Unicode 空白、引号/斜杠和负数半点舍入。现场测试的预期证明来自独立 source 的完整投影，不由设备自己生成期望值。实际分组改名、兄弟重排、同点同名按钮换位、窗口替换与遮挡被拒绝；停止期间不发布迟到摘要，六类畸形请求关闭会话且不 crash。

最终完整检查：**482 Rust、81 TypeScript、104 Swift（52 source + 52 device）**，失败与忽略均为零。格式、workspace/all-targets Clippy -D warnings、schema/生成类型、类型检查、真实进程回归、生产构建和 Swift 6 严格编译通过。完整检查后只新增现场夹具的父子关系不一致、重复 child 和 children 超限负例，并对最终脚本独立复验。

诊断 input_authorized 始终 false；该批次结束时 ActionIntent 到 native worker 的完整绑定描述符、source ObjectRef/revision、许可 generation 与截止时间接入仍待完成，后续只读接线见下文。执行用 hit_test/input 继续 unavailable；本轮没有任何输入注入，完整 M2 和真实原子输入尚未验收。见[守卫实现与边界](../architecture/m2-device-guards.md)与[当前夹具记录](../../artifacts/m2-native-device-guards.json)。

## 完整绑定证明与提交上下文（2026-10-01 续接）

Observation/ActionIntent 现在携带完整 BindingProof，包括 source ObjectRef、snapshot ID、environment/layer revision、完整指纹和原始 display frame；production pointer/AX 动作不再接受只有摘要的请求。主机从当前 EnvSnapshot 复算证明；设备固定原始 issue evidence，并按 target 跟踪最新已接纳证明。新实例即使摘要相同也会撤销旧许可、取消排队动作；证明缺失、亚点几何/层级变化与 revision 回退同样关闭旧权限。

InputBackend 的 hit_test/submit 共用完整原始 intent 和当前 SubmissionAuthority。期限取 permit、当前 evidence、root、host lease 与固定 command end 的最早值，当前证据更新不延长原始动作。共享 schema、72 项 fixture 与生成 TypeScript 已同步；I0 冻结契约、36 项测试和 demo golden 保留。

最终完整检查通过：**492 Rust、81 TypeScript、104 Swift（52 source + 52 device）**，失败与忽略均为零。新增真实 cu-device/socket 回归拒绝“摘要不变但实例引用变化”，无 world input；既有真实三进程任务、模态、停止/恢复继续通过模拟应用。最终 Rust 进程另做原生被动守卫 smoke，仅读取项目自有窗口。

该批次结束时原生 Swift worker 的权限入口和同句柄原子提交仍未实现，input/hit_test 能力继续 unavailable，没有任何物理输入。相关实现、准确验证边界和后续工作见[绑定证明与提交权限](../architecture/m2-binding-authority.md)。

## 原生只读权限探测（2026-10-01 续接）

完整 SubmitContext 已进入真实 Swift worker，固定原始证明与许可作用域、建立一次保守时钟锚点、拒绝重放和已撤销 ID。双租约、停止序号、期限和撤销在接纳、查询前后与发布前核对；焦点应用/窗口、enabled、非模态和 AXPress 能力需已知且匹配。Rust 异步探测不消费普通 guard 通知，超期或畸形回复关闭守卫。

最终全量检查通过：**496 Rust、81 TypeScript、130 Swift（52 source + 78 device）**，失败与忽略均为零。最终现场 smoke 覆盖核心到 native 接线、正常 eligible、防重放、实例不符、停止序号回退、排队后到期、查询前及查询中撤销、查询中到期与停止；既有指纹/遮挡/冻结/回收检查继续通过。脚本自行构建 Rust 示例，夹具 AX 动作调用次数为 0。

eligible 的 input_authorized 始终 false，实际输入和完整 M2 仍未验收。协议、原子提交缺口、source 实例证明边界与后续工作见[原生只读权限探测](../architecture/m2-native-authority.md)。

## 原生 AX 单次提交（2026-10-01 续接）

scoped helper 在同一命中 AX 句柄上提交一次 AXPress，最终门与停止/撤销共用锁，硬 watchdog 独立。成功和 unknown 回执保留已提交范围，不因随后停止或到期改写为 rejected，不重放。自有夹具实际调用 4 次，覆盖成功、超时、调用期间停止、调用开始后 helper 死亡；source 读回控件标题变化。默认只读 smoke 动作计数仍为 0。

最终全量检查通过：499 Rust、81 TypeScript、139 Swift（52 source + 87 device），失败与忽略均为零。当前 Rust 示例用内存核心生成权限，生产异步提交/回执仍待接入；完整 M2 和 MVP 仍进行中。见[实现与验证边界](../architecture/m2-native-atomic-submit.md)及[一期交付跟踪](../mvp-status.md)。

## 生产执行核心与原生后端（2026-10-01 续接）

真实 cu-device 已使用异步 Pending/completion 接入 NativeAxBackend，不经过 SimBackend。停止和撤销保留在途 unknown，迟到回执核对实际提交时间/序号，固定回执上界、隔离与去重防止重放；旧 guard revision 不被刚到的停止/人工输入通知自动刷新。

自有夹具的正常、超时、调用期间停止和 helper 死亡四项实际动作各一次，没有模拟输入，StopAck 本次 3 ms。全量检查通过：513 Rust、81 TypeScript、139 Swift，失败与忽略均为零。默认只读回归动作计数为 0。产品授权、其它原语和在线业务效果闭环继续推进，完整 M2/MVP 尚未达成。详见[生产接线](../architecture/m2-native-device-pipeline.md)。
