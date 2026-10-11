# 原生菜单展开与按标签选择：最小完整接口方案

状态：接口与运行时已实现，实际完整任务正在验收；仅覆盖登记应用、固定授权标准窗口中的一级控件菜单。普通 AX/指针权限、根任务冻结谓词、原 attempt 期限与预算保持。本文保留实施前设计，当前实现与现场记录见[菜单选择接线](m3-native-menu-selection.md)。完整 MVP 尚未验收。

实施后的接口差异：Source 使用已有受信 `registered_editor` 的精确 PID/window text policy 启用 watch 菜单 profile，没有新增独立 `NativeRequest.origin_window_id`。`MenuBindingScope::Opener` 及 `MenuOpeningRef` 均携带不可延长的 `attempt_deadline_ms`；`BindingReducer::bind_in_menu` 使用当前完整上下文。稳定菜单身份比较允许普通 registry 的实例字段随新读更新，仍严格检查当前项目实例、实际 Source 锚与完整受保护描述。非根菜单步骤使用自身已登记的类型化选择值；根控件选择仍与冻结目标一致。

## 1. 已取得的实际事实

[关系 probe](../../artifacts/m3-native-menu-relationship-probe.json)经过一次生产 ShowMenu，回执 Completed、submitted_count=1，夹具 action_calls=1、selection_calls=0。两个实际项目具有 AXPress；中心点系统命中同 PID 且与实际项目 CFEqual。项目 AXWindow 返回 -25212/noValue，AXTopLevelUIElement 与原聚焦 AXStandardWindow CFEqual，父链为：

```text
AXMenuItem -> AXMenu -> AXMenuButton -> AXGroup -> AXWindow -> AXApplication
```

实际菜单绘制窗口属于同 PID、layer 101，ID 不同于原标准窗口。Source 已遍历上述真实子树，对象 window_id 为原标准窗口，但因 transient menu 设置 complete=false/active_layer=None。probe 的 diagnostic ID 仅在该诊断进程有效，不是绑定或输入权限。probe 同时发现应用菜单栏，故同 PID 和 TopLevel 属性都不足以授权选择。

## 2. 当前传递链与必须处理的阻碍

| 位置 | 当前行为 | 本次明确修改方向 |
| --- | --- | --- |
| native/macos/Accessibility.swift | AXMenu 使全读不完整；普通 viewport 为原窗口矩形 | 对可证明归属的一级菜单构建 typed context；项目可见性使用经过验证的菜单 viewport |
| native/macos/AXSession.swift | 菜单拓扑/通知变化或 observer 不完整重建普通 identity registry，并释放普通 observer | 增加独立 Source 菜单锚表，普通 registry 重建不自动移植或伪造其实际 CF 身份 |
| crates/source-macos/src/shell.rs | RefreshNow 销毁 NativeWatch 并启动新 helper | 菜单跨输入观察等待同一 watch 的周期新读，不发送会销毁锚的 RefreshNow |
| crates/interaction-engine/src/environment/state_core.rs | usable() 要求 complete、active_layer、目标窗口帧匹配与像素期限 | 增加用途明确的 menu_input_evidence；不能让正常输入使用菜单证据 |
| crates/interaction-engine/src/execution/mod.rs | bound_observation 按 current instance 构造 BindingProof 并复制到 intent | 在这里建立菜单关系证明，不能仅在 ActionPlan/private helper 上附加未经过 Core 的字段 |
| crates/device-macos/src/backend/native.rs | 固定 window 与 binding.fp1.window_id 相等；仅 TargetInput/WindowActivation | 固定 window 仍为原窗口；新增 MenuSelection 用途，限定 Press+菜单关系证明 |
| native/device/HitTest.swift、BindingProof.swift | 普通路径必须读取 AXWindow 并终止于标准窗口 | 菜单专用关系路径，不提供通用 TopLevel fallback |
| native/device/DeviceMain.swift | 一个长驻 AX worker、一个请求槽；AX引用不进 GuardState | 新请求仍用该 worker；查询和局部 AX 引用留在同一 worker 栈，gate 只包围一次 action |

