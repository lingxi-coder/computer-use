# M2 原生观察与持续来源

2026-10-01（续接 2026-09-30）。M1 的八项独立审查发现已修复；M2 已实现原生观察链路，已接入持续来源、证据失效、二进制 PNG 通道与桌面只读预览。完整 M2 出口仍须独立验收。

## 运行

持续 watch 需要 macOS 14.2 及以上、Apple Silicon，使用已安装 Xcode 的 Swift 6 工具链；权限预检和诊断 probe 支持 macOS 14：

```sh
npm run build:native
npm run build:macos-source
npm run check:macos-source
printf '%s\n' '{"protocol_version":1,"command":"permissions"}' | target/debug/cu-macos-source
CU_SOURCE_BACKEND=native CU_TARGET_BUNDLE_ID=com.apple.TextEdit npm run dev --workspace @computer-use/desktop
```

目标应用须已运行。`CU_TARGET_PID` 可限制同 bundle 的一个实例；`CU_NATIVE_SOURCE_HELPER` 可指定绝对 helper 路径。无 native 配置时继续使用原有 fixture。启动前验证配置；来源失败不回退到 fixture。

启动桌面 UI 还需要 Electron 运行时；README 中跳过 Electron 二进制的检查安装不能直接启动 UI。独立原生观察 smoke 不依赖 Electron：

```sh
node --import tsx scripts/smoke-macos-source.mjs
```

该脚本编译并短暂显示项目专有测试窗口，验证后关闭窗口及三个进程。它检查真实 SCStream Complete、PNG、OCR、后续 idle 和 host 投影，再暂停本次启动的 helper，检查期限失效、可见原因及 helper 回收。需要已有 AX 与屏幕录制权限，缺少权限时退出，不申请授权。[本机观察证据](../../artifacts/m2-native-observation.json)只覆盖当前主机上的自建窗口；没有向用户应用注入输入。

权限预检仅调用被动查询，不弹出授权窗口或更改 TCC 设置。权限不足、目标不唯一、AX 读取不完整均有明确健康状态；活动层不明确时执行门禁关闭，已选窗口的只读观察可以继续。原生设备清单为 `backend=macos`，`input`、`human_input`、`hit_test` 为 `unavailable`；即使操作员切换 execute，也不会提交实际输入。

## 实现边界

- `cu-source` 默认启动持续 `watch` helper，独立有界 worker 处理 NDJSON，监督与停止通道持续可用。超时、异常退出、回放、序列缺口或时钟跳变时终止并回收 helper；终止后仅显式 Refresh 可重建来源。诊断单次捕获保留为 `native_capture_mode=probe`。
- AX 专用线程和 CFRunLoop 独占 AX 引用与 AXObserver；节点、深度、消息超时、注册和整轮期限均有界。敏感字段不读值；不能确定安全分类的文本输入字段按敏感字段处理。
- AX 的 CFEqual 注册表提供有界实例连续性。销毁、布局、焦点、窗口或几何变化会推进 generation；通知覆盖不足时明确降级，退回每次读取的新实例 ID，不宣称跨读取绑定可靠。
- `StructureSnapshot.identity_boundary` 标识真实 AX 注册表身份，由注册表 UUID namespace 与正 generation 组成；它表示跨读取的身份边界，不是 `source_seq` 或树修订号。完整的原生持续 `WATCH` observation 必须携带该边界；单次观察、合成快照及身份不完整的读取可以为 `None`。边界变化使旧身份依据失效。该字段接线尚无新的真实应用观察验收。
- ScreenCaptureKit SCStream 限定目标 PID 和窗口。回调只复制有界像素与 SDK 元数据到有限队列，AX、OCR、编码和输出在其后处理。AX 不完整、含敏感字段或所选窗口的隐私/几何上下文变化时不保存图像或 OCR。只读 watch 可以观察已明确选定的后台窗口；活动层保持真实状态，不能借后台像素获得输入许可。
- Complete 的图像 ID、校验和、显示时间和几何作为像素事实保留；idle 只确认同一图像仍被流报告，不保存新图片，不更新像素采集时间。Blank、Suspended、Stopped、缺流、丢弃和上下文变化撤销旧依据。
- Vision OCR 将归一化框映射到全局显示点，输出数量、文本长度、像素和 JSON 大小有界。
- PNG 和 OCR/来源元数据保存在私有目录；每次 helper 最多保留八份图像，重启与退出清理旧目录。Manifest 原子更新，分别标出历史 Complete、最近像素观察和当前仍有效的图像身份。
- AX `StructureSnapshot`、`FrameMeta` 与有来源 stamp 的健康事件进入 StateCore。不同 session、退休 stream、乱序、未来时间、非法几何或失效前的迟到 Complete 不能恢复旧依据。后台帧可以展示，执行仍须帧窗口与真实活动层一致。许可期限同时受 AX 年龄、像素年龄、真实回调活性和 attempt/root deadline 约束，迟到 Observe/Grant 重新检查当前依据与原期限。引擎在外部事件与 Tick 检查期限；失效会撤销绑定、旧决策和提交前 dispatch，已提交的不确定结果仍保留 unknown。
- Operator 展示目标应用/窗口、来源状态及原因、帧状态/尺寸、像素年龄和回调确认年龄。原生输入不可用时显示只读状态。PNG 经独立有界二进制通道到达只读预览，OCR 仍以私有 manifest 输出；在线感知融合尚未实现。[图像通道与限额](m2-image-channel.md)说明数据路径、所有权和失效语义。

