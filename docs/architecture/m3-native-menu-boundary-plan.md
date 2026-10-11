# 原生菜单选择边界矩阵

状态：脚本已静态实现；父任务首轮 applied 在实际选择后因资产目录权限失败，修复后待重新运行，完整矩阵尚未验收。它覆盖已登记自有 MenuFixture 的 native helper 边界；不能替代 public Core/Engine、Operator、真实模型、20 次 TextEdit、100 次完整 fixture、资源门槛或签名打包等完整 MVP 验收。

脚本：[smoke-macos-menu-selection.mjs](../../scripts/smoke-macos-menu-selection.mjs)。生产方案：[m3-native-menu-selection-plan.md](m3-native-menu-selection-plan.md)。

## 运行方式与归属

必须等待其他 Operator/fixture GUI 作业结束，由父任务串行安排。使用已构建、当前版本的 Source、device helper 与包含故障控制的 MenuFixture.app。脚本不编译、不替换共享二进制，不附着既有应用；每个场景启动自己的 fixture，核对真实子进程 PID、登记 bundle、标准窗口及同一 watch/guard 生命周期。输入仅通过该固定 PID/窗口 helper。

```sh
node --check scripts/smoke-macos-menu-selection.mjs
node scripts/smoke-macos-menu-selection.mjs --self-test
node scripts/smoke-macos-menu-selection.mjs --list-scenarios

node scripts/smoke-macos-menu-selection.mjs --run \
  --fixture /absolute/path/to/MenuFixture.app/Contents/MacOS/MenuFixture \
  --source /Users/luolingfeng/lingxi/computer-use/target/debug/cu-macos-source \
  --helper /Users/luolingfeng/lingxi/computer-use/target/debug/cu-macos-device-guard
```

可用 `--scenarios applied,disabled-item,anchor-replacement` 定向运行；报告保留未运行场景，不能把子集称为全矩阵。默认完整列表包含未验证 AppKit bridge 的真实延迟入口，若它不被调用则该场景失败。`--output` 仅接受新建或空目录；默认使用独立临时目录。退出时保存报告、所有请求、原始 stdout/stderr、Source 资产与二进制/源文件哈希，失败不覆盖以前的记录。

## 真实证明链

1. 使用 `watch` 与 `text_policy={kind:registered_editor,window_id:<实际固定标准窗口>}` 启用菜单 Source profile；不使用不存在的 `origin_window_id` 请求字段，也不把一次性 observe 当跨输入锚。
2. helper 配置 `clock_origin_ms=0`，因此其共享毫秒域就是 Unix 毫秒，与 native Source 已发布的 `verified_observed_ms` 一致。脚本核对 nested sequence/time 等于实际 native observation，不改写 Source provenance。fp1 的缺省 nullable 字段仅按 Rust 序列化补为 null，不修改身份、描述或几何。
3. 正常完整读必须给出唯一当前 opener/Source 锚。发送带 `Opener` scope、固定 activated-attempt ceiling 的单次 ShowMenu，取得真实 Applied/native returned_ms。随后撤销该短步骤 permit，保留 helper 内的已结算历史事实。
4. 同一 watch 的新完整 menu context 必须具有更大 sequence、晚于真实 returned_ms 的 Source 时间、相同锚与 Source session。项目从该实际 menu 的直接子树按完整规范化标签定位；scope、fp1、frame 均由实际记录构造。
5. 单次选择必须由 native helper 独立复验后发出一个 AXPress。成功需要新 Source 读到实际 opener AXValue 等于选项标签且 menu context 消失；菜单关闭、AX 返回成功或 fixture callback 计数均不能单独证明业务结果。
6. fixture 的原生菜单计数、AX entry 计数和故障控制只供独立驱动器核验，不进入生产 Source/Execution。根窗口 capture stream 不被宣称包含辅助菜单的像素。

## 场景与可接受证据

