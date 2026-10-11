# 原生开关技能、指针选择与目标重新定位

2026-10-03。在已登记应用的 `ui.ensure_value` 路径接入真实指针闭环。AX 能力已声明时优先使用；已知完成但新来源显示值未变时，至多改用一次指针。没有 AX setter/press 的控件直接走指针。确认仍由新 AX 读回及离开页面/返回验证完成，输入回执不是业务成功。

## 真实测试发现的问题

[首次失败](../../artifacts/m3-native-operator-toggle-before-relocation.json)中，自有开关的 AX setter 返回成功且不改值。来源在生命周期通知不可用时使用每次读取的新实例标识，旧绑定因此失效；原技能无法进入第二路径，重新接纳后重复了五次 AX 起点，实际点击为零。记录还包含 SCStream 停止和系统辅助窗口，未宣称这些环境问题全部因果隔离。

来源继续保留这种失效规则。修复落在 Engine/Reaction 的显式重新定位：已知输入完成后，由 Environment 根据冻结 GoalSpec 的语义目标、标签、组、角色和当前来源重新建立唯一绑定；Engine 再检查当前控件效果、目标覆盖和应用范围。新输入取得新 Observation、BindingProof、许可和意图 ID，不沿用旧实例、旧许可或旧输入 ID。

## 授权与额度

重新定位仅开放给 GoalSpec 自身的布尔 `ui.ensure_value`，且原输入必须明确 Completed。第一步 AX 的新读回为不同值时才允许第二步指针；第二步指针，以及直接走指针的第一步，只能读回和结束，不发第三个输入。Unknown、Cancelled、Rejected 和尚未取得回执的阶段不能使用此转换。

原 Attempt 的 action/observation 额度、输入时间下界、创建时间、激活和根期限保持。新的来源证据至少达到输入结束时间，输入仍经过当前完整性、像素/结构、焦点/几何、停止、租约及 native fp1/命中检查。当前值未知、控件不可用或效果等级升高时不点击。模型只提出计划/决策，不能建立绑定或提升输入权限。

重新定位后的读取也允许在同一个技能内验证已知点击的效果。此时动作计数已经用尽，值仍不对就结束为 EffectUnknown；不能再次尝试点击。

## 夹具与证据边界

AppPack 只给自有夹具登记 `MVP toggle` / `AXCheckBox` 的 reversible_edit 语义，其他未登记开关保持 unknown。显式参考规划器理解 `Enable fixture toggle`，目标为 Bool(true)，确认范围为 ui_revisit；它仍只用于自有夹具验收，未替代真实教师/S2。

夹具的默认 AX setter 故意返回成功但不改值；`--toggle-pointer-only` 通过 AppKit selector 能力检查移除 setter/press。独立 setter/press 计数、原始 AppKit marker 事件、控件 down/up 和真实布尔值只由验收驱动读取，线上 Source/Engine 不消费这些测试计数。两个路径均须完整的 Operator 原生授权、普通目标确认、Execute、窗口激活、返回验证和撤销。

```sh
node --import tsx scripts/smoke-macos-operator-authorization.mts --runtime /absolute/Electron --execute --toggle
node --import tsx scripts/smoke-macos-operator-authorization.mts --runtime /absolute/Electron --execute --toggle --pointer-only
RUST_TEST_THREADS=1 bash scripts/check-all.sh
```

回归包含每次读取实例变化时的一次 AX/一次指针/两次导航，检查新的实例和许可、单一 Attempt 与原 action 额度；效果等级升高时只保留已发生的 AX；unknown 不重新定位或发第二次输入；陈旧绑定不能解除输入时间下界。全量 583 Rust、121 TypeScript、518 Swift（55 source、456 device、7 Keychain）通过，Rust 运行器为串行。

本轮闭环仍为登记参考任务。其它一期原语、滚动/菜单等技能、真实模型、TextEdit 保存/重开、持久恢复、完整冻结任务集、共同负载及签名验收继续进行；[完整设计](../design/complete-design.md)与[MVP 跟踪](../mvp-status.md)的范围保持。

## 最终现场结果

| 路径 | AX setter / press | 实际点击 down/up | 返回导航 | Operator 检查与结果 |
| --- | --- | --- | --- | --- |
| [AX 无效果后回退](../../artifacts/m3-native-operator-toggle.json) | 1 / 0 | 1 / 1 | 2 | 24 项，completed |
| [无 AX 能力直接指针](../../artifacts/m3-native-operator-toggle-pointer.json) | 0 / 0 | 1 / 1 | 2 | 24 项，completed |

两个路径使用同一 host、device、source、native helper 和夹具构建。ensure_value 各只有一个已结算 Attempt，动作额度仍为 2，末次意图分别为同一个 Attempt 的第二步和第一步。根效果持久化为 satisfied/ui_revisit，读取发生在已记录输入之后。真实 provider 调用为零，MVP 未完成。

同一最终构建的原有文本任务另通过 21 项 GUI 回归，仍为一次 setter、两次导航、返回读回及撤销后不重放。三个任务的构建/源码哈希、账本、检查日志、失败记录与限制汇总在[最终证据](../../artifacts/m3-native-pointer-skills.json)。

验收保存 Execute 前零输入基线及完整原始事件前缀，仅以之后的事件验证执行与撤销区间，不能过滤该区间内的额外/未标记事件。先前一条无 marker up 出现在任务前，原始失败保留。另有取消后候选超时和 ready 阶段零输入的失败，诊断记录显示目标主窗口不在 CG/AX 可读列表。夹具使用本机 SDK 定义的 [canJoinAllApplications](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/canjoinallapplications) 与原有 Spaces 属性，配合自有 Operator 的应用窗口集；用户系统设置与生产 Source 门禁保持。该设置后两条路径通过，未据此关闭任意应用/窗口管理的完整矩阵。
