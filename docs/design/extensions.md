# Computer User 设计扩展卷：游戏、空间与 WoW

- 文档版本：随核心卷 v0.5，2026-09-30。
- 地位：[完整设计文档](complete-design.md)的扩展卷，收录第 17–19、25 章。章号与核心卷保持一致；未注明的章节引用均指核心卷。
- 范围：后续扩展，不属于一期交付，不进入一期验收分母。本卷内容自 v0.4 原样移入，没有随 R4 评审修订。
- 启用前提：本卷涉及的持续按住、拖拽与游戏按键属于按住类原语，启用之前必须先交付核心卷第 14.6 节的设备死亡清理并通过验收。

## 目录

- [17. 游戏能力与 S0 局部控制](#sec17)
- [18. 空间定位、World Atlas 与可选三维地图](#sec18)
- [19. WoW 环境包：从专用方案到通用模块](#sec19)
- [25. 游戏端到端案例：到达地标并交互](#sec25)

<a id="sec17"></a>

## 17. 游戏能力与 S0 局部控制（后续扩展）

该扩展首版限定在已验证相机/输入配置下的短路线与交互。输入包括当前与近期场景、下一地标或路线节点、角色/相机关系的可用证据、最近实际输入和控制配置。

Jev/Laya 作为 S1 选择地标、交互技能和恢复分支；S0 控制器在本地按新观察处理转向和移动反馈。两层独立调度，远程决策 API 不进入每个运动 tick。S0 初期可用已标定规则形成对照，后续替换学习型局部控制。

首个可学习任务建议为：在允许的短动作集合中选择转向、前进、暂停观察等候选。动作时长和幅度来自已验证的 ControlProfile；模型不能直接产生无限持有按键或任意扩大动作范围。

```text
LocalPolicyInput
  current_and_past_frames
  next_landmark_or_route_segment
  current_motion_evidence_and_missing_mask
  actual_recent_inputs
  control_profile_revision

LocalPolicyOutput
  bounded_action_candidate / observe / stop
  expected_progress_evidence
```

画面变化不能单独证明角色移动；相机旋转、动画和环境运动作为专门反例。到达和交互成功分别验证。规则控制与学习控制使用同一动作集合、Guard 和完成标准。

战斗选择、追踪和空间导航可以后续拆成不同技能策略，不要求用一组输出同时覆盖全部游戏。WoW 的职业、GCD、地图和副本规则继续属于其领域设计。

### 17.1 通用游戏能力包

游戏按 UI 操作、空间导航、实时战术和回合决策等能力组合。背包/交易/设置复用 UI 能力；移动、目标跟踪和战斗各自提供领域状态与决策程序。

领域状态保存 player_state、abilities、target_tracks、motion_mode、camera_profile、location_hypotheses 和 pending_effects。血量、资源、冷却或施法状态不可读时保留未知。环境包声明角色版本、键位、能力语义与观察策略，不能把某个 WoW 职业规则写进通用内核。

S1 选择下一地标、交互目标、战术技能和已知恢复；S0 依据新观察完成局部方向与短动作控制；S2 处理任务阶段、复杂路线和未知机制。导航到达、对象存在和交互效果分别验证。

### 17.2 ControlProfile 与反馈状态机

```text
ControlProfile
  revision, environment_scope, camera_revision, input_mapping
  motion_mode, turn_response_intervals, motion_response_intervals
  input_latency, stop_tail, min_effective_pulse, max_pulse
  valid_conditions, evidence_refs, held_out_validation

VALIDATE → ALIGN → WAIT_TURN_FEEDBACK → CHECK_CORRIDOR
         → ADVANCE_PULSE → WAIT_MOVE_FEEDBACK → VERIFY_PROGRESS
任意状态 → RESENSE / REVOKED / RECOVERING / FAILED
```

标定分别测左/右转、前进、启动延迟、释放后停止尾迹及重复散布，保存查找表或响应区间，先限制到固定相机与移动方式。配置、视角或持续残差变化使 profile 失效。正式任务不能无限试错代替标定。

朝向控制只有在同一可靠参考系中计算目标差，使用进入/退出阈值形成滞回。局部脉冲上限由已验证 profile、根预算、当前场景变化、证据剩余年龄减释放余量和路线片段范围共同限制；不足最小生效时长时刷新证据，不强行拉长。

首个游戏版本转向和前进分开；等待前一动作反馈时不因 tick 再发同一脉冲。输入到固定终点必释放，即使技能仍等待观测。进展优先用沿当前路线的位移、地标关系或节点推进，相机绕转不能独自证明角色移动。

<a id="sec18"></a>

## 18. 空间定位、World Atlas 与可选三维地图（后续扩展）

### 18.1 位置假设与空间身份

PositionHypothesis 保存 world_space、region、floor/layer、place、可选坐标/范围、朝向、支持/反证、最后真实观察、来源和有效状态。多楼层或相似房间的候选不平均成一个位置；unknown 身份不能当通配符合并。

地图锚点、小地图连续跟踪、视觉地点匹配和可选局部几何分别发布证据。唯一地点需要兼容空间、独立或连续有效支持、优势门限和没有重大反证。来源组去重，裁剪候选保留覆盖不足标记。预测用于寻找，不能独立确认到达。

多假设下允许动作必须在各候选坐标变换后对应同一实际输入并满足约束；仅名称都叫“向右”不构成交集。未知分量影响当前移动或候选覆盖不足时优先停/观察；共同动作不能证明唯一位置。

地点信息获取也消耗根预算。InspectMap 按技能验证当前可暂停 → 释放冲突输入 → 开图 → 读取锚点 → 关闭 → 新主场景确认；地图遮挡关键字段时取消业务输入，关闭已确认面板若需清理动作则单独限定清理能力，不能扩大为移动/战斗授权。

### 18.2 图集与连接

```text
WorldAtlas
├─ world_spaces / region_maps / place_nodes
├─ connections / movement_conditions
├─ optional_submaps_3d / semantic_layers
├─ instance_definitions / session_overlays
└─ immutable_assets / evidence / releases
```

render_transform 仅用于图片展示，planar_alignment 表示经过验证的平面关系，connection 表示指定端点、方向和条件可达。拼图接缝、名称相同或二维相邻不证明可走；二维锚点不能补出高度和米制尺度。

连接生命周期是 CANDIDATE → PROVISIONALLY_TRAVERSED → VALIDATED，冲突进入 QUARANTINED / INVALID；反向边独立验证。正常复走使用 VALIDATED 边，候选探索限定当前连接、可见范围、移动方式、回退点、风险和根预算，探索不打开全局未验证路线。

世界/区域/楼层/局部分层规划，下一目标是可达节点。路径先过滤硬条件，再比较非负耗时、风险、不确定与失败成本；展示距离不是合法启发式。资源收益放目标选择层，避免负边循环。

分别报告底图、视觉、语义、通行和三维覆盖，底图道路不是已验证路线。未覆盖地区保留未知；发布和隔离使用与通用记忆相同的不可变修订及独立撤销原则。

### 18.3 地形、资源与时态覆盖

视觉类别、表面几何、角色可通行性和风险分别维护。只有图像分割时使用 VisualCorridor 和有界动作；可信局部几何支持栅格/分层表面；长期验证几何才参与稳定路线和导航网格。FREE / BLOCKED / UNKNOWN 保留观察范围和证据等级，分割填洞不证明地面连续。

桥上/桥下、洞穴/室外和不同楼层保持不同层及显式连接。入水、出水与游泳有独立条件，只有入水证据不证明完整横渡。一次没有进展先检查焦点、输入、相机、角色状态和定位，不能直接污染永久障碍图。

EncounterPrior 记录历史分布，LiveEntityTrack 记录当前会话；ResourceSite、ResourceObservation、ResourceAvailability 分开，后者为 PRESENT / DEPLETED / UNKNOWN / STALE。视频中的观察者位置不当实体刷新位置；遮挡、未覆盖或 TTL 过期不证明资源不存在。刷新估计保留真实时间、独立遭遇、采样覆盖与不确定性。

### 18.4 实例与独立空间

InstanceDefinition 保存稳定房间/楼层/连接；InstanceRun 保存本次门、机关、敌人和进度。进入/退出实例是注册复合技能：确认正确入口及一侧 → 有限穿越或交互 → 观察转换 → 确认预期空间与楼层 → 关联当前运行。

加载、黑屏和二维接近不表示进入成功；未知转换返回 CONTEXT_UNRESOLVED。run_id 是本系统关联身份，不伪装成服务器 ID。重新进入或不确定是否延续时，保留候选运行身份，旧动态事实降为未知。

### 18.5 三维几何与原生适配

SLAM、稠密重建和学习式世界模型是可选能力，首个短路线无需它们。通用内核只消费带状态、坐标定义、尺度与版本的几何证据。

ORB-SLAM3 等单目分支需要连续画面、真实时间与经过检查的相机配置，输出是相机轨迹和稀疏几何。纯单目单位不自动是米，相机位移不等于角色位移。UI、角色和动态效果按实际适配器过滤，跟踪丢失停止几何准入。

稀疏点、稠密表面和语义/通行地图是不同资产。稠密方案可用离线 RGB 多视角重建，或经校准的数值深度融合；额外深度不是全部系统必需输入。深度需声明轴向/射线/逆深度/相对定义，不能把热力图或逐帧归一化值拿去融合。内参、位姿、深度、体素和障碍尺度一致，拟合与验证点分开。

局部控制参考与全局修正分开。子图/尺度/参考不可解释跳变递增 control_epoch、撤销相关动作、重新建立参考；地图修正不当角色速度。原生活动地图是可变工作区，发布资产是不可变快照；几何修订影响的网格、语义投影和路径先重建验证，再原子发布。

原生快照必须有统一写入屏障或经审计的版本协议，涉及 Tracking、LocalMapping、LoopClosing、优化、合并、剔除和 Reset；仅前后读变化计数不能证明一致。轻量 PoseEvent 与完整 SlamSnapshot 分开，不能对外暴露裸指针。

锁定原生源码、相机、词袋、导出与快照版本后，逐项验证跟踪状态、pose=null、单目导出、重置/退出握手和跨对象一致性。来源、native_atlas_epoch、native_map_id、snapshot_revision、geometry_revision、coordinate_frame_id 分别登记；旧映射需要重映射/墓碑，不能假设二进制地图跨版本兼容。

<a id="sec19"></a>

## 19. WoW 环境包：从专用方案到通用模块（后续扩展）

本章将原 WoW 设计中的领域要求纳入通用接口。它是目标环境包规格；当前没有 WoW 采集、模型、控制或地图资产验收。通用章规定的 S1 默认主路径优先于原稿中的“规则优先、可选模型”路线。

### 19.1 环境包组成

| 模块 | WoW 内容 | 通用映射 |
|---|---|---|
| 感知 | 血量/资源条、目标框、施法、技能状态、地图、场景实体 | PerceptionRequest、Observation、ObjectFeature |
| 状态 | 职业/角色能力、移动方式、相机、战斗与目标 | domains.game-specific、ControlProfile |
| 快速决策 | 目标/技能、路线节点、观察地图、局部恢复 | S1 DecisionProgram、动态候选 |
| 局部控制 | 固定相机短转向/移动、地标/走廊反馈 | S0 LocalPolicy、动作证据策略 |
| 战略规划 | 任务阶段、复杂路线、未知机制、经验提炼 | S2 PlanBundle 与候选知识 |
| 地图 | 区域、入口、桥、楼层、资源、怪物历史与当前覆盖 | WorldAtlas、位置假设、release |
| 实例 | 副本定义与本次门/敌人/进度 | InstanceDefinition / InstanceRun |
| 交互 | 背包、物品、对话、设置和显式提交 | UI 技能、ScreenSchema、GoalSpec |

### 19.2 版本、相机与地图契约

客户端版本、语言、职业/能力、键位、HUD 布局、地图旋转/缩放与角色/相机朝向分别验证。小地图是否北向固定、玩家是否居中、滚轮改变 FOV 或距离都不写成跨版本常量。

主场景、地图、加载帧分路。scene_px 裁剪与内参一起变换；minimap_px、region_uv、atlas_display_xy、submap_xyz、camera_xyz、control_local 不互相冒充。方向差在可靠统一坐标中环绕计算，桥上/下与副本楼层保留不同身份。

如选择 ORB-SLAM3，按锁定源码验证 TrackMonocular、GetTrackingState 与导出/重置行为。isLost、MapChanged 等函数名不构成语义保证；纯单目完整轨迹是否支持必须通过代码和实际验收。自定义动态掩码、快照与 IPC 属于项目工程，不能假设官方封装已完成。

### 19.3 任务与打断

示例注册技能为 Reach/MoveTo、Interact、Gather、InspectMap、EnterInstance、ExitInstance、Fight 与已验证恢复。Reach 的地点成功不要求资源仍存在；Gather 另验证 resource_acquired；进入/退出另验证空间上下文。

战斗事件可抢占导航，但由 TaskRuntime/SkillRuntime 取消或暂停当前技能、释放冲突资源，再接纳已授权技能。S1 选择战术候选，精确冷却、GCD、输入期限和释放由代码处理；读不到必要状态时先观察或停。未知战斗机制才请求 LLM，已知反应不等待其返回。

完成采集、目标死亡、资源消耗或对话结果分别从新证据判断，技能输入回执不能直接充当业务效果。后续可扩展其他 MMORPG、动作/策略/回合游戏，只替换环境包和相关能力模块。

<a id="sec25"></a>

## 25. 游戏端到端案例：到达地标并交互（后续扩展）

任务 ID 为 `GAME-REACH-01`：在固定输入和相机配置下，到达指定可观察地标并完成一次交互。先使用可复位场景验证协议，再迁移到选定 WoW 路线；两者的验收结果分别记录。

| 变体 | 要验证的行为 |
|---|---|
| 已知宽阔短路线 | 短动作、真实进展与到达闭环 |
| 只转相机、角色未前进 | 不把整屏变化判为移动进展 |
| 目标暂时遮挡 | 区分预测与真实观测，失效时停止/重观察 |
| 路径或局部障碍变化 | 旧通行证据失效，不继续旧脉冲序列 |
| 持有移动输入时失焦 | 设备监督器撤销并释放输入 |
| 到达地点但对象不存在 | 到达可以成功，交互返回未存在/未验证 |

该扩展首版就用 S1 快模型选择技能，S0 可采用已标定的规则局部控制。控制标定、目标绑定和效果确认稳定后，再用相同候选动作比较学习型 S0。SLAM、稠密深度或全局世界模型不作为该首个闭环的强制前置。

### 25.1 完整步骤

1. 接纳冻结的 Reach 与 Interact 目标、环境/相机/控制配置和根预算。
2. 取得当前场景、地点/楼层假设与所需角色状态，缺失先观察。
3. S1 选择下一个可达地标与移动技能；已有路线只提供兼容先验。
4. S0 按 profile 进行短转向，等待新方向反馈；走廊有效后进行短前进。
5. 执行端持续检查依赖并在终点释放；新观察用于进展和到达判断。
6. 到达后重新绑定可见交互对象，S1 选择交互技能，Verification 单独检查效果。
7. 歧义/局部失败先有界观察和恢复，复杂路线或未知机制才升级 S2。
8. 按冻结范围报告到达、对象存在与交互结果，记录未知和失败，不合成一个假成功。

到空资源点允许 Reach 成功、Gather 返回未存在或未验证；视角绕转、流水和动画不充当前进证据。WoW 路线与可复位游戏夹具分别报告，模型暂停与失焦注入贯穿整个闭环。
