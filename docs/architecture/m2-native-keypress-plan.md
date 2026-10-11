# 原生原子 KeyPress 的一期实施方案

2026-10-03。本文是待实现的架构方案，不代表原生键盘已接线或实际 GUI 已验收。保留现有 `Primitive::KeyPress { key, modifiers }`；不新增 Hotkey 平行 API、别名、held 原语或隐式文本注入。

## 1. 目标与完整 MVP 边界

一期交付一个真实、可审计的原子键盘路径：冻结任务与计划中的明确语义操作，经新 Source 的焦点证据、唯一绑定、当前许可、原期限和 native 最终复核，提交一对 keyDown/keyUp；确认实际 release 后，仅由新的 Source 和独立 Verification 判定业务结果。停止、人工输入、租约失效、helper 死亡与 Unknown 不产生重放。

可以先交付版本化 `macos-atomic-special-key@1`，然后扩展布局相关字符键与明确 command shortcut。这是分阶段能力开放，不是缩减一期：TypeText、Unicode/IME 的明确支持边界、TextEdit 编辑/保存/重开、真实 S1/S2/Laya/MediaPipe、持久恢复、100+20 固定任务及环境/故障/性能/签名验收继续开放。少量按键或 fixture 的成功不得标记 M2/M3/MVP 完成。

## 2. 已有代码与具体缺口

| 位置 | 当前事实 | 必要变化 |
| --- | --- | --- |
| `crates/interaction-contracts/src/desktop/primitive.rs` | KeyPress 为 String + 四个 bool 修饰标志；held_token 为 None；validate 只检查非空 | 保留 API；定义唯一 canonical key 词汇、长度/控制字符界限及各 native profile 的支持表，未知名称拒绝 |
| `desktop/source.rs` / `native/macos/Accessibility.swift` | UiObject.focused 是 bool；AXFocused 缺失被投影为 false | 增加正向、带出处的 FocusContext；false 不是焦点身份或“无人聚焦”的证明 |
| `desktop/binding.rs` / `desktop/device.rs` | BindingProof 有 fp1/frame/menu_scope；purpose 只有 TargetInput、WindowActivation、MenuSelection | 增加互斥的 FocusedControl 用途及 focus_scope；必须贯通意图和当前 ExecutionEvidence |
| `reaction/skills.rs` | EnterText 看 focused bool 后通常走 AXSetValue | 不把现有 AX 文本技能静默改成按键；键盘语义操作须明确编译、能力与效果等级 |
| `device-macos/backend/native.rs` / `guard/authority.rs` | KeyPress 未在生产 native supported/action 映射开放 | 严格用途和 profile 检查；private key_press 请求传完整原/current proof |
| `native/device/HitTest.swift` | 点命中及 actual focused app/window；文本 SetValue 使用 AXFocused bool | 新建键盘焦点验证器，实际 AXFocusedUIElement 与目标 CF 关系；不能以中心点代替键盘接收者 |
| `GuardState.swift` / `DeviceMain.swift` | 启动/current mouse buttons gate；own pointer type+timestamp ack；tap 已监听键盘及 flagsChanged | 独立键盘 delivery 状态、系统 key/modifier clear、keyUp ack、关闭期间有界 drain |
| `AtomicClick.swift` | 同 commit gate 连续 down/up，150ms release ack | 借用顺序提交原则；不要复制其 post 后开始计时或 cutoff 后再查一次的边界模式 |

普通原生输入、菜单互斥、固定授权窗口与 AtomicOnly 均保持。现有原子鼠标死亡结果不能证明键盘 down 后 helper 死亡必定释放。

## 3. Source-owned FocusContext

建议 `StructureSnapshot.focus_context: Option<FocusContext>`；仅在完整、隐私合格的正常窗口读中产生，不在 menu_context 期间发布键盘可用证据。字段至少为：

```text
FocusContext {
  process: { pid, start_seconds, start_micros },
  origin_window_id, origin_window_frame,
  target_instance, target_fingerprint, target_frame,
  focused_instance,
  relation: exact_target,       // 首个 profile 只支持实际 CFEqual
  verified_source_sequence, verified_observed_ms
}
```