当前代码没有名为 ExecutionEvidence 的共享对象；证据决策分散于 StateCore.usable、attempt_evidence_usable、start_dispatch 和 DeviceView.bound_observation。实现需统一这些检查入口的用途参数，否则菜单证据会在某个旧检查处被拒绝，或绕开旧检查。

## 3. Source 契约：数据、观察归属、输入互斥

StructureSnapshot 增加两个明确字段，native Structure/NativeStructure/watch tracker 同步传递；不以侧通道日志提供权限：

```rust
menu_anchors: Vec<MenuOpenerAnchor>, // 最多 8 个，Source 自己从固定根窗口发现
menu_context: Option<MenuContext>, // 最多一个当前已证明的一级菜单
```

MenuOpenerAnchor 建议字段：

```rust
anchor_id: String,          // Source 随机会话内 ID，非模型参数
source_session_id: String, // watch/helper 生命周期，跨重启绝不复用
anchor_revision: u64,      // 单调，失效后新锚不同；无静默 resurrect
process: MenuProcessIdentity, // pid + kernel start seconds/micros，producer/native均实查
origin_window_id: String,
origin_window_frame: Rect,
opener_instance: ObjectRef,// 当前普通 registry 的实例，只用于该次快照查找
opener_fingerprint: InstanceFingerprint,
opener_frame: Rect,
verified_source_sequence: u64,
verified_observed_ms: u64,
continuity: MenuAnchorContinuity, // direct_read / observed_lifetime
```

MenuContext 建议字段：

```rust
context_id: String,        // 一次实际菜单出现的 Source ID，关闭/重开必须改变
source_session_id: String,
opener: MenuOpenerAnchor,
menu_instance: ObjectRef,  // 当前快照实际菜单实例
menu_fingerprint: InstanceFingerprint,
menu_frame: Rect,
drawing_window: MenuDrawingWindow,
```

MenuDrawingWindow 包含真实 CG id、owner_pid、layer、frame；它是当前可见性关联，不能替换 origin_window_id 或变成授权窗口。第一次范围只登记已验收的绘制 profile（本次 owned 菜单为 layer 101）；不直接允许任意高 layer 窗口。必须唯一匹配实际菜单 frame/可见项目，匹配前后复核。同进程的新 window 只是候选，绝不是菜单归属证明。

Rust Source shell 目前重写 snapshot.source_sequence，保持重启前后全局单调。必须在同一个发布点同步规范化 anchor.verified_source_sequence/time，不能将helper局部sequence与Host全局sequence混用。WatchTracker先校验原生stream/session与原始record，再由Source发布整个快照；Host不得自行修补过时嵌套provenance。source_session_id稳定对应同一实际AX watch会话，重启必换，即使外层source_sequence继续递增。

MenuContext 不复制 snapshot 的 sequence/time；其所有字段均属于携带它的完整 StructureSnapshot，opener.verified_* 必须等于该快照 provenance。fp1描述由当前Source实际树的相同规范规则派生，所有序列化描述必须可在携带快照的对象中重算并验证。关系完整性由真实对象 parent 链和逐级 CF 关系产生，不接收调用方自称的祖先列表。

快照窗口仍保留原标准窗口，active_layer 保持该实际聚焦标准窗口。menu_context=Some 表示有效输入域是菜单，不能表示背景普通控件也可输入。普通输入判定必须要求 menu_context=None；菜单输入要求 Some 且 context/anchor/项目精确匹配。窗口激活不在已展示菜单中运行。未归属菜单、多个菜单、submenu、应用菜单栏、sheet/dialog、缺失/矛盾关系均清空活动输入资格且 complete=false，不发布宽松菜单 context。

