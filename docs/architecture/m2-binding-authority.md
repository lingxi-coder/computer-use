# M2 绑定证明与提交权限

2026-10-01。完整绑定数据已从 Environment 经 desktop v1 消息进入设备执行核心，再作为同一个上下文交给 InputBackend 的命中检查和提交。原生 Swift worker 的提交入口仍未实现，原生 input/hit_test 继续 unavailable。

## 来源到设备

`BindingProof` 包含 binding、source ObjectRef、snapshot ID、environment/layer revision、完整 InstanceFingerprint，以及原始 display_frame。fp1 的整点 frame 无法重建亚点几何，不能丢弃原始 frame。证明字段、文本、层级和几何均有界；摘要必须由完整指纹复算，点击必须位于绑定 frame 内，带证明的动作必须使用 display points。

主机使用 `DeviceView.bound_observation` 从当前 EnvSnapshot 复算证明。它检查完整读取、活动窗口、可用/可见/非安全控件、原实例引用、绑定摘要和层级，不能只拿此前 Binding 的旧快照 ID。相同实例的新读取刷新 snapshot/revision/sequence/时间来源，不改变实例和描述符。

证明同时进入 Observation 与 ActionIntent。设备固定原始 issue evidence，拒绝动作中的实例、亚点 frame 或重新哈希后的描述符替换。production pointer/AX 动作缺少完整证明时在入队前拒绝；I0 转换保留 proof=None，I0 schema、36 项测试与 demo golden 不变。公开 desktop v1 schema、fixture 与生成 TypeScript 已同步，没有另写一套 TS 字段语义。

## 许可持续核对

设备为每个 target 保留最新已接纳 observation 的引用。新证据更换实例、分组、层级或几何，或删除原证明时，立即撤销旧许可并取消已排队动作；即使 fp1 完全相同，ObjectRef 替换也不能通过。重新恢复旧布局不复活已撤销的许可。

续约要求 binding/ObjectRef/fingerprint/原始 frame/layer 一致，environment revision 不回退。回退证据被拒绝且撤销对应 target 的许可，即使此前许可尚未续约。其他 target 的正常证据不改变此 target 的绑定。许可原有 session、surface、root、dependencies、stop floor 和 freshness 核对继续执行。

证明的 provenance 可以刷新；动作最初的 issue evidence 和结束时间保持不变。当前证据不替换原始证明，不把另一实例的较新观察当作同一动作的权限。

## InputBackend 上下文

命中检查与提交使用同一个 SubmitContext：

- 原始 ActionIntent：root/session、permit generation、dependencies、源 ObjectRef、原证明和摘要。
- 当前 SubmissionAuthority：permit、停止序号、当前 Observation、固定 command_end_ms 和 valid_until_ms。
- valid_until_ms 取 permit、当前 evidence、root deadline、host control lease 和 command end 的最早值，不能使用后端自己的查询 timeout 延长。

同步核心到 backend 的权限边界已完成；后续[原生只读权限探测](m2-native-authority.md)已将此上下文送入真实 worker，独立核对固定期限、撤销、双租约、stop revision 和焦点上下文。它不提交输入，eligible 不提供输入令牌。同一 AX 句柄上的复验与原子提交仍待实现，不能把只读诊断的 matched 摘要缓存为授权。当前 Native backend 仍直接拒绝提交并返回 hit_test unresolved。清理持有输入的 release 继续独立于旧业务权限；没有新增物理输入或按住能力。

## 验证

共享 fixture 覆盖完整证明的往返、未知字段、错误摘要/target/PID、未规范化标签、frame 不一致、surface pixels 和越界点击。Rust 回归覆盖同摘要的实例/亚点 frame 替换、重新哈希的描述符、更换/删除证明时撤销排队动作、环境 revision 回退、当前证据刷新和 host lease 对 backend 截止时间的约束。

真实 cu-device 进程测试经私有 socket 握手、观察、授权和提交，证明“摘要未变但 ObjectRef 变化”被拒绝且没有 world input。既有三个真实进程的完整任务/模态/停止/恢复测试继续使用模拟应用，不等于物理输入验收。原生诊断的真实 AX 证据见[设备守卫](m2-device-guards.md)，本批次没有新增 CGEvent posting 或 AX action。

[该批次验收记录](../../artifacts/m2-binding-authority.json)区分执行核心/模拟应用验证与当时尚未实现的 native 权限入口；当前 worker 的只读接线、检查和现场证据见[后续实现](m2-native-authority.md)。

最终 `CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true bash scripts/check-all.sh` 通过：492 Rust、81 TypeScript、104 Swift（52 source + 52 device），失败与忽略均为零。格式、Clippy -D warnings、schema/fixture/生成类型、类型检查与生产构建通过；随后在最终 Rust 进程上复验原生被动守卫。

实际原子输入、系统 held-state/死亡清理、硬件热键与 marker、签名权限和复杂环境矩阵、MediaPipe 仍属于完整 M2 的剩余工作。