`focused_instance` 来自这次真实 AXFocusedUIElement 的投影登记，不是根据某个 object.focused=true 反推。若实际 AX 焦点对象不能在完整树中唯一对应，或与受保护控件不是实际 CFEqual，首个 exact_target profile 不登记 FocusContext。AppKit field editor 可能与语义文本控件不同；这条关系需要实际探针和明确 typed 关系证据才能扩展，不能用标签、位置、最近祖先、同 PID 或 AXFocused=true 自动替代 CF 身份。

Source 逐读校验系统 focused application、应用 focused window、目标实际父链到 AXStandardWindow、CG root/PID/frame 与 kernel ProcessStamp；读前/后焦点、进程、窗口及隐私一致。超时、unsupported、未知 secure subrole、菜单/modal/辅助窗口或不完整祖先链均无正证据。所有新增 RPC 使用原遍历预算并逐调用设置实际元素 timeout；Source 的保守 observed_ms 仍对应完整读开始，不能因新增焦点查询把旧读 freshen。

FocusContext 不需要跨读保留 CF anchor，也不扩窗口许可；只描述当前 Source 正事实。实际 CF AX 引用留在 Source worker，本字段只携值。焦点变化应使 focus dependency 递增并撤销冲突 permits；仅 active_layer 不变不足以保持许可。

## 4. 必要 public / private 接口及责任

`ObservationPurpose::FocusedControl` 是正常窗口证据域的新用途，要求正常 StateCore.usable（含当前健康、完整像素/隐私/几何门禁），不借菜单 AX-only profile。只能携 `KeyPress`，不能据此授权 Click、AXPress、TypeText 或 ActivateWindow。

`BindingProof.focus_scope: Option<FocusedControlScope>` 携带当前 Source FocusContext。它与 menu_scope 互斥；FocusedControl purpose 必须有 focus_scope，普通/menu/window 用途必须无。完整证明的 original/current same_instance 除 fp1/精确 frame 外还核对同 kernel process、origin root、焦点目标与关系；current 的 snapshot、env/layer revision、source seq/time 均来自当前 evidence。旧 intent/proof 不得被当前同标签对象替换。

`FocusedControlScope` 还冻结可信 key profile，以及已解析的 key 语义类别；profile 属 trusted capability/launcher 配置，模型不能自选扩大。布局相关版本需附 typed KeyboardContext（见第 5 节）；首期布局无关键不能把缺失布局解释为已知 US。

跨模块所有权：

- Source：真实焦点、进程/root、隐私及输入环境的观测；无输入、无许可签发。
- BindingReducer：唯一语义绑定、fp1 与当前 FocusContext 对应；只增加明确 focused-control 构造路径，普通 bind/revalidate 不推导键盘授权。
- DeviceView.bound_observation / canonical ExecutionGate：统一组装 original/current focus_scope、purpose 与依赖，TaskRuntime 的预算/原 deadline 不变。
- ExecutionCore：immutable intent/permit、去重、提交 journal、未知隔离；KeyPress 仍是一个 atomic step、offset=0，不拆成 public KeyDown/KeyUp。
- Rust NativeAxBackend / NativeProbeRequest：profile 与原语匹配，private `key_press` action 携 typed key/modifiers/focus proof；不允许字符串作为 AX action 或任意脚本。
- Swift Authority / NativeExecutionScope：严格字段与互斥关系、固定 PID/bundle/window/ProcessStamp、期限、双租约、stop/revoke；加入 action 不意味着任意 key 已支持。
- Swift 新 `FocusedControl.swift` / `AtomicKey.swift`：worker 栈上的 AX 验证与实际事件构造/提交/送达确认。不把 AX handle 送入锁、IPC 或 Core。
- Reaction / Engine / Verification：解释业务语义、效果等级、预算和结果，不让设备根据 key 猜业务目标。