complete 继续表示结构读和隐私覆盖完整，不得为了菜单把超时、截断、secure/未知字段或进程变化变成完整。受证明菜单只能移除原有的笼统 transient-menu 理由。源对象的 window_id 表达实际 AX 祖先所属的原窗口，不凭 CG 辅助绘制窗口重写 fp1。菜单对象/项目用真实菜单 viewport 判断 in_view，不假定菜单必须完全在原窗口矩形内。

初次范围通过 trusted NativeRequest 的固定 origin_window_id 启用菜单 profile，与 NativeWindowScope/guard launcher 的值一致。Source 在同一 watch 的完整正常读取时，主动为最多 8 个已登记 AXMenuButton/AXPopUpButton 建立只读锚。这样无需把动态 AX handle 经 Host 再送回 Source，也无需当前已关闭的 watch stdin 增加控制通道。helper 启动先核实实际固定窗口；旧 observe 单次进程只作诊断，不支持跨输入锚。

## 4. 独立实际 AX 锚的生命周期

AXSessionWorker 拥有独立有界 MenuAnchorRegistry；CF AX 引用、root/opener/path 引用与可选 anchor observer 只在该 Source AX worker 使用。typed 值可离开 worker，CF引用不可离开。表容量 8，不能通过 LRU 覆盖仍被引用的活锚；达到容量拒绝新增并给出来源原因。

每个锚保留真实 opener CF 引用、实际原 root CF 引用、根/触发控件的受保护 frame/role/parent 路径及 ProcessStamp。每次读取必须：

1. 当前进程启动 stamp、bundle、PID、窗口 id/frame、聚焦窗口与原锚一致。
2. 对保留引用执行有界查询，仍有效且同 PID；从新遍历独立找到 opener，必须 CFEqual，不能仅比较地址、label、digest 或旧普通 instance_ref。
3. 受保护 role/subrole/frame/label/group/祖先路径未变；逐级同 PID、CF parent/child 成员关系、无循环、深度有界，终止于实际原 root。
4. 菜单出现时，保留实际 menu CF 引用，并以 CFEqual 检查它确实是 opener 当前孩子；项目当前属于这个菜单。context_id 由此生成，关闭后不得复用。
5. 原普通 registry 即使更换 generation，锚仍只有在以上直接检查全部通过时有效；输出的 opener_instance 改为该读实际普通实例，anchor_id 不靠实例字符串续命。

AXUIElementDestroyed 在实际 owned 夹具经常返回 -25207/notImplemented。不能把完整 destroy observer coverage 设为运行硬门槛，否则已有真实菜单永远不能运行。缺少此通知时 continuity=direct_read：只声明两个已完成直接查询之间的实际引用匹配，下一动作必须基于当前读及设备再验证，不宣称全域 lifetime coverage。保留原 CF 引用避免通过对象地址/字符串伪造连续性。

已收到的明确 destroy、process/window/frame/parent 改变、查询 invalidUIElement/noValue/cannotComplete、CFEqual 不同、helper 重启/关闭、权限丢失、无序/丢失/无法归因的锚事件均终止锚。unsupported observer coverage 本身不是“已发生毁坏”；不得把它误当成功覆盖，也不得将其与明确收到的毁坏/歧义通知混为一类。

普通布局变化引发的 registry.invalidate 允许本次锚直接重新检查，但不能转移旧锚到另一 CF 对象。若已有 anchor observer，其生命周期独立于普通 disposeObserver；若 callback 的 element 无法确定属于哪条锚，保守失效所有菜单锚。无 observer 时全部通过逐读直接检查，没有通知歧义的隐式豁免。菜单关闭清除 context，终止 menu 引用；opener 可继续作为重新观察的只读对象，但下次展开有新的 context_id。

## 5. BindingProof 与 public Device 消息

fp1 的规范化、字段和摘要格式保持；菜单 item 的 ancestor_roles 包含真实 MenuButton/Menu 路径，window_id 为原标准窗口。BindingProof 增加：

