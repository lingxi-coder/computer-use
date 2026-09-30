# 架构评审 R3：统一完整设计与实施边界

- 日期：2026-09-30。
- 请求：review，并整理成一份完整设计文档。
- 范围：v0.3 通用稿、桌面模块化补充、R1/R2；交叉核验当前 Harness 源码、外部 I0 快照和相关官方资料。
- 产物：[完整设计 v0.4](../design/complete-design.md)，作为唯一主设计入口。原两稿保留为历史记录。
- 方法：作者整合与修订；独立架构、需求完整性、本地复用和官方资料审阅分别提供发现，整合稿再独立复核。
- 初审结论：REVISE。以下 10 项设计问题需要统一或补全；不是发现 10 项已实现代码缺陷。
- 最终结论：APPROVED（设计范围）。独立架构复审确认 R3 的 10 项问题及 R2 的 5 项契约已一致整合，没有新的阻断矛盾；需求完整性、本地复用与官方事实复核通过。实现与性能验收仍在 M0–M5。

## 问题与修订

历史定位为本轮开始前文件行号；历史稿后来仅增加“已替代”入口说明，因此行号可能平移。关闭依据以主文档章节为准。

| ID / 级别 | 原稿位置及具体问题 | 主文档修订与关闭依据 |
|---|---|---|
| R3-01 / P1 | v0.3 §6 第 394 行将 task/budget、held input、skills 放入同一 BeliefSnapshot；§16 第 974 行让 Memory 建 TargetBinding。实现会形成两个权威写入者。 | §2/6/16：七模块单写，EnvironmentSnapshot 与 DecisionContextView 分开；只有 Environment.BindingReducer 建立当前 binding，Memory 返回候选知识。 |
| R3-02 / P1 | v0.3 §20 第 1177 行将根工作流与局部控制都交 SkillRuntime，和桌面稿 TaskRuntime owner 冲突，可能重复推进/恢复。 | §2.3/13/20：TaskRuntime 写根节点与 active subgoal；Reaction 只推进已激活 attempt 和局部子技能。 |
| R3-03 / P1 | v0.3 §11 第 696 行仍保留旧式单步接纳，可能绕过 R2 的预算/取消复验。 | §11.2/11.3/16.6：ReserveAttempt → 持久唯一映射 → ActivateAttempt → Execution 复验；accepted 仅指已激活，reserved/mapped 不得输入。 |
| R3-04 / P1 | 桌面稿 §6 第 159 行“不恢复旧任务”与 v0.3 第 887/1007 行同 root 再授权恢复冲突，重新建 root 会重置预算。 | §13.5/14.6/16.6：cancelled 为终态；suspended/needs_revalidation 仅经清理、新观察、原 deadline/预算与显式 resume 才继续。旧许可/绑定/输入队列始终不恢复。 |
| R3-05 / P2 | 重访验证与 TextEdit 读盘验收未指定谁执行；Verification 可能绕过 Reaction 操作，隐藏测试真值可能流入在线决策。 | §14.7/26.7：Verification 只发布 EffectResult/EvidenceNeed；有输入的重访/重开由 TaskRuntime→Reaction→Execution；独立 evaluator 读盘不进入 StateCore、模型或知识。 |
| R3-06 / P1 | 桌面稿 §2/6/8 仅写“持久记录”，未落实根预算、决策映射、outbox 与设备预记账的事务和 ACK 边界。崩溃后不能区分预留、激活与已投递。 | §16.6：host SQLite WAL 单物理 writer、各 owner 窄存储端口、三个持久提交点；device 独立带 checksum/epoch 的 journal 与清理 lease；停止不等写盘。§4.7 保留重启期限语义。 |
| R3-07 / P2 | 桌面稿 §4/8 进程清单没有 IPC bootstrap、Ready、角色认证、supervisor 与二进制入口。只列 library 不能说明如何启动、隔离和恢复。 | §3/4.6：Electron main supervisor、cu-host/cu-device/cu-perception/utility process；私有 UDS、长度前缀协议、角色/身份/epoch 握手、图像与控制通道、独立停止和明确 binary 目录。 |
| R3-08 / P1 | 桌面稿 §7 的“独立 home + shared credential stack”可能被实现成 Harness 便利 factory；isolated=true 实际选择明文 `.credentials.json`，也不能单独阻止请求层环境 fallback。 | §12.6：显式 InMemorySecureStorage/CredentialManager + ephemeral seed；每 root wrapper，仅共享指定 Arc；本产品 Keychain 持久化；env 白名单与模型路由显式配置；M0 验证无明文文件和跨产品密钥读取。 |
| R3-09 / P2 | 两份稿都承担 canonical；Jev/Laya 首轮、首设备待定、游戏“第一版”、P/I/M 阶段与两套目录冲突。不同读者会得到不同交付范围。 | §1/3/8/17–19/25/28/29：唯一完整主稿、一期 macOS/Laya/MediaPipe/Harness，游戏/手机/训练后续，M0–M5 唯一实施路线；§26.1 Jev 对照也后置。README 和旧稿入口同步。 |
| R3-10 / P2 | v0.3 第 6/1561/1596 行把外部 I0 描述为当前仓库已有实现，并链接不存在的 src/contracts/tests 与旧拆分稿。 | §27.4/30：明确外部无 Git 元数据快照、当前仓库仅设计、三项内容哈希、历史 36 测试的适用边界、先原样验证再机械迁移，删除主文档的失效本地链接。 |

