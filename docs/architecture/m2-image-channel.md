# M2 图像通道与只读预览

2026-10-01。原生 SCStream 图像已通过独立二进制通道到达 Electron 只读预览；原生输入仍 unavailable。

## 数据路径

```text
Swift SCStream → 私有 PNG → cu-source watch worker
  → 有界 ImageServer → 私有 images.sock → Node imageWorker
  → main 最新图像缓存 → 受信 image(frame_id) API → renderer Blob PNG
```

控制与停止仍走原有 JSON 信封和独立 device 路径。PNG 编码、容器/校验和验证、socket 发送和消费端解析都不在 source 控制循环或 Electron main 执行；main 仅核对小型 metadata，并在受信取图调用中复制已验证字节。

`cu-source` 的 watch mailbox 持有原 PNG 字节与原 FrameMeta，图像发送端只保留一个待发帧和一个发送中帧，替换待发帧不改动采集时间。Node worker 对每个已传给 main 的缓冲等待 ACK，期间只保留最新候选；缓冲通过 ArrayBuffer 转移。main 保留一张最新图像，调用者收到自己的字节副本。renderer 按图像身份取图，心跳仅续期显示；失效、换帧、卸载时回收 Blob URL，不把图像抵达当作新像素证据。

## 协议与限额

会话目录为 0700，socket 为 0600。Supervisor 通过继承管道给 source 一个独立的 32 字节十六进制凭据；它与 host/source 控制通道凭据不同，不进入 renderer 或日志。数据通道固定认证前缀为 `CUIAUTH1`、32 个 ASCII token 字节、big-endian u64 session epoch；成功回 `CUIREADY`。每个 source 生命周期只接纳一个成功认证的 supervisor 消费连接。它不提供对同一用户的恶意进程的沙箱保证。

数据包为：

| 字段 | 编码 |
|---|---|
| magic / version / format | 8 字节 `CUIMG001`；本版固定 PNG |
| metadata 长度 | big-endian u32，2–65536 字节 |
| PNG 长度 | big-endian u32，57–12582912 字节 |
| metadata | 已发布 desktop v1 `FrameMeta` 的 UTF-8 JSON |
| 图像 | PNG 原字节，长度和 SHA-256 必须与 metadata 相等 |

固定头先校验，再分配有界 metadata；metadata 校验后才分配像素载荷。限额为每边不超过 4096 像素，总像素不超过 4194304（RGBA 像素解码结果至多 16 MiB）；仅接纳单张、非交错、8-bit RGB/RGBA PNG，拒绝 APNG、多图、尺寸不符、截断与校验和不符。接收端每秒最多八张，session/PID、stream 退休、sequence 和原始 display 时间重放均检查。

认证与单张发送使用 250 ms 的总预算；消费者半包总预算为两秒，持续滴流不续期。慢消费者或坏包只关闭本次图像通道；不阻塞控制/停止，不重启采集或扩大权限。数据通道关闭后，本阶段需重建 Supervisor 会话才能重新连接。

Rust 与 TypeScript 共同读取 [golden packet](../../contracts/desktop-v1/image-channel/packet.bin)、[metadata](../../contracts/desktop-v1/image-channel/metadata.json) 和生成的 [PNG](../../contracts/desktop-v1/image-channel/fixture.png)。业务控制 Schema 和 I0 协议未增加图像字节字段。

## 展示依据

source 同时向 main 转发 FrameMeta 和健康 metadata。取图必须匹配原 image ID、stream、session、surface、clock、原始采集时间、SHA-256、窗口和像素几何，且 host 的 FrameView 仍确认有效。idle 可以续期同一图像的回调确认，不改变原采集时间；host 投影或回调超过两秒后禁止取图，renderer 的显示期限也独立到期。

AX/OCR 降级继续显示具体原因。已有图像的只读展示由 screenshot 来源与引擎帧有效性控制；AX 的执行绑定/许可要求由 StateCore 独立检查。复验发现的 AXObserver 不支持销毁通知（-25207）会令 AX 降级，但不应丢弃已验证的只读图像。

本轮还修复 Retina 坐标校验：`SCStreamFrameInfo.contentRect` 使用 surface points，要乘 display `scaleFactor` 后与像素缓冲比较。仍拒绝不能确认的 padding/cropping，不猜测 OCR 映射。[Apple 的 metadata 说明](https://developer.apple.com/videos/play/wwdc2022/10155/)与本机 SDK 注释支持此换算。

## 验证边界

[真实源进程记录](../../artifacts/m2-native-observation.json)覆盖自有 AppKit 窗口的 AX、SCStream、OCR、原 idle 时间、二进制接收/校验和、冻结后的取图拒绝和 helper 回收。[Electron 记录](../../artifacts/m2-electron-runtime.json)记录生产和开发模式的只读 PNG 解码、尺寸、来源降级提示、关闭重开和停止入口。所有图像均来自自有夹具，未向用户应用输入。

这仍不是完整 M2：在线神经感知、原生命中/人工输入/停止看门狗/原子输入、复杂权限/坐标/睡眠矩阵和签名安装包还需独立完成。
