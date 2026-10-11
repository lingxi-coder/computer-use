# Working in computer-use

## Delivery workflow

- Follow the user's latest scope and pause/resume instruction. A workflow review does not resume paused product implementation. Do not commit or push unless requested.
- Read [development-workflow.md](docs/development-workflow.md) before substantial work. Start with its compact handoff and the relevant section of `docs/mvp-status.md`; do not reread the whole historical log.
- Choose one dependency-complete product outcome per batch. Define observable success and required tests before editing. Prioritize the next missing user-visible behavior over additional diagnostic infrastructure.
- Delegate independent ownership: contracts, core behavior, desktop integration, and driver/tests. Stabilize shared interfaces first. Assign exact files; workers must not overwrite or revert another worker's changes.
- Use the user's selected agent model/effort when available. Reuse idle agents. The lead integrates; one independent reviewer reviews the completed diff. Do not ask every agent to review the whole repository.
- Workers use focused checks. The lead owns one Cargo build/test lane and one live GUI lane. Parallelize independent lightweight checks; do not compete for the same Cargo target, native fixture, foreground window, or live app.
- Integrate related work before broad checks. Use the verification matrix in the workflow document. A new message or turn alone does not invalidate previously accepted evidence.
- Diagnose the first failure and rerun the affected check after fixing it. Repeat a full gate only when its result was invalidated; do not restart unrelated passing gates.
- Preflight the known offline cache, disk space, required local IPC/GUI access, and build prerequisites. Use the tool permission mechanism when needed; do not repeat a known sandbox failure first.
- Preserve failure logs. Do not weaken production deadlines, permissions, Stop behavior, acceptance criteria, or the original 100 fixture + 20 TextEdit denominator to accelerate work.
- Produce one concise handoff per completed batch or pause. Link unchanged evidence to its checkpoint; do not copy historical manifests into every update. Sealed artifacts stay immutable.
- Record validation duration. Report completed behavior, relevant validation, and the next blocker; test counts and diagnostic runs do not establish MVP completion.

## Architecture and source boundaries

- `docs/design/complete-design.md` is the design authority. Preserve module ownership and the admission/receipt/verification lifecycle.
- Compare work against the latest accepted source checkpoint and current working tree. Git HEAD may predate substantial uncommitted implementation; preserve unrelated work.
- Keep product integration here. Respect other sessions' repository ownership; reading a dependency does not authorize editing it or messaging its owner.
- Use focused `rg` searches. Exclude `target`, `node_modules`, native build output, and historical artifacts unless directly needed.
