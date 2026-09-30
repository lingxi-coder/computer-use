# ADR-0003：监督通道、本地 IPC 与会话规则

- 状态：已采纳，已实现（M1 阶段 S5–S6）；与 PID 校验相关的部分延后到 M2
- 相关章节：主设计 §4.6、§5.2、§14.4–§14.8；计划决定 D5

## 背景

§4.6 规定 main 管理 native 子进程、进程间用私有 Unix domain socket，但没有规定 main 与子进程之间的传输，也没有规定进程如何拿到配置与一次性凭据。

## 决定

1. **监督通道是子进程的 stdin/stdout**：长度前缀 JSON，日志走 stderr。第一帧必须是 `Configure{role, session_epoch, settings}`，其中带一次性握手凭据；stdin EOF 即 supervisor 租约失效。继承管道天然是受信路径，不需要再发明一套握手。
2. **进程之间**（host↔device、host↔source）用私有目录（0700）下的 Unix socket（0600），握手是 Hello → HelloAck → Ready：检查角色、一次性 token、会话 epoch、协议版本区间；被拒绝的一方会收到原因。Ready 带设备的真实能力清单，模拟后端如实报告 `simulated`。
3. **停止直达设备**：main 把 `StopCommand` 写进 device 的 stdin，设备立即锁存；`StopAck` 同时发给 main（消息）与 host（`DeviceEvent::Stopped`）。host 先处理停止、再把 `acked_stop_seq` 推进；落后时设备以 STOP_LATCHED 拒绝它的请求，host 等待而不重试。
4. **两个租约**：supervisor 租约 = 管道；host 控制租约 = `LeaseRenew`。设备在没有租约时拒绝一切，所以 host 在握手后**立即**申请租约并等待确认，之后每半个租约期续一次。任一失效，设备自行关闭提交门。
5. **会话规则**：新的设备会话（新 epoch）必须配新的来源进程（来源的快照带设备身份）；停止序号属于一个设备会话，新会话从零开始——否则旧会话里“已处理到 5”会让新设备锁存的停止看起来已被处理。设备连接断开（进程被杀）时，root 立即转入 SUSPENDED、清理状态 unknown、停止显示为“未确认”（§14.6），只有 operator 的显式 resume 才会在新会话上继续。
6. **时间**：模拟中各进程共享一个由 supervisor 选定的起点，都读系统时钟；这只是模拟的便利，真实部署要声明单调时钟域与启动身份（§4.4、§4.7），并核实睡眠语义。
7. **S2 不在 wire 上**：`PlanningRequest`/`PlannerOutput` 不在 36 种消息里，S2 在 cu-host 进程内运行；决策请求则经 main 中继（`DecisionRequest`/`Proposal`/`Cancel`），main 不做语义接纳。

## 未解决

- **对端 PID 校验没有做**：需要操作系统的对端凭据（macOS 的 `LOCAL_PEERPID`），要用 `unsafe`，按工作区约定只能放在平台 crate 里，随 M2 做。目前角色隔离靠私有目录、文件权限与一次性 token——**不**宣称能防同一用户下的任意恶意进程。
- 帧的二进制通道（截图）、背压与消息速率上限只定义了元数据类型，没有实现。
- 进程崩溃后的自动重启策略没有定：测试 supervisor 只演示“杀掉设备并以新 epoch 重启”。