| 场景 | 实际设置 | 接受条件 |
| --- | --- | --- |
| applied | 正常原生 NSMenu | ShowMenu Applied；撤销展开 permit 后仍允许一次合法选择；新 Source AXValue 验证结果；同 intent 与新 permit 的第二选择均无输入 |
| stale-deadline | 冻结实际 item 请求，等原短输入期限结束 | 真实 helper 未提交，selection_calls=0；不得补发或延期旧请求 |
| closed-context | 取得真实 item proof 后通过自有控制关闭菜单 | 原 proof 无法执行，selection_calls=0 |
| source-session-restart | 关闭原 watch，启动独立 watch | 若新真实 context 可用，其新锚/session 必须被旧展开 audit 拒绝；否则在缺失当前证据处拒绝输入，并明确记录该拒绝层级 |
| anchor-replacement | 同 label/frame 的新真实 NSButton 替换 opener，旧对象强保留 | 冻结旧 proof 不可选择，实际 selection_calls=0；不能用新标签/新锚补接旧展开 |
| changed-ceiling | 显式把 packet 的 opened_by ceiling 加 1 | helper 未提交；归类为 hostile wire negative，绝不声称 Source 或用户期限真的改变 |
| foreign-geometry | 实际标准 root 窗口移动 | 冻结原精确 root/menu 几何 proof 不可输入，selection_calls=0 |
| same-pid-other-window | 显示自有同 PID 遮挡窗口并使其成为 key window | 原固定标准窗口的菜单项目不可输入；不会向其他应用发送事件 |
| disabled-item | 取得启用项目真实 proof 后禁用实际首 NSMenuItem | native 最终 enabled 检查拒绝，selection_calls=0 |
| duplicate-label | 打开真实重复标题 NSMenuItem | 实际 Source 必须显示同一菜单中两个匹配项，测试驱动器拒绝绑定、零 selection submit。此项仅证明实际歧义及驱动器拒绝；canonical production BindingReducer 仍须另验 |
| unknown-selection | 真正 NSMenuItem AXPress override 延迟 200ms 后调用 super | 必须同时有 selection_started、selection_ax_press_calls=1 与真实 native Unknown/submitted 回执；同 intent、新 permit 同 attempt、同 attempt 重展开均拒绝；后续真实 effect 只观察、不重放 |

`delay_selection_mode=true` 或 UI/callback 已改变不等于 AX entry 已调用。当前 fixture 明确声明 `delay_selection_verified=false`；如果 AppKit AX bridge 绕过子类 override，脚本必须失败，保留实际回执和原始输出。helper 死亡/没有回执也不能伪造成一条 native Unknown 回执；其单独的传输隔离验收还需生产 Core pending-slot 路径。

## 实际验收及剩余范围

2026-10-03 已由父任务串行运行全部 11 个场景，记录见[实际原生边界矩阵](../../artifacts/m3-native-menu-boundaries.json)。正常选择取得 Applied 并由新 Source 读回实际值；延迟选择确实进入 AXPress override 一次，原始回执为 submitted Unknown，实际效果一次，重复意图、新许可同 attempt 选择及同 attempt 重展开全部拒绝。禁用、实际 opener 替换、根窗口移动、同 PID 遮挡、过期及关闭上下文均零选择输入。显式 ceiling 改包只证明畸形请求拒绝。

watch 重启场景在没有当前菜单证据处拒绝，未提交 native selection；重复标签由测试绑定器拒绝。这两项不宣称 canonical 生产 BindingReducer/Core 验收。所有原始请求、Source 资产、回执与构建哈希保留在报告指定的独立目录；初次资产目录权限错误的失败也保留。Node 语法及 14 项脚本检查通过，脚本以 0700 建立资产目录并在失败后输出报告路径，未降低 Source 目录要求。

完整 Operator 任务仍在定位 Source 锚退役原因。菜单栏/子菜单、CG 辅助窗口歧义、实际 child-membership/TopLevel 故障、stop/lease/revoke、Source 明确 destroyed/查询失效以及 helper 丢回执隔离仍待验收；未测项保持开放，完整 MVP 范围不缩减。
