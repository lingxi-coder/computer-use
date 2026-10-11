# scroll_to 运行时与来源位置

2026-10-03。滚动设备能力进入通用技能与真实 Engine/TaskRuntime/Reaction/ExecutionCore 接纳路径。当前设备原语、模拟任务回归与真实 Source 读数分别验收；尚未证明完整原生 Host 滚动任务或一期 MVP。当前 Schema SHA-256 为 `68d04f242bf6908d84db378e18059b5c0a64cc7e7c1a12925c5d4a0697c25005`。

## 两种身份与计划参数

Subgoal.target 是逻辑目的地，Subgoal.scroll 是类型化 ScrollToSpec：容器 TargetSpec、方向、1–120 像素和 1–16 次动作上限。容器必须声明 AXScrollArea，不能与目的地语义 ID 相同；参数只能属于 ui.scroll_to。计划安装要求目的地是冻结 GoalSpec 的目标或登记控件，容器进入已确认计划的作用域。

候选和输入绑定的是容器，技能成功依据目的地。容器不可见或不唯一时不会猜坐标。选择容器时只细化容器的当前分组，不改变根目标的分组。每个已知完成的滚动后重新取完整、新序列且不早于实际 input_end_ms 的证据，并从当前对象重新绑定容器；原 Attempt、期限、额度不重置，每次输入取得新许可。

BindingReducer.bind_in_container 只从该容器在同一窗口的当前后代解析目的地，父链有循环/深度上限；同标签的外部对象、旧虚拟行和陈旧绑定不能证明成功。像素方向按已验证的 CG 原语约定：Down/Right 为负 Y/X，Up/Left 为正 Y/X；Source 内容位置变化与事件轴是不同坐标语义。

## Source 的实际位置与未知范围

UiObject.scroll 提供水平/垂直 ScrollAxisState，value 来自实际 AXScrollBar；min/max 分别可选。读取前核对 AX 类型、同 PID 和 AXScrollBar 角色，所有读操作沿用原有超时与预算。值和已知边界必须有限，值不能超出已知边界，两端已知时必须 min < max；不能在敏感或非滚动区域附加指标。Source 的采集一致性检查同时核对滚动状态。

真实自有 AppKit 诊断显示：普通 NSScrollView 和隐藏 AX children 的子类都能提供 scrollbar 引用与 AXValue，但 AXMinValue/AXMaxValue 返回 attributeUnsupported (-25205)。因此不为角色补造 0…1。位置存在、范围未知时可验证实际移动并继续有界输入；未知端点不能推出边界。证据为 [原始属性诊断](../../artifacts/m3-native-scroll-source-range-diagnosis.txt)和[实际 AXReader 输出](../../artifacts/m3-native-scroll-source-partial.json)。临时诊断应用只读且已退出，没有激活或输入事件。

## 效果与有限结束

偏移变化只证明进展。成功要求目的地可见，并由 Environment 在选定容器内用当前证据重新绑定；Engine 结束技能前由 Verification.scroll_target_bound 独立再验证，持久化 scroll.target_visible_bound@1、CurrentUi 导航 EffectResult 后才把子步骤标记完成。根目标原有编辑值、重访或提交谓词继续独立判定。

未知回执后不请求下一次滚动；新证据可以核对目标，但不能重发输入。缺值、陈旧/不完整证据或不明进展不会变成成功。达到实际已知的方向端点但目的地缺失时返回 NotFound，保留 observation_complete 与 search_complete；归一化位置和端点不足以证明虚拟内容无遗漏，当前 search_complete=false，根一次终止并明确覆盖范围未证明。未知范围下用完动作额度则 BudgetExhausted，不报告虚构端点或全局缺失。

## 验证范围与剩余工作

技能回归覆盖四向事件符号、实例复用、容器替换、容器外重复项、仅偏移进展、输入后时间下界、新来源序列、缺值/缺界/非法范围、unknown 不重复与额度。六项端到端测试使用确定性虚拟列表和真实 Engine/ExecutionCore：同一 Attempt、多次新许可、新绑定与独立导航效果；更强根谓词未满足时不完成；边界与未知范围分别终止，unknown 保持暂停。

desktop-v1 的 ScrollToSpec 和 Source 指标已同步导出 Rust Schema、跨语言 fixtures 与 TypeScript 类型。预发布组件必须使用同一新 Schema 哈希，握手继续拒绝不同构建契约；I0 wire/golden 不因这次扩展更换。

全量 608 Rust、121 TypeScript、550 Swift（60 Source、483 device、7 Keychain）通过，Rust 运行器串行。14 项技能与六项 Engine 回归通过；同一源码/Schema 构建的 12 项自有设备矩阵再次通过，Source AXValue 与独立实际 NSScroller.doubleValue 一致，min/max 保持未知。范围架构复核通过。日志、源码/构建哈希与边界见[本轮证据](../../artifacts/m3-scroll-runtime.json)。原生 Host 滚动任务的鼠标准备/导航闭环、AXScrollToVisible、完整内容搜索覆盖、菜单辅助窗口选择、TextEdit 保存/重开、真实 S1/S2/Laya/MediaPipe、持久恢复、100+20 任务、环境/资源和签名仍开放。完整 MVP 目标保留。