## R1/R2 契约保留

统一主文档直接包含七模块及其所有权、Reaction 完整闭环、只读组合视图、Memory revision/release；并保留 R2 的跨 owner 接纳、每 root 独立 Harness runtime、成功 StructuredOutput tool-use ID 计数、cancel/late result 隔离、设备 down 前持久预记账和 cleanup-only 恢复。

固定 100 次 fixture、20 次 TextEdit、全部接纳任务分母、独立故障矩阵、冷热统计和停止门禁已并入 §26。原 30 章的跨设备、游戏、空间、知识发布、训练标签、未来信息泄漏和晋级约束均保留，未来能力单独标明。

## 官方资料与本地核验补充

- MediaPipe 的 CPU/C++ ImageEmbedder、ROI 与源时间/坐标/回调契约仍成立；embedding 不被解释为 GUI 控件语义或任务完成。
- Laya Node 使用官方 Node-API addon / ONNX Runtime；macOS arm64 CPU 预编译支持不代表 Electron 签名包已验证。上游 numThreads 只是 best-effort，M0 要检查实际 session 线程和共同负载。
- 官方 LiteRT macOS Metal SDK 和现成 inference calculator 均存在；所选 MediaPipe graph/模型/签名资源仍需独立验证，CPU 基线不宣称 GPU。
- 相似系统的职责采纳保留，并加入 OpenHands 只读派生 view 的具体源码依据；没有运行这些第三方系统做性能比较。
- I0 三个核心文件 SHA-256 在本轮再次核对，与 §27.4 基线相符；本轮没有重跑 I0 测试。

具体一手链接集中在主文档相应章节和 §30.3；官方 master/main 链接不是依赖版本 pin。

## 关闭复核与验证边界

独立架构审阅覆盖完整 30 章：核心 §1–16 由架构审阅者检查，§17–30 另由其只读子审阅者检查；作者没有替代独立批准。需求完整性复核确认原 30 章关键要求保留，本地复用复核确认 R3-06/07/08/10 关闭，官方事实复核确认 MediaPipe、Laya 和参考系统边界。后续发现的 goal_draft 容器、Jev 对照范围和训练阶段说明已定点修订并复核。

本轮 R3-01–R3-10 全部关闭，没有待处理的设计阻断项。已批准主文档 SHA-256：`9da86109e3d790ca26d55cffe01279f7c0ebd504b6d99028ad40a5c86ea6438e`。该哈希用于绑定本次审阅快照，后续正常设计更新应重新记录相应审阅范围。

本轮文档检查通过：README、新主文档、R3 的本地链接；30 章及 129 个子章节的编号与显式锚点；数值章节引用；代码围栏和尾部空白；历史入口新链接；4 个 Mermaid 图的结构检查。未运行 Mermaid 渲染器，结构检查不等同浏览器渲染验收。原历史稿已有的不存在源文件链接保留其历史上下文，不作为新主文档入口。

没有编写运行代码、重跑 I0、构建 MediaPipe/Electron、运行模型、调用云端模型或完成真机停止/签名产品验收。M0–M5 门禁均保持待实施状态。