```rust
menu_scope: Option<MenuBindingScope>,

enum MenuBindingScope {
    Opener { anchor: MenuOpenerAnchor },
    Item { context: MenuContext, opened_by: MenuOpeningRef },
}
struct MenuOpeningRef { attempt_id: AttemptId, intent_id: String }
```

MenuBindingScope 不递归嵌套 BindingProof。Source 拥有 anchor/context 字段；Reaction/Execution 根据已知 Completed 的展开回执生成 opened_by。模型不能生成这些字段。opener 的当前 instance/fp1/frame 必须对应快照对象；item 必须有真实菜单 parent 链，fp1/frame 与当前对象一致。

BindingProof.validate 验证字段界限和内部关系。same_instance 还必须比较菜单 scope 的身份及完整受保护描述：source_session、anchor_id/revision、process stamp、context_id、menu_instance、root/opener/menu/drawing 几何和 fp1，以及 opened_by；只允许 verified sequence/time 及既有 snapshot provenance 单调前进。菜单开关或实例改变不能续期旧 permit。每个输入单独绑定和取得新 permit。跨展开/菜单两个阶段关联用anchor_id/revision+session+受保护描述，不能要求两个不同读的opener_instance字符串恒等；opener_instance仅在当前快照内核实，跨读连续性来自Source实际保留CF锚。

ObservationPurpose 新增 MenuSelection。TargetInput+Opener 仅允许一次 ShowMenu；TargetInput 且 Source 有 menu_context 时不得产生普通输入；MenuSelection+Item 仅允许单步、offset=0 的 AxAction::Press。MenuSelection 不能产生 Click、PointerPosition、Scroll、SetValue、ActivateWindow 或重复 ShowMenu。Core 必须显式匹配各 purpose 与 primitive，不沿用目前仅测试是否 WindowActivation 的二选一逻辑。Observation/ActionIntent 的菜单证明由原有完整消息传到 Core，original/current proof 的菜单身份检查必须一致。

BindingReducer 增加 bind_menu_item(env, target, context, now,max_age)，严格在当前 menu_instance 的实际后代中匹配经过规范化的完整 label，检查启用、可见、非 secure、声明 Press。重复标签返回 TargetAmbiguous；不使用 contains/margin 自动猜中，也不匹配同窗口之外的同名项目。失效策略包含 context_id/anchor 变化。

DeviceView.bound_observation（或后续合并的ExecutionGate.authorize）按 binding 的明确用途创建 menu_scope，intent 复制完整 proof；只有一个canonical证据构造入口，普通 binding 构造路径仍保持。不得只修改 private NativeProbe 而绕过 immutable issue evidence。

## 6. StateCore / Engine 的明确证据模式

新增统一用途 enum：NormalTarget / WindowActivation / MenuSelection。StateCore 的 normal usable 必须排除 menu_context；menu_input_evidence 返回一个 typed MenuExecutionEvidence，并由 attempt_evidence_usable、start_dispatch、DeviceView.bound_observation 同步消费。

MenuExecutionEvidence 至少包含：当前 EnvSnapshot/菜单 context、原固定 root 元数据、健康同一 Source/watch 会话、真实 AX 读的 source sequence/time、菜单锚当前直接连续性、绑定与 deadline。必须来自 action 的实际 input_end_ms 之后且 source_sequence 严格递增。Source session 改变或锚失效后不能通过 fresh stream 自动找同标签继续原菜单事务。

SCStream 当前明确排除 child windows，故原窗口像素不能证明 layer101 菜单内容，也不能将 IDLE 当新的菜单图像。首版菜单 AXPress 是显式的 AX 结构执行模式：要求原固定窗口元数据/隐私与健康原 capture stream callback，以及完整新 AX context；菜单内容由当前 AX 关系、CG可见绘制范围与设备最终系统命中证明，不伪称新截图包含菜单。normal frame deadline/refresh 门禁保持；仅 MenuSelection 使用这个版本化证据模式。若产品验收要求菜单像素证据，则必须另实现经关系验证的单辅助窗口 capture，不能在本方案中声称现有原窗口 stream 已做到。

