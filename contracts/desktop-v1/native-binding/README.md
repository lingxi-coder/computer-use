# Native fp1 golden

`golden.json` is consumed by Rust binding tests, Swift device self-tests and the owned native guard smoke. It fixes field order, explicit nullable fields, UTF-8 JSON escaping, SHA-256, Unicode White_Space/contextual lowercase and whole-point rounding away from zero at ties.

This is a diagnostic helper fixture, not a new desktop v1 Envelope variant or input permission. Native requests validate all fields and their digest before AX lookup; unknown or oversized contexts remain unresolved. A matching fingerprint does not establish source ObjectRef continuity or replace execution guards.

`scoped-window-source.json` is an immutable historical Source replay whose checksum is recorded in `artifacts/m3-scoped-window-activation-9c537571-b51e-4322-9d3e-f8145a8e82b9/replay-source-provenance.json`. The Rust WindowRoot unit tests use `scoped-window-synthetic.json`, a test-only projection that replaces the old `active_layer` field with an explicitly synthetic `global_focus` fact so current contracts can exercise the binding algorithm. That projection is not Source evidence and must not be used by runtime code or live-acceptance claims.

`background-window-source.json` preserves the original bytes of a real, complete
Background AX Source read from the ordinary Guard checkbox fixture. Its adjacent
provenance file records the archive path and checksum. The target has raw
`AXPress` support while its foreground-policy `actions` list is empty and another
application has focus. This replay checks background binding/candidate behavior;
the originating run submitted no target input and does not prove successful live
operation or MVP acceptance.
