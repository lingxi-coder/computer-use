# 产品模型配置、出站同意与保存密钥

2026-10-02。普通 Operator 已有模型配置表单、原生出站确认和可选 Keychain 保存。真实 Electron 43.1.1 的两次独立启动通过 58 项检查；仅使用脚本自有窗口、唯一假密钥和 `example.invalid`，没有创建模型任务或调用 provider。证据在[授权记录](../../artifacts/m4-model-authorization.json)。这项交付补齐产品授权入口，真实 S1/S2、签名安装、模型质量与完整 MVP 验收继续开放。

## 使用与权限

Operator 的模型面板填写服务、协议、HTTPS endpoint、model identifier 和 API key，并明确选择 S2 planning、S1 teacher 或两者。默认只勾选 S2，S1 须另行选择。支持已接线的 Anthropic Messages、OpenAI Chat/Responses 及显式命名的兼容或自定义服务。型号必须由用户填写；授权成功表示允许选定目的并已启动客户端，不表示账号、网络或模型质量已验证。只有 S2 目的启用模型规划，S1-only 可建立教师服务而不启用 S2。

提交后清空表单中的 key，main 显示原生 Cancel/Allow selected purposes 确认框，默认与取消按钮都是 Cancel。框内显示实际 endpoint/model 和精确选定目的，按目的说明规划或教师上下文的出站范围；采集截图、secure 控件值和连接 key 不进入模型上下文，key 单独用于服务认证。选择保存时，同一确认框说明 Keychain 持久保存和再次连接仍需同意。窗口输入授权、目标确认和 Execute 继续各自成立。

启动只加载公开的已保存路由，不读取其 key，也不恢复模型许可或目的。Use saved key 使用当前明确选择的目的，先显示原生出站确认，允许后才读 Keychain。Revoke connection 关闭准入、取消任务并关闭旧 host；Forget 再删除精确保存项。普通 Stop 取消当前任务；在等待模型确认或切换连接时，Stop 同时取消该连接操作。取消后的迟到 Allow 不能恢复许可。

## owner 和会话替换

renderer/preload 只提供受限的模型配置调用；main 的 ModelAccess 独占许可、内存连接和保存操作。严格 IPC 检查来源、字段、HTTPS 路由和保存 ID，拒绝传入路径、额外密钥字段或任意 Keychain 服务。key 不变成 OperatorCommand、desktop Message、投影、Configure 或 Trace。

模型/窗口配置互斥；活动任务、held/unknown/failed 清理状态或另一确认会阻止新连接。旧 host/device/source 关闭后才启动新管线，始终回到 observe，清除旧窗口 grant 和任务，不恢复输入。进程退出、停止、撤销和产品退出均有 generation 隔离。旧设备的清理不确定性保留到新管线，不能被新管线的 `not_needed` 覆盖；执行和再次授权继续关闭。

私有 main→host 有界 stdin 前缀沿用[固定 SDK 模型层](m4-model-runtime.md)，host 不从环境、文件或别的应用 Keychain 寻找凭据。main 保留当前连接所需引用，关闭旧 host 后释放旧 key 引用；可变发送缓冲被清零。JavaScript/Swift 的不可变字符串不提供原地擦除保证。原生对话框失败、Keychain 保存失败或替换失败使用固定错误文本，关闭新连接后再报告失败。

Environment 给目标编译和重规划提供静态 `registered_app_catalog`：最多 16 个 pack，每个最多 16 个控件和 16 条效果规则，总计最多 16 KiB。它没有实时控件值、截图、PID/window ID、绑定或执行能力。模型由此取得已登记的 app/semantic target/role，输出仍只是提议；效果等级、绑定、GoalSpec 与输入接纳由原 owner 验证。

## Keychain 范围与限制

异步原生 helper 使用固定服务 `com.lingxi.computer-user.model-keys` 和 main 计算的 64 位小写 SHA-256 account；API key 只经过有界 stdin/stdout，不放入 argv 或环境。查询明确限定 generic-password、精确 service/account、单条结果、不同步，并禁止交互认证。表单操作无读取既有外部应用凭据的入口。

`userData/model-access/connections.json` 只保存最多 16 项公开路由和 ID，原子替换、文件权限 0600；拒绝符号链接、超限、伪造 ID、未知字段和无效路由。读 key 后再次比对刚确认的完整路由，变化时拒绝继续。保存/删除串行写元数据；撤销先关闭管线，再等待已取消连接的保存收尾，Forget 随后删除，避免迟到的保存把已删除密钥写回来。Keychain 保存成功而新元数据写入失败，会回收本次新账号；保存失败不会留下已授权的新 host。