当前来源与设备声明 `macos/{session_epoch}/wall` 时钟域，时间基准仍是共享 wall offset。流记录保留原始 mach display ticks、换算后的 uptime、回调 uptime 与回调 wall 时间；用原始回调减去实际帧年龄映射像素时间。AX 按读取开始时间计龄。消息到达仅用于监督超时，不更新采集或确认时间；wall 与 uptime 的差值变化超过容限时关闭来源。尚未提供生产用 boot identity、分布式时钟映射或完整睡眠语义保证。

## 验证与后续

`check-all.sh` 在 macOS 增加 Swift 6 编译和基于生成图片的真实 Vision 测试；其余 Rust/TS 测试继续涵盖原有模拟闭环。原生源码的离线测试不等于桌面捕获验收。真实观察 smoke 使用项目独立测试窗口，记录 AX、截图和 OCR 结果后关闭，不操作用户应用。

Electron 43.1.1 的生产/开发窗口，以及原生来源降级时的只读展示和停止入口，已通过本机 smoke。此前锁屏时来源正确拒绝；恢复可观察会话后，健康原生帧与二进制 PNG 预览已通过生产和开发 UI 验收，包含 AX 降级提示和零 renderer 错误。历史失败与最新成功均保留在 [Electron 运行记录](../../artifacts/m2-electron-runtime.json)。

后续[被动设备守卫](m2-device-guards.md)已交付人工输入监听、完整 fp1 点位命中诊断、双租约和冻结/死亡回收，且在自有夹具验证。完整 BindingProof 已接入 Rust 执行核心和[原生只读权限探测](m2-native-authority.md)；完整 M2 仍需原子输入、神经感知，以及权限撤销、复杂坐标、睡眠和多显示器的真实环境矩阵。MediaPipe 与真实输入未标为 ready；M0 的签名权限、硬件热键和事件标记传播仍须分别核实。

相关 API：[SCStreamFrameInfo](https://developer.apple.com/documentation/screencapturekit/scstreamframeinfo)、[ScreenCaptureKit captureImage](https://developer.apple.com/documentation/screencapturekit/scscreenshotmanager/captureimage(contentfilter:configuration:completionhandler:))、[AX 消息超时](https://developer.apple.com/documentation/applicationservices/1459345-axuielementsetmessagingtimeout)、[AXObserver RunLoop](https://developer.apple.com/documentation/applicationservices/1462089-axobservergetrunloopsource)、[Vision OCR](https://developer.apple.com/documentation/vision/vnrecognizetextrequest)。
