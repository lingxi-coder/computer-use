# 原生 Host 滚动与重访任务

2026-10-03。自有 HostScrollFixture 已经通过实际 Operator 授权、目标确认和 Execute，完成隐藏目标的滚动定位、读值与返回读回。根按冻结的 ui_revisit 谓词 completed。仍为显式登记参考规划器，没有教师/S2 真实调用；这项任务不替代完整 MVP。

## 原子定位与滚动准备

新增 PointerPosition 是一个绝对 mouseMoved 事件。现有 PointerMove 仍属于 I0/按住类运动，AtomicOnly 继续拒绝；没有放开拖拽、down/up 或持有按钮。PointerPosition 要求完整点位命中、进程/窗口/启动身份、fp1、几何、焦点、非模态、enabled、原始期限、双租约和新许可，并核对启动和当前鼠标按钮都释放。仅登记非敏感指针角色和 AXScrollArea 可用，编辑字段暂未开放。

同一有序门只发一个 private-state CGEvent，flags 和 click state 为零，非零 marker/唯一 stamp 随事件携带；不调用 warp，不发补正、点击或独立修饰键。150 ms 确认期限锚定实际 posting 时间且不超过原始截止，只有截止前读到 tap marker/type/stamp 和实际光标点一致才返回 applied；跨截止的读数不能补认成功。缺确认、位置不符和时钟异常为 unknown，通道隔离，原意图只查询，新输入拒绝。回执只证明输入，后续 Source 独立证明业务效果。

受信 NativeWindowScope 启用每次 Scroll 前的定位准备；模型计划没有开关。定位后要求新 Source 序列、实际输入结束后的证据和重新绑定，完整容器框必须与准备时完全一致。相同中心的缩放和亚像素移动也会停止，不能用旧点追随新几何。定位和滚动在同一 Attempt 内计数，原生额度按最多两项输入每次滚动预留；次数、观察、根/尝试期限不重置。滚动和定位使用同一个由证明框导出的整数点，避免小数框中心与整数原语不一致。

## 冻结任务与独立效果

登记参考目标为 Reveal fixture scroll destination：实际 AXCheckBox / MVP scroll destination、Scroll fixture group、期望 false、ui_revisit。计划为 scroll_to → ensure_value → revisit，容器为 AXScrollArea / MVP scroll document，Down 120 像素最多四次。自有夹具是真实 NSScrollView、NSScroller、1200 pt 内容和原生 checkbox；初始目标在视野外，输入只通过生产 Host/Engine/ExecutionCore。静态窗口和 radio 导航提供 Document/Other 页面身份，返回保留实际偏移和 checkbox 状态。

现场四项鼠标输入严格为 mouseMoved、scrollWheel、mouseMoved、scrollWheel，滚动各 -120，实际偏移为 240。一个滚动 Attempt 的意图序列为 1–4，预留动作/观察额度为 8/9。新目标在容器内绑定，独立 scroll.target_visible_bound@1 / CurrentUi 效果持久化；ensure_value 读到实际 false 后不编辑。随后两次 AX 导航，返回后新来源读值和绑定满足 goal.required@1 / UiRevisit，根才完成。撤销后观察模式关闭输入且不补发。

原生账本没有逐许可日志，本报告不声称从 GUI 账本独立核验每个许可 ID；新许可/同 Attempt/未知不重放由生产接口及端到端回归覆盖。原生窗口本地监视器保留全部原始输入，测试只按 Execute 前基线划分，不过滤异常事件。

## 现场发现与修复

第一次实际滚动和独立导航效果成立，但 checkbox AXValue 没有暴露，根停止；夹具增加从实际 NSButton.state 读取的 AX getter，未写入期望值。第二次已经正确读值并完成两次导航，但隐藏目标由技能内部首次绑定，Engine 复用绑定时漏记目标页面，重访谓词保持 unknown。maintain_goal_binding 现在对复用的当前绑定登记页面，note_bound 使用绑定的实际观察时间，不用当前时钟补造返回证据。

新增 Engine 回归要求隐藏目标 → scoped bind → 已有正确值 → 离开/返回 → 新返回绑定，完成前两次导航时都不得 RootCompleted，最终仅一次完成且冻结谓词不变。两次原始 GUI 失败保存为 artifacts/m3-native-operator-scroll-*，最终实际 [GUI 记录](../../artifacts/m3-native-operator-scroll.json)通过 36 项检查，checkbox setter/press/click 均为零，导航两次。范围架构复核包括完整框、截止时间和 PageVisits 修复。

原子定位另有[三项真实设备矩阵](../../artifacts/m2-native-pointer-position.json)：成功一项事件，遮挡零事件拒绝，确认丢失仍仅一个事件且 unknown/隔离/原意图不重发/新意图拒绝。测试夹具不提供独立光标坐标，准确位置由 helper 实际读数与 tap 双确认；该限制明确记录。

全量检查通过 618 Rust、121 TypeScript、597 Swift/Keychain（60 Source、530 Device、7 Keychain），零失败和忽略；格式、Clippy、生成类型、类型检查与生产构建通过。原子定位三项矩阵及原有滚动/菜单十二项边界已使用最终同组二进制复跑。原有文本及开关 AX 回退 GUI 也用同一生产构建验证。授权驱动曾在独立等待 enabled 与后续点击之间遇到重绘，第二个对话框未打开；失败保留，驱动改为同一个同步操作检查并点击，仅点击一次，随后复跑。汇总见[当前检查记录](../../artifacts/m3-native-scroll-workflow.json)。菜单辅助窗口的绑定/选择、其余文本/按键/AX 原语、TextEdit 保存重开、实际模型/神经来源、持久恢复、100+20 任务、环境/资源与签名继续开放。MVP 目标保持完整。
