# M2 原生 AX 单次提交

## 2026-10-05 当前提交锁边界

`GuardState.commitPress` 与 `commitOwnedValue` 已将同步 OS 调用移出共享状态锁。独立 submission try-lock 拒绝并发输入且不消费其 intent；短状态检查完成期限、许可和 intent 消费后，由原 AX worker 上的同步非逃逸 closure 调用原对象。Stop、hotkey、human notice、revoke、close、权限/租约和 snapshot 无需取得 submission 锁。原 hard watchdog 本就使用独立 liveness 锁；本轮修复的是慢调用阻塞状态锁。

调用失败或返回时钟失信均保持 `submitted=true / unknown`，隔离后续输入并禁止重放。Click/Scroll/Move 的 ACK、菜单 opener 审计和 Key 确认后的最终回执也经过统一检查，Unknown 不升级为 Applied；重复隔离只首次推进 stop/fault revision，始终撤销对应 permit。已知 commit 时间保留，无法信任的返回时间为空，不用 `max(start, returned)` 合成时刻；Rust 接收端仅允许 Unknown 缺少返回时间，Applied 仍必须携带有效的完整区间和原请求摘要。开始前拒绝不携带提交时间或绑定摘要。

`bash scripts/check-macos-stop-gate.sh` 编译专用宏测试二进制，只允许 `--self-test-stop-gate`，使用 fake perform 与纯数据/私有日志检查软件并发门。该二进制不能运行生产 Device CLI；正常构建不包含 simulated scope。已通过 commit gate 的线程仍可能在 OS 调用前被调度暂停，Stop 也不能撤销已调用的片段。因此纯检查、25ms AX 请求超时和独立 watchdog 都不能代替 §26 的正常/共同负载物理停止与释放验收。最新冻结结果及失败日志见[首个 MVP 跟踪](../mvp-status.md)。

以下 2026-10-01 记录保留当时的实现和验收边界，含共享状态锁包住 AX 调用的旧行为；后续生产原语与当前锁实现以上述最新说明为准。

2026-10-01。本页记录单次提交入口批次，后续生产接线见[执行核心到原生后端](m2-native-device-pipeline.md)。Swift worker 新增 `atomic_submit`，在完整指纹命中的同一 AX 句柄上执行一次 AXPress。本机自有夹具的真实成功、超时和调用后 helper 死亡已验证；这是原生提交入口的验收，不是生产 ExecutionCore/InputBackend 接线或完整 M2/MVP 验收。

## 提交与停止

默认 `--epoch N --marker I64` 仍只能诊断/探测。受信启动器必须额外指定 `--execute-pid PID --execute-bundle BUNDLE`；helper 启动时核对运行应用和内核进程启动身份，此后不能通过控制管道扩大范围。每次请求只能为该进程/bundle 提交 AXPress；Click 和其他原语被拒绝，CGEvent posting 尚未实现。生产 `MacosObserve` 不使用这个启动入口，input/hit_test 仍 unavailable。

`atomic_submit` 复用完整 SubmitContext 的私有证明结构，以及固定时钟、两条实际租约、stop revision、期限、撤销和防重放。与只读探测共享单请求邮箱；同一 intent 已探测或已提交时不能再次提交。没有 eligible→提交令牌转换，没有缓存 AX 句柄。

点位命中、完整 fp1 双重读取、焦点应用/窗口、enabled 和非模态检查在一个 worker 内完成。通过检查后，局部 closure 把同一个 AX 句柄交给提交门；句柄不保存到共享状态、管道或别的线程。提交门的锁与停止、人工输入、租约和撤销共用：再次核对许可、实际截止时间、进程身份和守卫状态，先登记已提交 intent，再调用一次 AXUIElementPerformAction。只在这一动作调用期间持锁；AX 查询不在锁内。Swift 的 synchronous withLockUnchecked 仅允许该 worker 局部、非逃逸 closure 使用 AX ref，不把 AX 类型声明为 Sendable。

AX 消息超时设为剩余期限与 25 ms 的最小值。独立 liveness 锁/硬 watchdog 不获取提交锁，worker 超过 500 ms 或 run loop 超过 750 ms 时直接退出；普通 watchdog 继续处理租约和时钟。操作系统和应用反应不是与检查原子完成，调用开始后可能发生效果。已经进入提交临界区的调用先于随后收到的停止；停止不能让已调用的动作重新变成“肯定未提交”。这些预算尚不是签名产品的实测停止保证。

## 回执与 Rust 客户端

`atomic_submit` 返回版本/epoch/request、outcome、reason、submitted，以及已调用时的 committed_ms/returned_ms 和完整绑定摘要。时间位于当前 shared wall-offset domain。提交门之前的拒绝没有动作区间或摘要；调用返回 success 为 applied，其余返回均保守记为 unknown。成功只表示 AX API 返回成功，业务效果还需后续新来源和 Verification。

`GuardClient::start_execution` 固定范围，`submit_press` 非阻塞入队，`poll_submission` 消费单次回复。设备主循环应继续服务停止、撤销和租约；生产后端尚未采用这组方法。接收端验证 epoch/request/摘要、动作开始位于核心固定有效期、返回时间不早于开始，以及 submitted/outcome 的一致性；畸形、重复和未经请求的回复使守卫失效。

已提交的回执允许在权限到期或停止后到达，不把 applied/unknown 重写为 rejected。写入可能部分成功、helper 死亡、回执丢失或等待期限超出均为 unknown；不重发。示例的核心使用 SimBackend 提供真实生成的 SubmissionAuthority，随后由独立 scoped helper 执行实际 AX 调用；仍未证明生产 cu-device 从 Core dispatch 到 native receipt 的完整链路。

## 本机验证

```sh
bash scripts/build-macos-source.sh
node scripts/smoke-macos-atomic.mjs
node --import tsx scripts/smoke-macos-device.mjs
bash scripts/check-all.sh
```

原子 smoke 自行编译 helper、Rust 示例和项目自有夹具，只对本次创建的 PID/bundle 发出 AX 动作。最终动作计数为 4：成功一次、AX 超时一次、实际调用期间停止一次、实际调用开始后 SIGKILL helper 一次。未知结果没有重放；独立 source 在成功、超时和丢回执后观察到控件标题变化。

只读、未支持原语、错误 PID、撤销、停止、到期、遮挡，以及查询期间的撤销/停止均没有触发额外动作。对默认只读入口另跑完整原有 smoke，动作计数仍为 0。完整检查通过：499 Rust、81 TypeScript、139 Swift（52 source + 87 device），失败与忽略均为零；格式、Clippy、schema/生成类型、生产构建和 Swift 6 严格检查通过。

[现场记录](../../artifacts/m2-native-atomic-submit.json)明确区分真实 AX 调用、CGEvent posting、生产输入能力与业务验证。本批次保留 source ObjectRef 的信任边界：native 不拥有 source AX 注册表，不凭同布局指纹独立证明任意 source 实例的生命周期。

## 后续交付

下一步是生产 Core 的异步提交/回执接线，保证主循环在待提交时仍能服务停止与撤销；随后补齐一期点击、滚动、文本/AX 设值和窗口激活的登记能力、系统输入状态查询与设备死亡隔离。签名权限/marker/真实热键、锁屏/睡眠/多屏、神经来源、模型与完整业务验收见[首个 MVP 跟踪](../mvp-status.md)。
