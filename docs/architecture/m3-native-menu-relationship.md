# 原生菜单归属诊断

2026-10-03。自有 MenuFixture 经生产 ExecutionCore/NativeBackend 完成一次 AXShowMenu，随后只读探针核实实际菜单关系。当前没有菜单选择输入或通用菜单技能。记录见[现场原始证据](../../artifacts/m3-native-menu-relationship-probe.json)。

探针只接受启动脚本的同父进程、自有 bundle、该次唯一可执行路径和内核启动身份；结束再次复验身份。所有 AX 引用在读取前核对 PID，外部遮挡只记录 PID，不读内容。遍历、属性调用、父链和点命中受节点、深度和两秒总期限约束。本次没有期限超出或遍历失败。诊断 ID 只用于这次探针内 CFEqual 对照，不是执行绑定或许可。

两个实际 Fixture menu first/second item 均 enabled=true，声明 AXPress/AXPick/AXCancel，实际父链为 AXMenuItem → AXMenu → AXMenuButton → AXGroup → AXWindow → AXApplication。item 的 AXWindow 返回 -25212/noValue，不能当作存在或 attributeUnsupported；AXTopLevelUIElement 指向当前原始 AXStandardWindow。各自实际 AX 框中心的 systemwide 点命中均返回同一 PID，CFEqual 与目标 item 相等。

WindowServer 同时存在原窗口 layer0 和菜单绘制窗口 layer101。绘制窗口不能直接变成授权窗口；目前只有该次 actual parent/top-level 关系证据证明菜单属于原窗口触发控件。所有菜单关闭前后实际 ShowMenu 调用数为1、选择数为0。

生产 Source 在菜单展开后仍报告 complete=false、modal or transient active layer is unresolved，并因此不捕获截图。现有 fp1/HitTest 要求 AXWindow，不能直接选择这些 item。下一步增加明确菜单上下文和菜单专用复验：固定原窗口、当前同进程启动身份、展开触发控件的连续实际身份、当前菜单父链、顶层原窗口、当前几何和点命中都成立后，才从新 Source 绑定一个精确且唯一的一级菜单标签。普通窗口控件不应借菜单上下文取得输入资格。

菜单展开会触发 Source topology/observer registry 失效，旧 instance_ref 与旧许可不得复用。需要有界只读 opener 身份锚以及每次重新建立关系证据；同标签、同 PID 或相同 fp1 摘要均不能替代实例连续性。展开和一次选择保持同 Attempt、原期限和额度；unknown 后验证，不重发。选择成功须由新 Source 读回实际选择值，菜单关闭、AX 返回或夹具回调计数都不能单独完成业务效果。

重跑命令：node scripts/probe-macos-menu.mjs --skip-build。它复用现有生产二进制，只编译唯一自有夹具与只读探针；实际菜单选择、完整 Host 菜单任务和 MVP 验收仍开放。