同步 schema/TS、strict private request parser、构造 fixtures 和 immutable proof 哈希；不加旧 alias 或跳过 canonical evidence builder 的诊断提交捷径。

## 5. 键名、TIS 布局、IME 与 command 的开放顺序

### 5.1 special-key 首期

定义一套精确 canonical 名称和固定硬件键码表，例如 Return、Tab、Escape、Backspace、DeleteForward、ArrowLeft/Right/Up/Down、Home、End、PageUp/Down。这里只是候选词汇，实际 profile 的开放集合由 native 探针和语义效果验收决定；没有验证的键/组合保持 unsupported。拒绝 modifier 键自身、lock/Fn/system/media 键、空白别名、数字 keycode 输入和大小写宽松猜测。

首个可实施 profile 可以只开放自有焦点控件的一个布局无关特殊键和少量已验证组合；Return 可能提交、Escape 可能取消、Tab 会转移焦点，均不能因为“是特殊键”自动归为无副作用。每个操作需要冻结计划中的控件作用域、明确目标效果与 EffectClass。没有键盘语义/效果槽位的现有技能不能仅为演示生成任意 KeyPress。

修饰 flags 由现有四 bool 精确生成，组合来自 profile 的显式表；未知或未验证组合拒绝，不单独发送 modifier down/up。执行端不能把 key+flags 重新映射为别的 shortcut。

### 5.2 布局相关字符键

必须观察实际 TIS 当前输入源与 keyboard layout 数据、输入源种类/键盘类型，并在解析及最终提交前复核不变。使用 layout 数据实际转换与明确 dead-key 状态；不把 ASCII 字符直接映射到 US keycode，不忽略 shift/option/布局变化。

若 TIS 只提供布局但当前输入源是 IME，不能据此声称 IME 不参与。IME marked/composition 状态不能读或不能唯一归属实际焦点控件时，依赖 composition-free 的 profile 拒绝。Source worker 的 SDK/actual probe 必须先明确：标准 AX/TIS 是否能支持这个事实；若不能，公有 KeyboardContext 使用 unknown，能力保持不可用，另设计可验证的 registered-editor 支持，不猜“没有 composition”。

字符 KeyPress 和 `TypeText` 是不同能力；Unicode injection、逐键布局输入、AXSetValue、剪贴板均不得互相静默替代。TypeText 的完整 UTF-8/Unicode/IME/分段/预算和效果路径仍另行完成。

Source worker 已核对本机 SDK：`TISCopyCurrentKeyboardInputSource` 与 `TISCopyCurrentKeyboardLayoutInputSource` 语义不同，IME 可有底层布局；可有界比较实际 input-source ID/type/category/modeID、ASCII-capable 与 layout 数据摘要，后者只是输入环境连续性，不证明 composition。generic AX attribute 常量没有通用 marked-range；公开 Web AX 的 `kAXTextInputMarkedTextMarkerRangeAttribute` 使用 AXTextMarkerRange，不能假定 AppKit 支持。`NSTextInputClient.hasMarkedText/markedRange` 是进程内契约，不能用 helper 的 current NSTextInputContext 代替目标应用。

因此首个 exact-CF fixture profile 可以选择有明确登记键盘 handler 的布局无关控件操作，不声称通用文本/IME 支持；composition 状态如未知必须原样记录。若业务/profile 依赖“无 marked text”，unknown 就拒绝，而不是为让 fixture 通过补造 none。一般文本控件及 TextEdit 的开放等待实际 route/composition/效果证据。

### 5.3 command shortcut

command/control/option 组合仍通过同一个 KeyPress API，但必须明确登记 exact shortcut、应用/window/control 作用域、预期结果和效果等级。不能开放任意 Command+字符，也不能把系统全局 shortcut、应用切换、退出、关闭、保存、外发等操作归为普通焦点控件编辑。

TextEdit 保存/重开需要真正的应用/文件结果、对话框和固定窗口授权设计及独立验证；`Command+S` keyUp confirmed 不是已保存文件的证明。系统 stop-hotkey 保留人为停止路径，不作为可提交 shortcut。

