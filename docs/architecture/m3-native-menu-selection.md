# 原生菜单选择与只读结果验证

2026-10-03。登记自有菜单已从真实 Operator 完成一次展开、一次选择、实际值读回、两次导航及冻结 UiRevisit 根完成，撤销后零输入重放。39 项 GUI 检查、11 个实际原生边界场景通过；全量 671 Rust、121 TypeScript、681 Swift/Keychain（109 Source、565 Device、7 Keychain）通过。证据与构建哈希见[进度及验收记录](../../artifacts/m3-native-menu-selection.json)。最终同组生产二进制的滚动 36 项、文本 21 项、开关 AX 回退 24 项亦通过。完整 MVP 仍在推进。

## 菜单输入的归属与连续性

Source 为固定授权标准窗口中的 opener 保留最多 8 个实际 CF 锚，逐读检查进程启动身份、根窗口、受保护父链、fingerprint 与精确几何。普通 registry 换代不证明或破坏实际 CF 连续性；明确移动、缩放、销毁或无法归属的事件仍使锚不可逆退休。缺少 Destroyed observer 支持时只声明 direct_read。

一级 MenuContext 由实际菜单、完整父链、精确框和唯一 layer101 绘制窗口产生。绘制窗口不会变成额外输入许可。项目 AXWindow 的实际 noValue 只在专用路径接受，同时检查 item/menu TopLevel、CF 父子成员关系和原聚焦标准根。正常 AX/指针路径不添加通用 fallback。根窗口 SCStream 排除 child windows，不能声称其像素包含辅助菜单；菜单使用明确的 AX 结构证据模式，原窗口捕获健康、隐私与几何门仍适用。

ui.select_by_label 先用当前 opener 与 Source 锚发一次 ShowMenu，再用实际返回时间之后、同一 watch 的新关系绑定唯一、启用的一级标签，并取得新许可发一次 AXPress。两个输入在同一 activated Attempt 内计数，原期限不重置；选择前 Source session、锚或作用域改变会拒绝。Core/helper 要求已结算展开审计、同 attempt、不可延长的 ceiling，原子 gate 只包围一次调用。重复意图、新许可重发、未知后重开均拒绝。

## 明确完成后的结果阶段

最终 AXPress KnownCompleted 且 dispatch 已清空、mechanical_unknown=false，才切换只读结果阶段。允许在原预算和期限内正常重观测与重新绑定；旧锚退休仍撤销旧 binding/permit，但不否定新的 CurrentUi 证据。新 Source 必须完整、正常、满足隐私与捕获门；新锚证明同 kernel process、原授权 epoch/PID/window ID/精确根框，当前语义 role/subrole/group/label/ancestor/sibling 不变，当前 fingerprint、框和 provenance 与实际对象完全匹配。旧 anchor ID/revision/source session/current instance 不承担这个只读阶段的输入权限。

Reaction 和成功结束时的 Engine 都独立复核固定 scope，Verification 再从新正常绑定实际读取 expected value，记录 menu.selected_value@1 / CurrentUi / Causality::Unknown。根目标保持冻结的 UiRevisit，必须离开、返回并取得不同证据，不能用菜单消失、AX 返回或回调计数完成根。

Unknown Press 即使随后读到目标值，也保持 EffectUnknown 和 Core 隔离，不返回 Succeeded、不推进 revisit、不再次选择。迟到 Completed notice 不能覆盖消费过的 Unknown。12 项 Reaction 与 11 项真实 asyncCore/模拟 Source 流程回归覆盖已知结果重观测、六项 scope 变化拒绝、Source gap、输入前 watch 替换拒绝和未知不推进；这些不是实际硬件证明。

## 捕获恢复与独立窗口证明

不一致图片先失效。完整、无敏感字段且原 AX/CG 标准窗口精确匹配时，同 watch 最多两次重新建立基线；只接受新边界之后的真实全尺寸 COMPLETE 像素。合法旧边界及明确半尺寸过渡在入队前过滤，真正普通队列缺口仍终止；零/缺失/未来时钟、异常尺寸和 blank/suspended/stopped 保持失败可见。

现场发现捕获刷新会销毁一个已读过的其他 AXWindow。诊断 CF 分类不提供例外；独立私有证明在同次完整活读预算中检查真实直接 AXApplication 父关系、同 PID/kernel stamp、唯一 AXWindows 成员、明确 CFBoolean 非 modal 和与所有受保护/菜单引用不相交。最终后验检查成功才保存最多 8 个、两秒有效的证明；销毁回调不查询已死对象，仅实际 CF/根/进程/会话匹配并消费一次。普通 registry/binding/permit 仍失效，下次菜单锚仍需直接复验。未知、歧义、过期及受保护路径事件照常退休。

私有 stderr 前缀最多 16 KiB，超限持续排空，写入 0700 自有目录内的 0600 nofollow 原子资产；不进入 Source health、公开日志或输入证明。现场诊断及失败记录保留，不能凭完整序列化描述相同猜测 CF 身份。

## 实际验收边界

[Operator 记录](../../artifacts/m3-native-operator-menu.json)含实际同意、固定 GoalSpec、单 attempt 两意图、消费映射/outbox、独立 CurrentUi 和后续 UiRevisit、原生 item states [0,1]、选择值及撤销不重放。AppKit AX 查询可触发 MenuWillOpen/DidClose 回调；所有生命周期计数保留，但它们不是 ShowMenu/选择输入计数。撤销检查使用实际 action/selection/popup/navigation 及原生选中状态。

[原生 11 场景矩阵](m3-native-menu-boundary-plan.md)验证实际替换、禁用、几何变化、同 PID 遮挡、过期、关闭及真实 delayed AXPress Unknown 不重放。duplicate-label 只覆盖测试绑定器；Source restart 在缺少当前证据处拒绝，未声称 canonical 生产 Core 选择验收。更多菜单故障、原子键盘与 Unicode 输入、TextEdit 保存重开、真实 S1/S2/Laya/MediaPipe、持久恢复、100+20 任务、环境/性能/停止及签名产品仍开放。

重跑自有任务：npm run smoke:macos-operator-menu -- --runtime <owned Electron>。当前使用登记参考规划器，真实模型调用为零。
