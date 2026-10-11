# Keychain credentials and OAuth sessions

This is a tracked summary. Referenced logs, manifests and scripts remain in the local `artifacts/keychain-oauth-20261010/` evidence directory and are not included in Git.

User scope: reuse LingXi Electron provider credential settings and persistence semantics so API keys and OpenAI account tokens survive restart in macOS Keychain.

Source references and hashes: `reference-sources.json`. UI adapts the existing Electron Card/Row settings and login/re-login/cancel/sign-out semantics; Main adapts the strict account-session serialization and credential-broker ownership. The existing Computer Use native Keychain adapter remains in use under its own service namespace. This is not a transplant of Electron's signed Data Protection Keychain/XPC deployment.

Credentials never enter renderer snapshots or plaintext metadata. OAuth refresh tokens remain Main-owned; the model Host receives only the access-token credential. Startup discovers public metadata only; reconnect confirms task purposes before reading Keychain. Prior in-memory login sessions did not retain refresh tokens and need a fresh login in the new build.

Validation records are adjacent JSON/log files. Native tests use random synthetic credentials with cleanup, never user credentials. First sandbox Keychain check returned unavailable; the same owned-item check outside the sandbox passed. Initial integration checks caught the unfinished reauthentication IPC allowlist and TypeScript integration issues; retain those failed records and use the final successful records for acceptance.

This batch does not establish live OpenAI refresh or full MVP business acceptance. No commit or push.

Final evidence: affected-tests-final (all passed), api-tests (37 passed), desktop-types-verified, desktop-build-final, auth-helper-test/build/clippy, native-session-verified. Independent reviewer found no remaining blockers after the store rollback, unified legacy writer, and dispatched-refresh cancellation fixes. The broad desktop-regression attempt was interrupted after sandbox socket failures; it is not an accepted full-suite result.

GUI: fully quit old single-instance process and launched final built output. Native accessibility verified OpenAI account detail shows Credential storage / macOS Keychain, post-sign-in saving copy and Sign in with OpenAI; no actual login was initiated in the new build. Old process had memory-only connected sessions. User must sign in once in this build to persist. final-files.json pins production source, helper binaries, and Electron output.