当前 helper 使用 macOS 默认的文件 Keychain。服务/account 的精确范围不是签名 access-group 隔离；Data Protection Keychain、签名身份稳定、锁定与授权矩阵属于 M5 打包验收。Apple 的[macOS Keychain 技术说明](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)区分这两类 Keychain；[密码条目文档](https://developer.apple.com/documentation/security/adding-a-password-to-the-keychain)说明 SecItem 的存取方式。锁定、不可访问或 helper 缺失时可使用已明确同意的内存连接，产品不自动回退到别的凭据。

## 验证与边界

2026-10-03 的[目的授权现场检查](../../artifacts/m4-model-purpose-live-index.json)使用最终账本 Host 二进制，实际两次 Electron 启动通过 93 项检查和九个原生确认框。覆盖 S1-only 不启用规划、两目的同时选定、逐次撤销、保存后重启不恢复目的及再次确认；12 次 Host 账本检查均为 S1/S2 零调用，唯一假密钥账号删除已确认。当前二进制、runner 和 manual smoke 的运行前后 hash 一致，历史 58 项记录仍保留。沙箱内启动/Keychain 不可用的失败单独记录，runner 已修复清理错误导致原始失败产物无法保存的问题。真实账号、provider 调用、签名 Keychain 和业务任务仍未验收。

```sh
bash scripts/check-all.sh
npm run smoke:macos-model-authorization -- --runtime /absolute/path/to/Electron
npm run smoke:macos-operator-authorization -- --runtime /absolute/path/to/Electron --execute
```

原生 Keychain 自检在独立 `.self-test` 服务中验证 7 项，并删除自己唯一的临时账号。真实产品 GUI 在另一个唯一账号中验证保存、退出后重连和 Forget，失败也执行精确账号回收。两次启动的实际 NSAlert、58 项检查、每个旧/当前 host 的零调用账本、窄窗口无横向溢出和无 renderer/preload/CSP 错误均记录到 artifact。截图只来自测试窗口；没有提供真实 API key，也没有消费云端额度。

全量复验曾重现来源 helper 的 5 秒权限/输出测试超时、3 秒 watch callback 超时及批量 device ready 超时；相应同一特征图二进制的单项/来源整组复跑通过，未改 helper 或进程启动期限。诊断期间观察到较高系统负载，但启动超时的因果关系尚未确认；[失败与诊断记录](../../artifacts/m4-model-authorization-diagnostics.json)保留，不以一次通过代替共同负载与启动时延验收。

另一个 500 ms 图像关闭检查暴露了可复现的实现边界：整幅 8 MiB checksum 校验后才首次检查取消，探针测得约 360 ms，超过 250 ms 发送预算。现在容器与每 64 KiB checksum 块都检查取消/截止时间，取消时不发布包前缀；40 ms 请求取消的五次探针收尾为 40–42 ms。原预算、500 ms 断言、字节配额和 wire 都保留，新增中途取消回归及原来源检查通过；这五次探针不构成停止性能报告。

最终 `RUST_TEST_THREADS=1 CARGO_NET_OFFLINE=true bash scripts/check-all.sh` 通过 569 Rust、121 TypeScript、166 Swift 自检（55 source + 104 device + 7 Keychain），格式、Clippy、生成类型、真实 IPC、类型检查和生产构建均通过。Rust 测试运行器为串行，内部实际 IPC/worker 并发仍按各检查执行；默认并发运行的启动失败单独保留。

最终同组二进制上的模型授权 58 项与原生编辑 GUI 复验均通过，实际原生任务完成、一次 setter 和两次导航已核对。一次前序 GUI 复验在来源 revalidation/refresh 失效且输入结果未解决时暂停根，报告 stopped_confirmed/not_needed，没有错误报告完成；失败产物也保留在诊断记录。后一次通过不能消除该项负载/来源稳定性问题，完整任务集仍须计入所有失败与暂停。

授权测试不代替真实 provider 规划、教师决策、费用/延迟或模型质量。普通启动仍需显式配置原生观察目标；应用选择与持久产品历史尚未完成。全局物理热键、held 原语真实释放和完整停止矩阵按 M2 门单独验收。[完整 MVP 跟踪](../mvp-status.md)的范围不变。
