# M2 原生只读权限探测

2026-10-01。本页记录只读探测批次；后续独立提交入口见[原生 AX 单次提交](m2-native-atomic-submit.md)。完整 SubmitContext 已通过 Rust 私有管道进入 Swift AX worker，增加独立截止时间、撤销记录、意图防重放和查询期间的守卫复验。结果只表示本次只读检查的 `eligible`，`input_authorized` 固定为 false；没有可复用输入令牌。原生 backend 的 submit 仍 rejected、执行用 hit_test 仍 unresolved，能力清单仍 unavailable。

## 接线与所有权

`NativeProbeRequest::build` 从执行核心提供的原始 ActionIntent 和当前 SubmissionAuthority 构造请求，核对 session/surface、permit generation、dependencies、原始与当前 BindingProof、证据新鲜度和固定 command end。只接受一个 offset=0 的 AX Press 或无修饰左键 Click。点击必须在原始 display frame 的半开范围内；不把原始亚点几何替换为 fp1 的整点 frame。

`GuardClient::probe` 非阻塞发送，`poll_probe` 消费单次回复，设备可以继续服务停止、租约和撤销。探测不会消费普通 guard 状态通知。连接重建先推进核心 epoch，再用实际 DeviceIdentity 配置守卫。CoreEvent::PermitRevoked 经同一私有 writer 转发，撤销后的 ID 不能靠更高 generation 恢复。

Swift 控制 reader、watchdog、tap 与 AX worker 共享 GuardState 的同一个锁；跨线程只传 Sendable 值，不传 AX/CF refs。NativeAuthorityLedger 在锁内固定最初收到的 root/task/permit/source 实例描述符及 dependencies，拒绝同许可的实例替换和 source sequence/environment revision 回退。原始和当前 source ObjectRef 必须一致，但 worker 没有 source 的 AX 注册表；这些引用的实际生命周期仍由来源和执行核心证明，不能仅凭 native 的 fp1 命中声称任意同布局实例连续。

## 私有协议与期限

守卫仍以 `--epoch N --marker I64` 启动。新增命令沿用版本 1、精确 session epoch、8192 字节记录上限和私有 stdin/stdout：

| 命令 | 数据与行为 |
| --- | --- |
| configure_authority | clock_origin_ms、surface{id,epoch}、clock_domain；domain 必须为 macos/{epoch}/wall；单次固定锚点，相同重复配置不重置时钟 |
| revoke_authority | permit{id,generation}；包括尚未接纳的 ID，保存整个 ID 的撤销记录 |
| authority_probe | request/intent/task/root、permit、core stop_seq/native guard_revision、surface/domain、当前与原始 evidence、source sequence、开始/结束/有效期、两份完整 BindingProof、dependencies、action/x/y |

新命令及嵌套证明拒绝未知字段。结构/协议畸形关闭会话；上下文、期限、撤销和停止的语义拒绝返回有界结果。诊断和探测共用一个待处理、执行中或未消费回复的邮箱。意图、许可作用域、撤销 ID 各最多 128 条；不淘汰旧记录以复活旧权限，耗尽时拒绝，撤销记录耗尽则关闭控制会话。

Swift 配置时先采集 uptime，再采集 wall。共享绝对期限按该固定锚点转换为本地 uptime，并保守减去 1 ms；请求在管道中排队不获得新的相对 TTL。单次剩余期限最多 1500 ms，且不得超过固定 command end。wall/uptime 回退或 elapsed 差超过 100 ms 使权限时钟永久失效，增加 guard fault/stop revision；不能重新配置解除。此处仍使用现有 wall offset，并未实现生产 boot identity 或完整睡眠时钟保证。

Rust 接收端同时使用共享绝对时间和发送前捕获的 Instant 限制等待。过期回复、错误 epoch/request/digest、重复或未请求回复，以及 input_authorized=true 都使守卫失效；eligible 回复还要核对最新 guard revision、两条租约、权限与时钟状态。停止状态通知独立保留。

## 原生只读复验

接纳要求 AX 与人工输入监听可用、实际 host/supervisor 截止时间未到、native guard revision 精确匹配、core stop floor 不回退、许可未撤销。worker 在 AX 查询前、结束后以及主线程发布前重新核对相同 ticket。查询预算为固定权限期限与 150 ms 诊断预算的最早值，500 ms 硬 watchdog 保留。

查询复用完整 fp1 的有界路径/几何/窗口/内核进程身份检查，并增加前后两次活动上下文核对：系统焦点应用 PID、焦点窗口与命中窗口 CFEqual、控件 enabled=true、AXModal 已知为 false、有界动作列表包含 AXPress。未知属性拒绝。查询中撤销、过期或停止清除迟到摘要。

发布前复验与未来实际输入提交之间仍需要同一有序临界区；本批次没有提交操作，因此 eligible 不能用于另一次调用的输入授权。实际 AX 动作必须保留同一命中句柄并在提交前重新核对，系统 held-state 与死亡清理也仍未实现。

## 验证与边界

运行方式见[设备守卫](m2-device-guards.md)。smoke 会自行构建 `native_authority_probe` 示例，仅启动项目自有夹具。示例的 ExecutionCore 使用内存 SimBackend，取得核心实际生成的 SubmissionAuthority 后送入真实 Swift worker；这验证了核心到 native 的完整上下文接线，没有证明原生输入路径。

最终全量检查通过：496 Rust、81 TypeScript、130 Swift（52 source + 78 device），失败与忽略均为零。格式、Clippy -D warnings、schema/fixture/生成类型、类型检查、真实进程回归、生产构建和 Swift 6 严格编译通过。新增单元检查覆盖固定锚点、时钟异常、延迟请求、过期、撤销、停止序号、实例替换、防重放和注册表上限。

最终[现场记录](../../artifacts/m2-native-device-guards.json)覆盖真实 eligible、同意图重放、停止序号回退、source 实例不符、排队后到期、查询前及查询中撤销、查询中到期与查询前停止；既有完整 fp1、遮挡、冻结和进程回收检查继续通过。夹具显式声明 AXPress，但实现只计数并拒绝动作，最终调用次数为 0。没有 CGEvent posting 或 AX action。[本批次记录](../../artifacts/m2-native-authority.json)保存检查范围与结果；report_ms 仍是只读报告耗时，不是输入停止性能。

完整 M2 仍需原子输入、输入持有与死亡清理、真实热键/marker、签名产品的权限撤销和锁屏/睡眠/多屏矩阵，以及 MediaPipe 和实际模型验收。
