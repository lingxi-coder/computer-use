# 自有临时 TextEdit 文档启动

`apps/desktop/src/main/ownedDocumentLaunch.ts` 提供 Main 内部的实际启动器。Main 创建新的 0700 私有目录和 0600 `document.txt`，保留 UTF-8、多行、tab 和 CR 内容；拒绝 NUL 或超过 4096 UTF-8 bytes 的初始文本。调用方只取得不包含身份字段的 `OwnedDocumentRegistration`，可以停止该登记；它不接受现有用户文档路径或目标 PID。

Main 从固定产品路径启动 `cu-macos-owned-document-launcher`，通过该子进程自己的 stdin/stdout 通道核对 launcher PID、父进程、可执行路径和启动回执。原生 helper 固定使用系统 TextEdit，要求新应用实例、不激活、不加入最近使用列表、忽略持久状态，并比较启动前的进程集合。回执还需匹配新 PID、内核启动时间、TextEdit 路径、launchd 父进程、请求时间、私有文件 inode 和初始 hash。它不授予 AX 或输入权限。

首次写入 launch 命令之前，Main 就进入结果可能未知的阶段。写回调失败、超时或回执不符不能推断为未启动，不能重发 launch。停止由 helper 对自己登记的实例核对内核身份后发起；Main 只在已验证回执的 PID、停止结果、helper 退出及精确文件清理均通过时释放登记。不能确认时保留 Unknown。

清理比较创建时记录的目录和文件 dev/ino、owner、类型、权限及 canonical path，拒绝 symlink、额外子项或替换后的 inode。原 inode 内容因编辑而改变不阻止确认停止后的清理；atomic-save 换成的新 inode 需要另行登记，当前保留为 Unknown。创建失败也使用精确回滚，未能安全移除的部分目录保留在 Main 内部账中。文件系统检查建立在私有目录和自有进程已停止的前提上。

`npm run build:macos-owned-document` 严格编译 helper；`npm run smoke:macos-owned-document` 使用产品启动器创建一份新的中英临时文件并停止，只观察文件和进程，不发送 AX/键盘/鼠标输入。每次现场运行写入独立 artifact，保留失败和清理状态。

[现场索引](../../artifacts/m2-owned-document-launch-live-index.json)记录八项 Main 聚焦检查及严格 Node/Swift 编译通过。实际单实例启动、停止与私有文件清理通过；`npm run smoke:macos-owned-document -- --preexisting-owned` 还实际创建一个自有基线实例，证明第二个实例停止时基线保持，随后清理两者。首次现场运行发现 Foundation 与 Node 对 `/private/var` 的 canonical 表示不同，helper 已改用 POSIX realpath，原失败记录保留。这里验证的是私有文件 bytes 和进程生命周期。

Main 已支持只向其当前 opaque registration 对应的 Source 子进程发送有界私有 bootstrap；它核对新实例的内核启动时间、私有文件 hash/dev/ino 和目录身份，不经过 renderer、settings 或模型消息。Source 与原生 helper 分别验证 prefix，并将每次专用 AX 读取绑定到实际 stream/snapshot/sequence 和有效期。通用 Structure 仍为 incomplete，独立 owned scope 证据不提高通用 observer 健康，也不授予输入权限。

独立复审修正了返回时使用真实 registry boundary 的门，并将文本 subrole Unsupported 特例限制到实际 receiver。[首次只读 Source 运行](../../artifacts/m2-owned-document-source-24eb14c8-11f1-4e6b-a996-fdd1b203beed.json)确认真实 AXDocument 与登记文件一致，但 receiver 完整性校验拒绝发布证据，停止时还触发信号回调 actor 隔离异常。显式 Sendable 回调修复后，[第二次运行](../../artifacts/m2-owned-document-source-15b54a35-73a5-4a4e-ad55-b23c48ef0966.json)正常退出并停止清理自有文档，receiver 完整性门仍拒绝证据；两次均零输入。这些失败记录不证明可用的 owned binding。

Operator 任务流程、Binding/Device、编辑、保存和重开验收仍待完成。生产键盘保持关闭，MVP 未完成。

