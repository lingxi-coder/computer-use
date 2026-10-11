# M2 生产执行核心到原生 AX 后端

2026-10-01。真实 `cu-device → ExecutionCore → NativeAxBackend → Swift worker` 链路已接入，受信配置可对固定 PID/bundle 提交一次 AXPress。测试启动真实二进制，直接通过已认证 socket 发送类型化根任务、观察、许可与意图；没有 SimBackend，也不写模拟 world input。默认 Operator 的原生观察配置仍不启用输入。

## 异步状态与回执

InputBackend 增加 Pending（单调提交序号、固定 reply deadline）、poll_completion、revoke 和 abandon_pending。状态机继续同步运行，后台管道 IO 使用有界 writer/reader，没有引入异步运行时或让停止循环等待 AX。原生后端的 CheckedDuringSubmit 只声明“在提交时使用同一命中句柄复验”，不伪造已经匹配的摘要。

Core 同时只保留一个在途提交，尚未收到回执时不推进步骤或派发另一动作。重复意图返回原记录，不再次入队。停止/撤销先关闭旧许可，并经原生 writer 立即通知 helper；在途记录暂为 unknown，仍保留核对槽位。迟到 applied 记录真实提交时间，取消后的已提交前缀为 partial；明确未调用则恢复相应 cancelled/rejected；unknown 或畸形回执隔离共享通道，并撤销旧许可。

回执核对 session、intent、提交序号与实际调用时间。调用开始必须在原始固定权限范围内，返回可以在 command end 之后。略晚于外壳已采样时间的提交时间被保留，等待当前时钟追上，不延长权限。缺回执的等待上界固定为原权限 deadline + 最多 500 ms；达到上界关闭 transport 并记录 unknown，不重试。在途状态未结算时禁止恢复输入通道；unknown 后恢复也不会复活旧许可。

单次命令结束不能凭缺少回执宣称 completed；异步回执后尚未执行的步骤也不能越过固定 command end。现有同步模拟器、I0 36 项与逐字节 demo golden 保持通过。异步路径当前只支持无持有状态的原子输入；异常 backend 返回异步 held 原语时隔离通道。

## 原生范围与守卫版本

新的受信 Configure 设置为 `backend=macos-ax-press`，并要求 native_guard_path、native_target_pid、native_target_bundle。当前登记 bundle 为项目 GuardFixture 与 TextEdit；实际现场执行证据仅覆盖 GuardFixture，不能由此宣称 TextEdit 已验收。没有该配置时继续 macos-observe，配置缺失、未知应用、非法 PID 或 helper 失败不能获得输入能力。其它原语继续明确拒绝。

NativeAxBackend 独占该 helper 的 client；Shell 与 Core 通过同一 client 服务两条租约、停止和撤销。Ready/capability 的 input/hit_test 只有在真实权限、人工输入监听、authority clock、两条实际租约健康且 Core 未隔离时才 available；helper 的只读诊断回复仍不产生输入令牌。

每次提交固定 Shell 已与 Core 对账的 guard revision，不能用 reader 刚收到的更高版本自动替换它。这样，人工输入/停止通知在 Core 视图之后到达时，旧许可不能偷偷获得新的守卫版本。Native worker 仍做同一 AX 句柄上的完整 fp1、焦点、非模态、enabled 和最终权限复验。

## 当前验证

```sh
bash scripts/build-macos-source.sh
node scripts/smoke-macos-device-pipeline.mjs
node --import tsx scripts/smoke-macos-device.mjs
bash scripts/check-all.sh
```

生产链路 smoke 启动项目自有夹具、真实 cu-device 与 Swift helper。四项分别为正常成功、AX 超时、实际回调开始后的停止、实际回调开始后的 helper SIGKILL。夹具最终 AX 动作计数为 4，每项仅一次；source 在随后读回控件标题变化。成功回执含一个实际提交时间，超时/停止保持 unknown 并含已知调用范围，丢回执为 unknown 且不编造提交时间。重复请求均未增加动作。调用中停止的 StopAck 本次实测 3 ms；这不是物理界面停止上界或性能验收报告。

另用真实 helper 验证旧 guard revision 的请求被拒绝，动作计数不变；真实生产遮挡请求返回 TARGET_OCCLUDED，不丢失后续恢复所需的具体原因。默认只读 smoke 完整回归，动作计数为 0。全量检查通过：513 Rust、81 TypeScript、139 Swift（52 source + 87 device），失败和忽略均为零；格式、Clippy、schema/生成类型、类型检查、真实 IPC、生产构建和严格 Swift 检查通过。[现场记录](../../artifacts/m2-native-device-pipeline.json)保存生产链路与模拟输入的区分。

12 项确定性异步核心回归覆盖在途去重、停止后的三类回执、原始提交时间、缺回执上界、固定结束、剩余步骤、错误会话/意图/序号/时间、未来时间暂存、跨 epoch 核对及隔离恢复。配置/能力测试覆盖登记范围、必填项与没有 helper 时不可声明 ready。

## 仍需交付

这是实际执行链路，不是完整应用任务闭环。独立测试驱动器根据真实 source 的 BindingProof 构造 Observation；产品 Operator 的目标授权、持续来源与技能、在线 EffectResult 和根完成仍须接入并验收。其余一期原语、CGEvent marker/真实热键、系统 held 查询与设备死亡隔离、签名权限和环境矩阵仍未完成。native 不拥有 source AX 注册表，任意同布局实例生命周期仍不能仅凭 native 指纹独立证明。模型、神经来源及完整一期任务/签名报告见[首个 MVP 跟踪](../mvp-status.md)。
