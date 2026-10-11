# 原子滚动与 AX 菜单

2026-10-03。生产设备路径新增单次 Scroll 和 AXShowMenu；此记录描述设备能力边界，通用 scroll_to/菜单选择技能与 TextEdit 保存任务继续开放。

## 提交范围

固定 app/PID/window、内核进程起始身份、双租约、停止序号、原始绝对期限、同实例 BindingProof 和新许可检查沿用现有路径。只扩展私有 NativeProbe 请求，不改变 desktop-v1 契约。scroll 载荷仅 dx/dy，拒绝额外字段；其它动作不接受这个载荷，也不能混用 value/mouse。

`macos-atomic-scroll@1` 只接受 AXScrollArea，每轴像素增量在 [-120,120] 且至少一轴非零。一次 private-state CGEvent，flags/phase/momentum 为零，没有 held 状态或追加流。CG wheel1=dy、wheel2=dx；负 dy 向文档下方、负 dx 向右。构造器没有位置参数：实际鼠标与未改写的事件位置必须精确等于证明框中心，在提交门内再检查一次。不移动鼠标、不假造事件位置；不满足前置条件就拒绝，提交门已消耗意图而条件变化则 unknown 且不重放。

事件携带现有非零 marker 和唯一单调 stamp；监听 tap 按 marker/type/stamp 确认 scrollWheel，最多等待 150 ms 且不超过原始期限，等待在提交锁外。缺确认保持 unknown、隔离通道，原意图只查询；新意图也不能绕过隔离。tap 确认是输入证据，业务效果由后续新 Source 另行验证。单步原子 Scroll 按实际 input_end_ms 结束，不等待 command_end。

AXShowMenu 只接受 AXMenuButton/AXPopUpButton，在当前前台标准非模态窗口验证完整 fp1、enabled、焦点和声明的 AXShowMenu。最终命中的 AX 引用只留在 worker 栈上，在有序门里仅调用一次；消息超时最多 25 ms 且受原始期限约束。任何调用后的不确定结果不重试。AppKit 菜单追踪可能持续至菜单关闭，独立 watchdog 保留硬上限。AX success 只说明调用已返回，不能证明菜单显示或选择成功。

## 验证与剩余工作

新增 Rust 回归覆盖增量/角色拒绝、载荷隔离、实际返回时间及 unknown 不重放；Swift 检查覆盖事件像素轴、flags、phase、momentum、marker 和严格载荷。独立架构复核批准上述范围。现场自有 ScrollFixture 用一次授权点击准备鼠标，再取得新的 AXScrollArea 证明；滚动效果来自真正的 NSScrollView，不由测试命令伪造。MenuFixture 使用真正 NSMenu，分别记录调用计数、追踪、实际辅助窗口和原生项目框。

全量 586 Rust、121 TypeScript、545 Swift（55 Source、483 device、7 Keychain）通过，Rust 测试串行；日志与源码哈希见 [检查记录](../../artifacts/m2-native-scroll-menu-checks.json)。首次现场准备点击已知完成一次，随后检测到 AppKit 按钮外观的 AX 框与滚动容器框不同，检查失败且尚未提交滚动；原始结果归档，修复夹具后以新独立任务验证。第二次现场已实际投递一个 Scroll，真实偏移 y=160→280，但标签在动画前读取而陈旧，检查失败；夹具改为真实 clip 变化驱动可见标签。第三次八个滚动场景全部通过，菜单实际窗口与两个原生项目也可见；AppKit 在只读项目框初始化时产生的生命周期回调使绝对计数断言失败，驱动保留基线/增量诊断，动作次数与可见性继续独立严格核对。最终统一现场 [12 项矩阵](../../artifacts/m3-native-scroll-menu.json)全部通过，同一构建/源码哈希已核对，provider 调用为零；前三次原始失败在 artifacts/m3-native-scroll-menu.previous-* 中保留。
| 场景 | 实际输入 | 独立结果 |
| --- | --- | --- |
| 上/下/左/右 | 各 1 个标记 Scroll；准备 Click 各 1 对 | 偏移每轴 160→40/280，新 Source 标签一致，completed |
| Scroll 确认丢失 | 1 个 Scroll，真实偏移已改变 | unknown、通道隔离、重复不发、新意图拒绝 |
| Scroll 遮挡/标签陈旧/增量越界 | 0 个 Scroll | rejected，偏移不变 |
| Menu 实际展开 | 1 次 AXShowMenu | completed，实际 layer=101 窗口和 2 个原生 AXMenuItem 可见 |
| Menu 无效果 | 1 次 AXShowMenu | completed，但无菜单可见，输入完成不能证明效果 |
| Menu 延迟返回 | 1 次 AXShowMenu | unknown、通道隔离、重复不发、新意图拒绝 |
| Menu 遮挡 | 0 次 AXShowMenu | rejected，无菜单可见 |

菜单展开期间另采集真实 Source；该矩阵不建立辅助菜单窗口的输入权限。原生项目框读取可能触发 AppKit 布局生命周期回调，因此记录输入前基线与增量作为诊断，实际动作次数、CG 窗口和项目框分别校验。所有场景串行，原始 AppKit 事件不按期望过滤。

普通容器后代命中、任意鼠标位置、菜单辅助窗口的新权限/后续项目选择、通用 Host/Engine 技能、TextEdit 保存/重开、真实模型、持久恢复、完整任务集、环境/性能与签名仍开放。MVP 未完成。