## 6. Native 最终焦点证明

从 system AX focused application 取得实际应用，核对 PID/bundle/kernel stamp；从应用读取 focused AXWindow，要求实际 AXStandardWindow/root、CG ID/frame 唯一匹配原固定窗口；读取实际 focused UI element，要求 exact_target CFEqual 实际绑定对象。验证完整 fp1（role/subrole/group/label/ancestor/sibling/frame）、enabled/in_view/nonsecure、actual AXWindow/父链与当前源 focus_scope。

与 source 投影 registry token 不直接等同：native 重新取得并核对实际对象。不得使用通用 AXTopLevel fallback，不得在按键前自动点击、设置 AXFocused 或重激活窗口来“修好”焦点；若需要聚焦，须前一步独立授权、KnownCompleted、新 Source 正证据后再发新许可。

所有 AX/TIS 采证在原 ticket deadline 内有界完成；最终再次读取 focused app/window/element，actual CFEqual 且进程/窗口/布局不变才进入 ordered commit。Source/notified focus revision、human/stop/revoke 的变化由同一 commit gate 排序。查询与 gate 之间仍有系统事件竞争，不能声称锁住全系统焦点；current dependency、tap human-stop 和最终查询共同提供可审计的边界，结果变化由独立验证处理。

## 7. 原子事件、系统状态与确认

预先构造 private CGEventSource、唯一 marker、唯一单调 stamp、精确 keycode 与 flags；keyboard autorepeat=false。down/up 均设置相同 keycode、stamp、marker、修饰 flags，无 modifier down/up，零显式延迟，不设硬件事件 suppression，不修改用户键盘布局。

启动和每次提交前检查 combinedSessionState/hidSystemState 的所有实际支持 keycodes，以及系统 momentary modifier flags、mouse buttons clear；失败关闭键盘 capability，不为了清理用户按键发送任何释放。CapsLock 等锁定状态是单独输入环境事实，不能与 momentary held modifier 混为一谈；字符/组合 profile 需明确处理，未知不支持。

一个 original commit gate 包住 down 和 up；down 一旦开始，即使其间 stop/expiry，也必须尝试立即完成已经承诺的 up，不得以二次 gate 拆开原子对。第一 post 是输入已可能到达的边界，permit/intent 从此不可重用。构造失败/最终验证失败且尚未 post => definitely not submitted；任何开始后的异常 => submitted Unknown。

tap callback 只在 marker、event timestamp、type=keyUp、keycode、flags 全部匹配时登记 owned release；独立于“own marker 不算 human”的分类规则。普通 owned flagsChanged、错误 keycode/stamp/down 类型均不能确认释放。回调不输出字符、keycode、AX 内容或 modifier payload。

confirmation deadline = min(original ticket deadline, actual first-post monotonic time + 150ms)，溢出拒绝；确认完成布尔值只能由 cutoff 前获得的 tap ack + 当前 system key/mod clear 共同建立。循环退出后不得再读一次 tap/system state 将晚到 release 升格 Known。继续 drain 用于安全关闭，也不能改变原 Unknown receipt。

Applied/KnownCompleted 表示这一对 post 调用返回且 matching release 在界限内经过 session tap、system key/mod clear；不表示 app 已消费或业务成功。returned_ms/input_end_ms 必须来自最后实际确认时刻的共享时钟；时钟失效、超 deadline、tap lost、system仍按住、ACK缺失全部 Unknown，不重发同键。

## 8. Stop、死亡和 Unknown

closing 立即禁止新的 down，保留 event tap 有界等待已经提交的 up；键盘 delivery 有独立 drain 状态，不覆盖鼠标状态。lease/revoke/human/stop/tap权限丢失不通过重租约恢复旧 permit。helper/控制管道/host/supervisor 失效保持 Core isolation 和原 receipt，禁止自动 helper 重启后重放。