Source 的 menu结构和生命周期变化单独驱动 menu dependency revision，进入/退出菜单撤销相冲突的普通/menu permits。frame/AX/security health 仍独立有效；菜单 profile 不能掩盖 root 来源丢失或未知敏感内容。当前 require_frames 的 generic evidence invalidation 分支需认得这个显式模式，否则只读菜单新证据会误取消已知展开后的 attempt。

首版 native菜单 AwaitEvidence 必须 await_source(..., refresh=false)，从同一个 watch 周期取新读；本地预算仍计数，等待不重置期限。如果 watch不提供下一完整合格读，在原 observation/attempt期限结束为 unresolved/unverified；不得调用 RefreshNow 后按相同标签重建锚。选择已结算进入结果验证后可结束菜单 context，再使用普通刷新获得结果/返回页面证据。

## 7. private ax_menu_press 与 native worker

NativeProbe 继续携带原/current BindingProof（Swift NativeSourceProof 同步 menu_scope），并明确传递 attempt_id。action=ax_menu_press 仅由 MenuSelection+AxAction::Press 构造；原 AXPress action 名仍为 ax_press。strictProbeFields/serde deny_unknown_fields 同步完整枚举与有界嵌套字段。

长驻 device helper 为每次已知成功的带 Opener scope ShowMenu 保存一个有界 value-only 展开审计记录：task/root_epoch/attempt/intent、Source anchor 身份、原窗口与 opener 描述、实际 returned_ms、stop revision。菜单请求 opened_by 必须命中同任务/同 attempt 的记录，且 Source menu观察在 returned_ms 之后。此记录不是许可，不能更新期限或绕过当前新 permit；普通展开 permit 在步骤结束后被撤销不删除历史事实，也不授予任何后续输入。Stop、lease loss、process/window变更、helper死亡结束该事务；同菜单选择首次进入commit即消费，失败/超时Unknown均不能再选。

Device helper 不保存跨请求 AX handle；展开记录只含值。菜单请求由同一个长驻 worker 独立取得实际 item/menu/opener/root AX引用，Source anchor对跨读 opener 连续性负责，device 对本次实际父链/描述/最终命中独立复核。普通日志或诊断ID不能变成新的 native AX token。

专用检查顺序：

1. 单一级 item、source menu proof、原 fixed scope/purpose/dependencies 与展开审计记录合法。
2. 通过item中心系统命中，先检查PID再读任何content；目标role必须 AXMenuItem。
3. 沿真实父链、逐级PID与父子成员关系查到唯一 AXMenu、预期 opener、原 AXStandardWindow；不接受 MenuBar/Application 为窗口替代。
4. item AXWindow 允许本profile实际验证的 noValue；若有值必须与原root一致。其他异常不能触发fallback。item AXTopLevelUIElement 必须与该链root、focusedWindow CFEqual，后者CG layer0/id/frame必须为原授权标准窗口。
5. 对 opener/menu/item 的当前 fp1、精确frame、menu drawing唯一关系、启用与声明Press复核；menu与opener的链每次实际CFEqual；最终重新hit同item并再读关系/几何。
6. 设置 <=25ms 且受原 ticket deadline限制的action timeout。查询都在gate外；gate仍检查当前permit、stop、两条lease、原deadline、fixedscope，只调用一次 AXUIElementPerformAction(item,AXPress)。
7. 非success、helper hard watchdog、回执缺失均Unknown；原pending slot/隔离机制阻止重放。probe/eligible仍input_authorized=false。新TopLevel逻辑仅位于ax_menu_press分支，普通BindingProof/HitTest不增加通用fallback。

