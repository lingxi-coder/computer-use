# S2 模型层接线与回合收尾

2026-10-02。路线 B 已实现；以下证据来自固定 SDK 的实际编解码器、注入运输层和真实 cu-host 启动检查。没有使用真实云端账户，也没有把登记参考 GUI 任务改记为模型任务。完整 MVP 范围继续由主设计 §26、§28 和 [交付跟踪](../mvp-status.md)管理。

## 固定依赖和调用

| 依赖 | 唯一源码与完整 revision |
| --- | --- |
| llm-runtime 及其 Harness 包 | `https://github.com/lingxi-coder/harness-runtime`，`4dfd9abef1dc293a9617d35387165cb2256b5d98` |
| lingxi-llm-client | `https://github.com/lingxi-coder/llm-client`，`0c6a907d897a54e656700c00cf335d91b10dca0b` |

Cargo.lock 只有一个 provider SDK 源身份。相邻工作目录和 SDK 仓库的未提交改动不参与构建。按主设计“无需会话能力时选 B”的规则，使用 ModelRuntime 的单次请求；未装配路线 A。资源和真实模型质量验收仍开放。

当前支持 Anthropic Messages、OpenAI Chat 和 OpenAI Responses；路由、endpoint、provider 身份、model 和输出上限均显式给定。请求只有一个强制选择的 `StructuredOutput` 函数，没有会话、工具循环、文件/截图附件或 provider-hosted tools。不同 provider 的 strict-function Schema 子集不能完整表达当前联合契约，因此 provider 工具声明使用普通函数 Schema，宿主同时执行完整 JSON Schema 和类型化反序列化检查。重复 JSON 字段也被拒绝。

调用走 SDK 的网络与流编解码，产品包装只设置配额：上下文 64 KiB、响应原始字节 1 MiB、单调用参数 256 KiB、最多 16 个工具身份、有界 36 项事件队列。文本和推理 delta 不算提交；未知工具没有执行入口。一次有效提交产生提议，零次为 missing，两次即 conflict。截断、未结束流、矛盾身份和额度超限隔离整个回合。请求不自动重试、重定向或使用环境 proxy。

## 宿主生命周期

2026-10-03：S1 教师已使用独立 `TeacherDecision` 函数、SDK profile 和适配器，经既有 `Input::Decision` 接纳；它不安装 S2 计划。私有 bootstrap 的 `purposes` 必须显式且非空、唯一，S1-only 不建立 S2 planner。教师存在时 Host 内部处理决策，不再向 Supervisor 发送同一教师请求，也不接纳该路返回的提议。取消、迟到、新 root、关闭与精确目的门经注入 SDK 运输层验证，真实 provider 仍未调用。

教师调用写入独立 `decision_model_calls`：Engine 必须仍持有逐字段一致的 pending request，事务开始记录成功后才打开 SDK；终结以原 root/decision/generation 和 open-row 条件更新，完成时间不得早于开始，再向 Engine 排队提议。已取消行不能由迟到结果覆盖，未知 token/费用保持 null。崩溃未收尾的行保持未知，重启不恢复教师 slot 或输入。相邻 S2 账本也改为完整 request 比对和同样的时间顺序约束。[当前检查](../../artifacts/m4-s1-teacher-ledger-checks.json)记录 Host 29、Engine 357 项测试和 Clippy，通过的 SDK 用量样本为 12/7 tokens、费用 null；这不是实际云端成本或模型质量报告。

cu-host 主循环继续运行同步 owner，网络在单独一个 Tokio worker 上等待。一个在途回合之外最多暂存一个规划请求；旧回合收尾前不能打开下一次调用。Engine 发出精确 request/generation 的取消，停止和暂停在本地队列中优先于已排队模型结果。适配器按 root/request/generation 隔离回合，模型不提供这些身份。

