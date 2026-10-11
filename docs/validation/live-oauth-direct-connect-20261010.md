# Live OAuth and direct connection

This is a tracked summary. Referenced logs, manifests and scripts remain in the local `artifacts/live-oauth-direct-connect-20261010/` evidence directory and are not included in Git.

User authorized use of the logged-in OpenAI account and then requested removal of connection confirmation dialogs. Main now accepts the explicit Connect action and its selected purposes directly, with cancellation still enforced. Native input/window and task confirmations are unchanged.

Live UI evidence (2026-10-10 PDT): account initially Connected / credential saved in macOS Keychain; selected gpt-6-luna; a real S2 request interpreted the Promotions-off task and produced goal confirmation. Execution failed after two requests for more facts with `the replan budget is exhausted`. This is a failed simulated business test, not MVP acceptance. The exact binding failure cause is not established; the simulator starts on Notifications and the UI goal had no group.

Fully quit and restarted the app. Saved metadata appeared; Connect saved account restored Connected without browser login. A bounded read-only check through ProductModelKeyStore confirmed access and refresh fields exist and the access session was unexpired; no credential values were logged. Live refresh-token rotation was not forced or tested.

After the direct-connect change, built and fully restarted again. Clicking Connect saved account immediately transitioned to Connected with the selected S2/S1 purposes and no confirmation dialog. Model directory refresh still fails and retains the bundled SDK catalog; this pre-existing issue remains open. Source review found Electron uses a curated static catalog for this account flow; no guessed endpoint or SDK changes were made.

Validation: connection-tests, desktop-types and desktop-build JSON/logs retain exact commands and durations. The updated manual smoke script was syntax checked, not executed. No production Rust changes or Cargo rebuild. No credentials in this artifact. No commit/push.
