# 来源刷新确认与动作等待

2026-10-03。前序真实 GUI 任务曾在来源刷新仍排队时开始后续导航，随后旧流被刷新关闭，输入结果未解决，根被安全暂停。记录保留在[授权阶段诊断](../../artifacts/m4-model-authorization-diagnostics.json)。本轮补齐刷新生命周期，不降低新鲜度、来源门禁或 unknown 的停止要求。

## 时序与 owner

原有等待只有 `observed_ms >= floor_ms`：刷新请求已发出但 Source 尚处于合并等待时，旧流的新观察也可能满足这个条件。接纳新动作后 Source 再退休旧流，会撤销该动作依赖。

Environment 的 StateCore 现在分配单调刷新 generation，并持有当前未确认 generation。刷新期间普通目标和窗口激活证据均不可用于输入；仍可接收事实供观察。设备身份变化不代表 Source 已完成刷新，不会清除这个等待。

Host 只保存当前 Source 请求的消息 ID/generation 对应关系。它通过现有 Envelope `in_reply_to` 和 `CommandAck` 转交确认；旧、外来、无关联、重复或普通数据消息不能清除新一代等待。Source 连接替换或写入失败时清除传输关联，保留 Environment 的等待与正常重试预算。

Source 的 watch 模式先发送旧流失效、取消并回收旧 helper，建立新 watcher，再确认最新合并请求。确认排在新流数据之前；真正恢复输入还需新来源状态、AX/像素、几何、时间与原有守卫全部成立。确认本身不是新鲜证据。Probe 模式在对应观察 job 收尾后确认，运行中的 job 与后来排队的刷新使用不同 ticket；fixture 模式先发布对应新观察再确认。

Source 只保留一个排队 ticket，以及至多一个已运行观察 job 的 ticket，均绑定请求者连接。重连不能把旧请求确认给新 peer。刷新突发仍合并；超时重试花费既有观察预算并升级 generation。停止、取消、总期限和未知输入的收尾继续优先，不恢复旧输入。

## 回归范围

- Environment：刷新期间新鲜数据不能放行，旧 generation 不能确认新等待，设备身份变化不构成来源确认。
- Engine：初始来源缺失、刷新重试、旧/拒绝确认、新鲜未确认数据均无实际输入；正确确认和新证据之后，同一 GoalSpec/预算可继续完成。
- Host：精确关联、重复/旧/外来/无关联拒绝；带关联的普通 snapshot 不是确认。
- Source：真实 Unix 通道上的 32 次刷新合并为一次；只确认最新请求，旧 helper 已退出，新流身份不同，四项旧来源权威先关闭。

三进程 modal 回归另修正了一个夹具等待条件：实际输入已到应用但新投影仍在途中时，旧 Goal 确认可能还在缓存中；现在只等待 NeedsInput 与 Effect 确认，不提前消费旧确认。原保存效果、目标指纹与 unknown 收尾断言保留。

desktop-v1 的消息形状、Schema 和已发布 fixtures 保持；复用已有 CommandAck，不增加秘密或授权字段。内部 Engine IO 新增 generation，不赋予 Source 输入权限。真实 provider/教师、完整任务集和共同负载指标仍按[首个 MVP 跟踪](../mvp-status.md)验收。

## 当前验证与现场限制

[本轮证据](../../artifacts/m3-source-refresh.json)记录全量 574 Rust、121 TypeScript、166 Swift（55 source、104 device、7 Keychain），格式、Clippy、生成契约、类型、真实 IPC 和生产构建全部通过。Rust 测试运行器为串行；这不构成共同负载性能验收。

同一生产构建在实际 Electron 43.1.1 连续完成三次登记 GUI 任务，均通过 21 项检查：原生 Cancel/Allow、固定范围、非聚焦指示、窗口激活、一次 setter、两次导航、独立窗口与根效果、completed、撤销和零重放。共享原生对话框驱动的模型授权流程另通过 58 项检查，唯一假密钥清理成功，provider 调用为零。规划仍为显式参考后端。

早先的对话框超时和任务来源不完整均保留。失败诊断证明自有窗口仍可见时 AXWindows 可以为空，也捕获到隐藏 TUINSWindow 与无法唯一匹配的 AX 窗口；因果隔离尚未完成。测试驱动只遍历自有原生 modal、在遍历内检查期限，并为 AXPress 单独设置动作超时。夹具关闭自动补全与写作工具，失败时保存自己的窗口元数据、计数、独立只读 AX 探针及清理前账本。没有因此放宽生产 AX 完整性、像素、期限或未知输入门禁。三次成功不代表所有应用或 TextEdit 验收通过。