关键实施门槛：自有实际死亡矩阵必须验证 before-down、between-down-up、after-up、等待 ACK 时 device/host/helper 死亡的系统 key/mod 最终状态。KeyPress 在 Core held ledger 中没有 token，现有 `release_owned_inputs` 不能凭该 ledger 猜测释放半个按键。不能假定 private CGEventSource 或 SIGKILL 会释放已提交 keyDown。

如果真实 between-down-up helper 死亡可留下 key held，必须先完成明确的 owned atomic release 生命周期/独立可靠收尾设计与实际验收，或保持键盘 capability unavailable；不能以“临界区很短”绕过，也不能启用 held policy、释放所有系统键或清理用户输入。安全收尾只能释放可证明本次拥有的那一半，不成为业务重试。

Unknown 后可记录新的只读业务观察，但不能把观察到的 expected value 改写机械 receipt 或自动推进后续输入；需明确 reconciliation 状态及授权边界。部分成功、迟到 Completed、改新 permit/intent/attempt、重新聚焦均不能解开 sticky isolation 或重放。

## 9. Engine、语义操作和业务验证

在已确认计划中增加/复用明确键盘语义槽位，仅编译一个现有 KeyPress；不可新建 Hotkey API。special profile 先选有真实可观测结果的自有焦点控件操作，目标仍来自冻结语义描述，按 key 的实际操作提升 EffectClass 并经 gate 接纳。

每次输入前必须获得新 complete Source + focused-control binding；聚焦动作若已知完成，必须在原 attempt/action/observation预算内计费，动作配额不得仅为键盘无限扩大。原 TaskRuntime/root/attempt deadline、原 decision/outbox/permit、seq/floor 贯通，reobserve 不重置。

KnownCompleted 后恢复普通只读结果验证：fresh normal Source、current semantic binding、实际值/选择/页面效果及独立 predicate，observed_ms 严格晚于实际 input_end、source_sequence 递增。如按键转移焦点，不再要求旧控件仍 focused，但验证目标/授权 window/process 仍严格；新业务输入需自己的 fresh FocusContext 与新许可。

未知操作保持未知。一个 keypress receipt 或 fixture 计数不能产生 Satisfied；CurrentUi 效果先持久化，冻结根若要求 UiRevisit/Persisted 必须完成更强、独立的返回/文件证据，禁止降级 FrozenGoalSpec。

## 10. 最小执行顺序与验收资产

1. Source worker 先完成只读 SDK/自有实际关系探针：focusedUIElement 与控件/field editor、TIS/IME可观测边界，明确 supported/unknown；无输入的探针不计输入验收。
2. contract/Source/Engine 统一 FocusContext、focus_scope、FocusedControl、互斥校验和依赖失效；保持 normal/menu/window 隔离。
3. Device worker 接 private key_press、actual FocusedControl 验证、AtomicKey、系统状态和严格 cutoff/关闭；Core ledger 仍只看到一个 KeyPress。
4. pure/合同测试验证 immutable original/current、wrong focus/window/process/kernel/geometry/role/label、Source stale/partial/secure、wrong ACK marker/type/key/flags/stamp、cutoff/overflow、startup/current held、Unknown不可转成功与不重放。
5. 自有物理 fixture 记录实际 keyDown/keyUp/action效果和系统 clear、原/current Source/proof、receipt/journal、构建/源码/schema哈希。物理 stop/revoke/lease/tap/死亡矩阵先过，再开放对应 profile。
6. Operator 走 Cancel→观察自有焦点→Allow→Execute，生产 Host/Engine/Core 只发计划中的一对，独立 fresh Source 与 frozen root 验证；驱动不得补按键、重聚焦或手动修值。撤销后无 replay。
7. 逐步扩大特殊键/flags/布局与command支持，完成 TypeText、TextEdit和完整固定任务/环境矩阵；所有报告明确 direct-helper、Core、Host/Operator及实际 MVP 各自边界。

父任务在 Source/fixture 实际可行性证据后决定首个键与关系 profile。本文只拥有设计文件，不授权或声称已经修改/开放生产键盘输入。
