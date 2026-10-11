# 开发推进流程

本流程缩短实现反馈周期。完整设计 §26/§28 和 100 fixture + 20 TextEdit 的验收范围保持不变。

## 当前交接（2026-10-10）

- 最新真实验证与直接连接批次：[live OAuth](validation/live-oauth-direct-connect-20261010.md)。用户已登录并授权测试；GPT-6 Luna 真实 S2 目标解释成功，模拟执行因重规划预算耗尽失败，未计业务成功。Keychain 重启重连成功；按用户要求移除模型/提供商连接确认弹窗，点击 Connect 按已选用途直接连接，保留输入/窗口/任务确认。41项相关测试、类型检查、构建通过；实时模型目录刷新仍失败，真实 refresh token 轮换尚未验收。

- 最新凭据批次：按 Electron 设置语义统一 API key / OpenAI OAuth 的 Keychain 保存，提供已保存账号重连、重新登录、取消、退出登录。刷新 token 只留 Main/Keychain，Host 仅接收访问 token；重启恢复元数据，重新确认用途后读取凭据。验证与限制以 [Keychain OAuth 批次](validation/keychain-oauth-20261010.md) 为准。旧内存会话无法凭空恢复 refresh token，新版首次需重新登录。

- 最新优先级：直接复用 LingXi Electron Settings 的提供商列表、配置详情、Card/Row 样式和凭据事务逻辑，接入本项目的多提供商配置后端。设置页展示当前目录及真实来源/刷新状态，不在保存时选择模型；Tasks 区选择具体连接和模型。批次证据见 [multi-provider checkpoint](../artifacts/multi-provider-task-model-20261010/checkpoint.json)。原桌面外壳证据沿用 [UI checkpoint](../artifacts/desktop-electron-shell-20261010/checkpoint.json)。
- Main 多连接管理、明确的内存/Keychain 策略、用途同意、停止取消和账号过期接齐；模型选择前及任务创建时复验连接身份、用途、模型能力和实际 Host 授权。SDK 目录覆盖29个受支持的直接认证 profile，实时目录通过 llm-client 更新；不支持目录的 profile 明确展示 SDK 来源，不把列表展示当作真实供应商验收。云平台/Copilot 认证尚未接入此界面。
- OpenAI OAuth 回调 state 长度已对齐 SDK 的16字节随机值，本地 mock 回调通过。用户仍明确“稍后登录”，没有重试真实登录、读取现有密钥、真实模型调用或原生业务输入。Rust adapter、受影响 Host、严格 Clippy、生成一致性与原生构建已通过；TS workspace 549通过/1跳过，最后设置 UI 替换单独补定向检查、typecheck、构建与可见界面验证。具体耗时和适用源码以批次记录为准。
- 完整 MVP 与原100fixture+20TextEdit、§26/§28验收门保持开放，未提交/推送。以下 T 记录为历史上下文。

- 用户“继续”恢复并推进到 T，FORM-01..07 的本地能力已接齐，真实业务仍未验收。已异步询问应用内 OpenAI 重连是否完成；收到“已连接”前不读旧密钥、不运行真实 provider 或原生业务输入。
- 当前 [T checkpoint](../artifacts/mvp-scoped-number-2682cbf4-a021-4a6e-818b-3b498f199104/scoped-number-checkpoint.json) 继承 [S checkpoint](../artifacts/mvp-scoped-advanced-form-99f9bd60-4927-421a-902f-fcee48649e4d/scoped-advanced-form-checkpoint.json)。封存目录及 runner 不重跑，新结果进入新批次。
- FORM-07 保留 Alert threshold／提醒阈值、Advanced preferences／高级偏好设置和 Number desired。仅此可信注册 AXTextField 将实际规范十进制 Text 解析为安全整数；空白、指数、前导零、溢出与超出 ±(2^53−1) 拒绝。generic Text 与 Number 不隐式互换。
- 精确复用 scroll-preferences／scroll-preferences-zh。冻结 Number 计划是 scroll-before → 一次 ensure → 原 away/home revisit → scroll-after，可选末尾同目标 read。夹具每次页面重建 reset scroll zero，返回后必须重新滚动并取当前完整新 Source、可见字段与新绑定，才满足原 Revisited+ReadValueEquals；没有改变夹具或原 profile。注册容器只获得限定的 Navigation，输入/激活/许可映射前仍严格复验绑定、窗口、Source、祖先、原语与配额。Unknown 不重放。
- T 的全工作区分段覆盖 10 包、1462 项零失败；最后 Engine 643、numeric7、消费者51通过。复用 workspace02 已过的 IPC/supervisor/execution-core/contracts，补验 Engine、三个直接消费者、sim-world/source-macos 与全 doc tests；未把两个失败的 workspace run 改称成功。最终格式／Clippy／构建／契约与生成一致性通过。只在最后等价布尔及 test-only 注解后定向复验。独立评审无生产阻断。规划目录容量40→64修复42注册被截断，16KiB总预算不变。
- 本轮 R 的 FORM04、S 的 FORM06 与其他未变 Native/桌面证据按父 checkpoint 引用；零 provider 调用、零原生业务输入、正式业务成功仍0，MVP未完成。未改SDK/Harness／依赖pin，未提交、推送或重新签名发行。
- 下一本地缺口 LIST（优先 LIST01），只读分析已并行启动，尚未编辑。随后 MODAL/TextEdit。正常产品重连后先核实连接与当前二进制，再退出配置 Main，单 GUI lane 补验 SET/FORM。完整100fixture+20TextEdit、S1质量、Stop/故障/资源/恢复/发行/第二设备等原 §26/§28 门保持开放。

## 批次与并行

