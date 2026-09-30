# ADR-0001：冻结 I0、独立的 desktop v1 命名空间，与 execution-core 重构

- 状态：已采纳，已实现（M1 阶段 S1–S3）
- 相关章节：主设计 §14.8、§26.10、§27.4；来源记录 `contracts/i0-v1/PROVENANCE.md`

## 背景

外部 I0 原型是确定性内存模拟器，有 36 项测试、golden fixtures 和一个 demo 快照。设计要求 I0 的序列化字段、fixtures 和时间语义不变（§27.4），同时要求生产用的执行状态机被 I0 的回归直接约束（§3.4：模拟器和 cu-device 运行同一套状态机）。桌面产品又需要 I0 没有的东西：根 epoch、停止锁存与 `acked_stop_seq`、效果等级、绑定证明、带修饰标志的原子原语、AX 动作。

## 决定

1. **两个协议，两个命名空间。** `interaction-contracts::i0` 原样保留 I0 wire v1（逐字节来自来源文件）；新消息放在 `interaction-contracts::desktop` v1，不通过修改旧 fixture 掩盖不兼容。两者的 JSON Schema 与 golden fixtures 分别在 `contracts/i0-v1/` 与 `contracts/desktop-v1/`。
2. **原语是 I0 的超集。** desktop 原语包含原子点击、带修饰标志的按键、文本、滚动、AX 动作和窗口激活；按住类原语仍在，但生产配置 `PrimitivePolicy::AtomicOnly` 拒绝它们（§5.3、§14.6：按住类原语需要崩溃清理机制之后才启用）。I0 模拟器使用 `AllowHeld`。
3. **执行监督逻辑分离为 `ExecutionCore<B: InputBackend>`。** 它不含 IO、时钟和线程：时间由调用方传入（`advance_to`），输入后端是 trait，事件通过 `drain_events` 取出。`Simulator` 只剩一个薄外观：保留 I0 的方法签名，做 I0 类型与 desktop 类型之间的转换。
4. **设备层标识符用普通 `String`，设备以上用校验过的新类型。** 设备必须能对格式错误的请求回一个带原因的 receipt，而不是在解码时失败；越往上越需要类型保证。

## 验收与证据

- `simulator_contract`（20 项）与 `executor_regressions`（6 项）除 import 外一字未改，在 `Simulator` 外观上全部通过；8 项 protocol 单元测试与 2 项 wire 测试同样通过——合计仍是原 36 项。
- `i0-demo` 的输出与 `contracts/i0-v1/i0-demo.golden.json` 逐字节一致（`demo_golden` 测试）。
- `ExecutionCore` 另有 22 项 desktop 执行测试，覆盖停止锁存、根 epoch、许可状态、命中测试与原子策略。

## 后果

- 两套类型之间有转换代码，外观若漂移会被那 26 项测试发现；它们是这条边界的守门人，不能为通过而改断言。
- I0 文件冻结：改动 `i0.rs`、schema、fixtures 或 golden 需要新的 ADR。
- 设备层用 `String` 意味着设备端要自己做非空与长度检查（`validate()`），而不是依赖类型。