取消覆盖连接打开和响应流读取。先丢弃请求 future/响应流，再发收尾 barrier；取消、超时和迟到结果不会安装计划。授权撤销立即关闭调用准入，收尾后删除内存 key。host 退出也有有界收尾和落账窗口，退出过程不安装提议或推进设备动作。无法确认最终用量的崩溃记录保持未知。

`model_calls` 由 Deliberation 独占：网络开始前写入 root、request、generation 和显式非敏感路由身份；结束记录 proposal、missing、conflict、invalid、quarantined、cancelled、expired 或 unavailable，以及可用用量。收到 proposal 不代表 TaskRuntime 已安装或业务完成。该表没有上下文、参数、原始错误、密钥或原始响应；重复/外来终结不能覆盖记录。完整且有效的 SDK usage 才写 token 数；partial/missing 保留 null。费用仅在冻结价格与执行事实足够时产生 USD 微额估计，未知不写成零。

重启只恢复高于已记录调用的规划编号，不恢复在途请求、slot 或提议；未结束调用继续保留未知终态。真实产品历史与用户 revalidation 验收仍单独开放。

## 私有配置与产品边界

main 的 `SupervisorOptions.modelAuthorization` 是显式内存入口。密钥和路由走 host stdin 的独立有界前缀，`--model-bootstrap` 只声明前缀存在，参数与环境没有 key。它不是 desktop Message、Configure settings 或 Trace 数据。接收缓冲和内存 key 使用 Zeroizing；Debug 隐去 key。HostManaged CredentialProvider 同时检查 provider/profile/credential ID，拒绝后不回退 shell/keychain/file。

main 启动 host 时只继承必要的 PATH/TMPDIR/locale/timezone 变量，移除 HOME、provider 密钥、模型/endpoint override 与 proxy。没有匹配私有授权的 model backend 在连接来源/设备或创建 ledger 前退出。解析/传输错误使用固定文本，provider 返回的错误内容不进入日志。发送缓冲在 pipe write 完成后清零，启动失败和 shutdown 也清理保留项。

常规 Operator 已有[模型设置、原生出站确认与本产品 Keychain 保存](m4-model-authorization.md)，用唯一假密钥在真实 Electron 中验证；保存连接不恢复许可，有模型 key 不授予设备输入。原生业务 GUI 验收仍用登记参考后端。真实 S1 教师、真实 provider 任务、签名 Keychain、取消时延/资源与完整质量报告仍待交付。

## 证据和复跑

```sh
cargo test -p harness-adapter --test model_route --locked
cargo test -p desktop-host --locked
cargo test -p interaction-engine --locked
node --import tsx --test apps/desktop/test/modelBootstrap.test.ts
bash scripts/check-all.sh
```

[接线记录](../../artifacts/m4-model-runtime.json)记录构建哈希、固定源码身份、检查计数和证据边界。正例经过三种 SDK 编解码器；负例覆盖工具/Schema/身份/次数/配额/截断、连接与流中取消、超时和撤销。完整 host 流程检查模型 goal 等待确认、取消后新目标等待旧调用收尾、退出收尾落账；真实 cu-host 进程检查父环境有 key 时无授权仍拒绝，以及失败日志不回显测试 key。真实 IPC、I0 和原有原生回归单独保持。

SDK feature 合并启用 `serde_json/preserve_order`，曾使 I0 demo 的键顺序改变。本轮给 I0 与 desktop 导出显式稳定排序，保留 Schemars 的 keyword 顺序，并新增已发布 schema/fixtures 的逐字节断言；冻结文件没有为 SDK 变动而重写。回归过程还出现过 helper callback 与模拟 source ready 的启动超时，单独复跑通过，原因未确认；失败记录保留在 artifact，安全期限未放宽，共同负载和时延验收仍开放。

最终全量通过 566 Rust、103 TypeScript、159 Swift（55 source + 104 device），格式、Clippy、生成类型、真实 IPC 和生产构建均通过。最终构建的登记参考 GUI 任务复跑通过，已核对同一组二进制哈希；该任务仍明确记录 `live_models_verified=false`。