1. 用几行明确产品结果、共享接口、文件归属、验收命令。默认安排 3–4 个互不重叠的实现任务；无需为用满并发而拆碎工作。
2. 固定共享契约，再并行推进 core、desktop、driver。契约由一位 owner 生成 Rust/TS artifacts，避免相互覆盖。
3. worker 完成实现与聚焦回归，主代理及时整合。Rust 编译及依赖它的测试统一排队；独立脚本、类型检查和只读评审并行。
4. 相关修改收齐后，一个 reviewer 检查批次差异，再运行受影响的集成门。真实 UI 使用同一份已构建产物，运行中不再修改相关源码。
5. 交接只写完成行为、验证结果、未通过的门、下一依赖。一次通过且没有相关变更的检查无需重复。

## 验证矩阵

先执行能够发现本次错误的最小检查；聚焦结果仅证明其覆盖范围。无法判断依赖影响时扩大检查。

| 改动 | 开发时 | 整合批次后 |
| --- | --- | --- |
| 文档、流程说明 | 链接/命令核对，`git diff --check` | 无产品构建或 GUI 重跑 |
| MVP 脚本 | 对应 `.test.mjs` / `.test.mts`，使用 `--import tsx` | 公共运行器、目录或 ledger 变化时补消费者与对应集成 |
| 单个 Rust 模块 | 受影响 crate/测试过滤器；确认确实执行了预期测试 | 消费者回归及相关 Clippy |
| wire contract、数据库、跨模块生命周期 | 契约/迁移/边界反例及直接消费者 | 收齐修改后一次 Rust workspace、TS 消费者、类型与生成一致性检查 |
| Main/preload/renderer | 对应 desktop 文件测试与 typecheck | IPC/授权/任务流程变化时补一次受影响的普通产品 UI 路径 |
| 模型接线 | decision-worker 或 harness-adapter 的相关测试 | 生命周期/用途授权/取消回归；真实 provider 进入明确的模型验收批次 |
| Device/Source/Swift、权限或 Stop | 相关 Rust/Native 检查 | 对应原生输入/Stop/清理集成，不能只用单层 mock 代替 |
| SDK pin、toolchain、公共构建配置 | 来源一致性与适配层检查 | 全量集成，重新建立构建与真实运行证据的版本对应关系 |
| 包资源、签名或发行候选 | 相关包脚本测试 | 当前产物完整组装、签名和安装验收；旧包不能代表新源码 |

现有 `scripts/check-all.sh` 是广泛回归入口，保留其检查；它不作为每次小改动的默认循环，也不等于完整 MVP 验收。该脚本目前只跑 MVP 的 `business-results.test.mjs`，不能当作整个 MVP 脚本组通过。

在仓库根目录按需使用：

```sh
# 定向 desktop 示例；原生进程测试先准备匹配的二进制。
node --import tsx --test --test-concurrency=1 apps/desktop/test/ipc.test.ts
npm run typecheck --workspace @computer-use/desktop

# MVP 脚本组同时覆盖 mjs 和 mts；用于相关公共接口变化。
# continuity 测试还需要与当前源码匹配的 target/debug/compile_device_context。
node --import tsx --test scripts/mvp/*.test.mjs scripts/mvp/*.test.mts

# 整合后的 TS workspace 测试，前提是匹配的原生二进制已经准备好。
npm test --workspaces --if-present

# 跨契约批次的 Rust 全量门，不是每个 worker 的默认命令。
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run gen-types:check
```

## 构建、失败与证据复用

`artifacts/` 是本机验收证据目录，不纳入 Git；历史 checkpoint、日志和运行器链接需在原验收机器查看。可共享的最新摘要位于 `docs/validation/`。

- 延续已确认的 Cargo cache、离线 mirror、toolchain、feature/profile 配置。本机环境参考 [M validate.py](../artifacts/mvp-frozen-admission-5f575246-064c-438a-9212-f67033334d23/validate.py)；它会写入封存目录，**只读取配置，不直接运行**。新结果写入新批次目录。
- `npm test` 先运行 `build:native`，Rust supervisor 也可能请求构建。共享 target 只允许一个构建 owner，不并行运行 workspace/选定包构建。切换 feature/profile 可能改变二进制，真实运行前核对实际产物。
- 需要 socket、进程或 GUI 的测试直接使用允许这些本地能力的环境；不要再次尝试已知失败的沙箱路径。遇到磁盘不足先处理首个根因，不逐个重跑级联失败。
- 第一次失败先区分代码、环境、测试器原因。针对修复运行回归，再补受影响的门；同一原因复现两次就停止原命令的盲目重试。
- 相关源码及依赖、命令、toolchain、环境匹配时才沿用旧结果；不确定就重验。保留真实运行的前后产物及资源核对，不把旧结果标成新运行。
- 未变化的历史证据引用父 checkpoint 路径和 SHA，只新增本批次差异和结果。最终候选或消费者要求完整 manifest 时生成一次，保留核验语义和旧 checkpoint 格式。
- 在现有运行记录中记录命令、开始/结束时间、退出状态。观察下一批次的实际耗时，再讨论提速比例；不新增调度或缓存框架。

## 本次优化依据

M 中 workspace Rust 测试共启动 5 次，包含 socket 权限失败、Python mock 守卫启动失败及级联、契约样本计数不一致，最终通过。最终 Rust 日志编译时间为 36.26 秒，68 个 suite 报告耗时合计 82.40 秒；最终桌面测试报告 61.95 秒。这些局部时间不能当作整个任务的墙钟耗时。

首轮 MVP 脚本遗漏 `--import tsx`，目录 UI 首轮因结果汇总错误被标失败，修复后均通过。重点改进执行环境与入口预检、收齐修改再集成、沿用适用证据、减少重复扫描和封存。
