# M0 S1 离线语义质量实验

本实验是 [完整设计 §8.6](../design/complete-design.md#sec08) 中 M0 离线决策质量工作的第一段。它只检查当前固定版本的本地 Laya choice 后端与 20 个手工夹具，不接管产品决策、不改变线上配置，也不能单独证明 M0、S1 或产品质量已经验收。

## 冻结范围

语料在 `scripts/mvp/s1-quality-corpus.mjs` 中维护为 20 个实际 `DecisionRequest`：导航、同名消歧、恢复分支和中英混合标签各 5 个。每个夹具包含当前 `DecisionState`、两个 executable candidates 和一个 choice question。数据按正式 `@computer-use/desktop-api` schema 构造和校验。

`model_cases` 与 `reference_labels` 是不同字段。模型请求只含通用 fixture 身份、状态、候选与问题；不含夹具编号、人工期望答案或理由。`projectLayaFeatureInput` 只投影状态与问题；评测标签直到后端调用结束后才与结果关联。模型所需的 `root_task_id` 是所有夹具共用的 `fixture-root`，不是产品任务 ID。

每例依序尝试 canonical、reverse、左旋一位和左右半区旋转；完全相同的顺序只保留第一次。当前 20 个问题都是两个选项，因此每例固定有 canonical 和 reverse 两个顺序，共 40 个计划样本。没有 warm-up；tokenizer 预检从不计作模型调用或样本推理。

本轮唯一执行的模型臂是固定 revision `e4e9ddf21a7b1903b7acffd8814ad4307bf63a67` 的本地 multilingual base ONNX 包，ORT Node 固定为 1.30.0。manifest 固定 SHA-256，并由生产后端在建 session 时验证其包文件。当前 manifest 没有精度字段；因此 runner 将其记录为本地 FP32 baseline candidate，同时明确说明 manifest 本身没有独立证明 FP32 dtype。云端 teacher、fine-tuned Laya 和 INT8 都保留为 `not_run`，不访问 provider、不转换权重，也不以其它模型替代。

## 输入路径和记录方式

预检读取同一个固定 tokenizer，并直接复用 `serializeState` 与 `buildSequence`，使用后端的 32 KiB 状态/问题上限、512 输入 token、192 head token 和 option marker 规则。每个样本记录请求和 feature SHA-256、字节长度、token 长度、截断数、markers、option 区分状态和拒绝原因。超预算或 preflight 不可用的样本仍留在 plan 与结果中；后端返回拒绝、截断、错误或没有启动时都保留原始行。

真实推理调用 `LayaDecisionBackend.create/decide`，只运行一次 encoder 和一次 head。runtime wrapper 在 `head.run` 返回时复制同一份 `logits` 与 `act_logits`，然后原样返回张量给生产后端继续处理。概率由这些捕获的 logits 通过相同的 vendored `softmax` 计算；不会再次调用模型。报告同时保留后端 proposal、失败码、真实 graph 调用数、每图耗时、wall time、raw logits、概率和拒绝状态。只有观察到各一次真实 encoder 与 head 调用并捕获两组 head logits，`real_inference_observed` 才为真。

运行前，脚本先写唯一 UUID plan：含冻结规则、完整请求与置换矩阵、预检结果、语料/标签分开的 SHA-256、源码/manifest/ORT 运行库哈希和唯一结果路径。plan 使用 `wx` 创建，发生任何模型推理前必须成功保存。最终结果也用 `wx` 唯一创建，并引用 plan 哈希；失败、拒绝、截断、未运行的行均随结果保存。源码和运行库在运行后再次散列，以报告是否稳定。

## 预先冻结的度量

- **Production policy accuracy/coverage**：使用后端实际接纳结果；固定置信度为 800/1000、action probability 为 0.5。coverage 分母固定为 20 个 canonical fixture。条件准确率分母是已接纳数；没有接纳项时值为 `null`，绝不写成 100%。`all_case_accuracy` 的分子只计正确且已接纳的选择，分母固定为 20；另列 `raw_argmax_accuracy`，单独报告有真实 logits 的样本（包括后端拒绝项），不会混入生产口径。同时按四类各 5 例分别报告这两种口径。
- **Accuracy–coverage curve**：只用 canonical 样本，置信度点预先固定为 500、600、700、800、900、950/1000，action probability floor 始终为 0.5。低于生产阈值的曲线点明确标成 counterfactual；不改变 800/1000 产品阈值。
- **Calibration**：对有真实 head 输出的 canonical 样本计算 top-choice ECE，固定 10 个等宽置信度区间；正确性由 argmax 是否等于独立 reference label 判断。另报多类 Brier score（每例对各候选 one-hot 误差平方求和，再对样本平均）。包含后端拒绝但有有效 logits 的样本。输出不完整时同时报告样本数和 incomplete 标记。
- **Permutation flips**：只比较真实 raw head argmax 的候选 ID，包括最终被后端拒绝的 logits。稳定率分母要求该例 canonical 和所有唯一置换都有真实 logits；零分母时稳定率为 `null`，不视为通过。
- **长度与候选数**：按全部计划置换汇总候选/选项数量、UTF-8 字节数和预检 token 长度分布。未运行或预检失败项仍计入总计划数，并分别报告可测行数。

冻结的诊断 screen 要求 20 个 canonical 样本都捕获真实 head 输出；production-policy coverage 至少 0.80、已接纳条件准确率至少 0.95；每一类别 coverage 至少 0.60、条件准确率至少 0.80；完整 canonical ECE 不高于 0.10；20 例置换稳定率至少 0.95。任何缺失的必需分母使结果为 `not_assessable`；完整但不满足标准为 `diagnostic_fail`。即使得到 `diagnostic_pass`，它也只是这组小型手工夹具的离线信号，不能晋级产品模型或宣称任务质量验收通过。

当前后端只接受 choice，因此 score 首位偏差不运行，也不伪造 score 输入。这个 runner 不测 latency；M0 CPU 延迟证据由独立 latency scan artifact 提供。teacher 标签不是 ground truth，本轮没有 teacher 调用或标签生成。此实验也不创建 M5 task-level knowledge release。

## 运行

仓库根目录执行一次冻结测量：

```sh
node --import tsx scripts/evaluate-laya-quality.mts
```

可选的离线线程参数只有 `--threads 1`、`--threads 2` 或 `--threads 4`；默认 2。它只控制本次 runner 的 CPU session，产品默认线程数仍为 2。runner 每完成 10 个计划样本打印一条进度，单次 backend decision 最长 30 秒；全轮和 session setup 期限写入 plan。报告保存在 `artifacts/m0-laya-s1-quality-plan-<UUID>.json` 与对应的 `artifacts/m0-laya-s1-quality-result-<UUID>.json`。