NativeWindowScope 和 NativeExecutionScope.window 仍绑定原标准窗口；普通 root input 在菜单存在时原生最终检查也拒绝。不能仅靠Host路由排除它，更不能让CG绘制window成为第二个通用许可。

## 8. SelectByLabel 状态机与效果

添加 Skill::SelectByLabel / ui.select_by_label，Subgoal 增加 typed SelectByLabelSpec { choice_label: String }，非空、<=512bytes并拒绝控制字符。Subgoal.target继续为opener/root语义控件；label来自这个控件当前菜单，关闭后验证该控件的实际AXValue。expected 使用冻结GoalSpec.desired；首版为Text且与choice_label按登记规范化规则一致。不增加独立result目标或planner提供的临时menuitem目标。planner不提供anchor/instance/context/windowID、原生 action 或证明。AppPack登记具体菜单选项业务效果；Unknown/External不执行，破坏性标签推断只升高等级，不能借navigation展开绕过选择权限。

```text
Start -> 当前正常读绑定 opener + 对应 Source锚
      -> ShowMenu一次（新permit）
      -> AwaitMenu（实际returned_ms之后、同watch新sequence）
      -> 原锚关系验证 + bind_menu_item精确标签
      -> AXPress一次（新permit、MenuSelection）
      -> AwaitResult（选择实际input_end_ms之后新读）
      -> 菜单已消失 + 实际结果控件值等于冻结desired -> Succeeded
```

整个流程同一 activated attempt，最多2次input、最多3次预算化新观察，期限为原attempt/root的min，不在menu显示时重置。不支持submenu或键盘/点击fallback。Known ShowMenu无效果只再观察一次后结束；Unknown ShowMenu即使菜单可见也不选择，因为物理通道待隔离/协调，不把新证据变成第二次输入授权。Unknown Press只核对结果，不再次Press、ShowMenu或清理时点击。

menu_visible本身只能得到独立CurrentUi导航EffectResult；实际选择效果要用菜单后Source读回的值证明，不能只以菜单关闭、AX成功、selection callback计数或已绑定项目结束。Engine在run.done前独立复核选定结果；root的goal.required/ui_revisit/commit谓词不改写。源锚不是 root success predicate，菜单暂态不作为PageVisits的一次离开/返回。

## 9. 实施顺序、文件归属与验收

1. contracts source/binding/device/planning：上述typed数据、purpose/primitive组合校验、same_instance及schema/TS再生成。
2. native Source：Models/Accessibility/AXSession 的独立锚和context，StreamCapture/NativeStructure/watch tracker传递及固定scope。先验证无destroy通知时仍有直接CF连续性；不能以普通registrygeneration冒充。
3. Environment/Engine：新明确证据用途、绑定、dependency/context变化、DeviceView proof构造；验证native AwaitEvidence不重启watch。
4. native/device与Rustguard/backend/Core：专用请求、scope/审计/单次action、原期限、unknown隔离；普通AXWindow严格路径回归。
5. reaction skill/候选/plan安装/verification：同attempt有限流程与真实结果predicate，源菜单输入互斥。
6. owned HostMenuFixture、登记pack/reference planner：真实NSMenu或NSPopUpButton，Source可读的实际结果值与稳定语义标签；driver才读取独立计数，不把计数输入生产Engine。

必须覆盖：同PID其他窗口/菜单栏；外部遮挡；父链/childmembership不符；不同TopLevel；opener替换同label/frame；菜单关闭重开；辅助window歧义；disabled/重复标签；截断/过期/乱序；destroy不支持的直接读；明确destroy/查询失效；Source/helper重启；同中心几何变更；原root普通动作在菜单中拒绝；ShowMenu/PressUnknown；停止/lease/revoke；original期限/预算；项目Press无效果；选项值新读成功但根仍待revisit；完整root成功与撤销零重放。

保留实际probe的原始哈希和失败来源；完成后再运行生产Source/Device/Host的物理闭环。当前关系probe只能支持本设计的关系假设，不能替代菜单选择验收。