进一步的[有界诊断](../../artifacts/m2-owned-document-source-b50fc1bd-3897-44ae-a762-8027a41e2446.json)确认只有 registration 校验失败；Foundation 标准化将 POSIX `/private/var` 路径改写为 `/var`。纯登记门改为验证规范绝对路径语法，实际 POSIX realpath、hash/dev/ino 检查仍由 bootstrap 执行。新增路径回归后，严格编译与完整 Source 171 项通过。[修正后的真实读取](../../artifacts/m2-owned-document-source-2a4c9a20-c91a-44f9-9ede-8a291a9a6cd4.json)在一次启动期拒绝后取得三次连续 eligible sidecar，receiver reference、namespace、generation 保持不变；通用完整性仍 false。Source 正常退出、自有 TextEdit 清理完成、零输入。这里只证明独立只读 receiver scope；[现场索引](../../artifacts/m2-owned-document-source-live-index.json)保留全部三次失败及成功记录。

[完整 Rust IPC 运行](../../artifacts/m3-owned-document-rust-source-live-index.json)使用 Main 的 opaque token、私有 prefix、Desktop Configure 及 authenticated Host socket，取得三个稳定 mapped `StructureSnapshot`；generic incomplete/degraded 保持，receiver/namespace/generation/process 身份稳定，provenance、document 与 marked-range 时间关系和有效期通过。Source 正常 Shutdown，自有 TextEdit 和私有目录已移除，原有 TextEdit 进程集合保持。首次脚本重复清理的 false failure 保留，修正后的运行完整通过；独立 post-hash audit 确认实际二进制/API/脚本与记录一致。可用 `npm run smoke:macos-owned-document-source` 和 `npm run smoke:macos-owned-document-rust-source` 重跑只读验收。[检查汇总](../../artifacts/m2-owned-document-private-pipeline-checks.json)不声称 Host/Engine/Device 的输入授权、编辑或保存能力已验收。

Main → Host → Engine 的可信只读绑定已完成，Main 向 Device、Source、Host 各自发放读取前新生成的私有 prefix。共享登记保持精确 14 字段、有界与脱敏；Host 和 Device 在 payload 到达后检查新鲜度。Engine 独立维护 owned scope，不提升 generic complete、对象或 observer 健康；拒绝过期、重放、generation 回退及已退役 namespace，并在 refresh、身份变化、停止和恢复时清空当前绑定。Device 只保留观察上下文，输入、hit-test 和 human-input 能力仍不可用。

[当前检查](../../artifacts/m3-owned-document-readonly-binding-checks.json)记录 Host 37、Engine 366、Source 62、Device 49 项及 Main 21 项通过；相关 Clippy、格式和类型检查通过。Device 首次沙箱 CLI 测试因本地 Unix socket EPERM 失败，授权环境中的离线全量重跑通过。[当前三进程现场索引](../../artifacts/m3-owned-document-host-live-index.json)保留两次成功运行；最新运行取得三次 current readonly binding，私有登记未进入公开投影，mode=observe，attempts/root/S1/S2 账本均为零，自有实例清理完成，源码与二进制 hash 前后相同。执行授权、实际文本编辑、保存和重开仍待实现与验收，MVP 未完成。

2026-10-04：自有 receiver 新增有界 AXValue 只读摘要。已有 profile gate 通过后才读取同一 CF receiver；先检查 CFString 长度，再按完整 UTF-8 计算 SHA-256 和字节数，超过 2048 bytes 不截断，未读到值保持 None，空值与缺失值分开。读后再次校验 process、AXDocument、窗口、focus、marked range、settable/enabled、registry/observer 和期限。Rust Source 在原始 read start/finish 与回调时间中验证读回时间后映射，Engine 仅在当前私有 binding 内保留该证据。正文及摘要都不进入公开投影或现场 artifact，Host 只输出摘要匹配布尔值与长度。

[当前读回检查](../../artifacts/m3-owned-document-value-readback-checks.json)记录 Engine 369、Host 37、Source 64、Device 49、owned 契约 8、desktop 契约 4、Main 21、desktop-api 12、完整原生 Source 173 项，以及严格编译、Clippy、格式、类型/schema 和工作区编译通过。[最新现场运行](../../artifacts/m3-owned-document-host-5c02c0d6-d0cd-499e-9517-59dc0c50dbcb.json)三次取得同一自有 TextEdit 正文的精确摘要及 61 UTF-8 bytes（含中文、Emoji、组合字符与 tab），与初始私有文件匹配；observe、零输入、四个账本均为零、清理完成、hash 前后相同。它证明初始 UI 文本实际可读，尚未写入文本，也未形成输入后的 EffectResult；受控写入、保存和重开继续开放，MVP 未完成。可运行 `npm run smoke:macos-owned-document-value` 重跑。
