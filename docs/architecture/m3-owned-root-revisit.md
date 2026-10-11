# Owned document revisit with the original Root

This continuation retains the existing Host process, SQLite database, frozen
Root, attempt history and budgets. Main replaces the owned TextEdit process,
Source and Device through a private control channel. The reopened Device is
`macos-observe`; it receives no owned input bootstrap or Save authorization.

The Host and Main use inherited FD3 with version 1 length-prefixed JSON, bounded
to 32 KiB. Each request has a distinct UUID. The transition UUID correlates
Prepare, Finish and Abort. The public desktop Envelope, operator commands,
renderer and model tools cannot carry these messages or new registrations.

Engine issues Prepare only after checking a settled Save, its durable receipt
evidence and the preceding CurrentUi effect for the same Root. Main captures
the private saved file before closing the target. An opaque, one-use token pins
directory and file identity, exact bytes and BigInt nanosecond metadata. The
fixed version must remain identical after the exact target's stop receipt and
after the one-use ticket opens a distinct process and registration. The private
lineage contains metadata and hashes, without document text or paths.

After Prepare, old peer connections and queued observations are fenced. Main
reaps Source and Device before creating replacement peers, rotates all peer
tokens and advances the session epoch. A new image channel uses the new token,
epoch and PID. The optional image connection starts after Host acknowledges
Finish, so it cannot delay the fresh Source's observation startup. The Host
remains alive throughout. Main does not abort a Finish
already acknowledged by Host merely because delivery crossed the deadline.

Finish installs the new private registration for verification. A matching
Source refresh acknowledgement and a fresh owned AX read from the new session
and namespace are required for UiRevisit. Save completion and file lineage do
not independently complete the Root. Stop, cancellation, invalid identity,
drift, replay, unknown input and expiry cannot restore the old input grant.

The dedicated revisit wait does not run the generic periodic refresh retry.
That retry would replace the new native watch and advance the Environment's
generation without advancing the dedicated acknowledgement correlation. A
delayed read stays on the accepted generation; the challenge remains bounded
by 30 seconds and the Root deadline. Challenge expiry ends Unverified. The
Root deadline takes precedence and retains the ordinary Expired, epoch and
stop semantics.

The [physical retained-Root diagnostic](../../artifacts/m3-owned-root-revisit-ff122492-4524-402e-baec-0e357fe91a88.json)
passed on 2026-10-04. It performed one SetValue and one Save, verified the saved
52-byte Unicode file, reopened a distinct TextEdit process and registration,
and recorded a satisfied UiRevisit effect from fresh Source evidence at
5183 ms. The original Host, Root ID/revision/epoch, CurrentUi history and two
Attempt/mapping/outbox records were preserved. The new Device was observe-only;
revisit added no input or SDK call. Cleanup stopped the owned instance, and
source/binary hashes were unchanged during the run.

Four preceding failed runs are retained under `m3-owned-root-revisit-*.json`.
They did not falsely complete or replay input, and each cleaned up. The first
two exposed generic refresh retries; the following runs showed a matching ACK
without new proof. Moving optional image startup after Finish was followed by
the successful run. Earlier diagnostics do not prove an exact native failure
reason, so the 5-second helper bootstrap freshness window remains a documented
timing risk rather than a confirmed unique cause.

The [checkpoint](../../artifacts/m3-owned-root-revisit-checkpoint-20261004.json)
also confirms the successful physical run's source and binary pins are current.
Engine 386 and Host 53 tests pass. Main has 165 passing tests plus passing
workspace and diagnostic script type checks; tests cover file drift, one-use
tokens, peer replacement, image startup order, delayed Finish delivery,
coalesced late frames, delayed readback and both deadlines. Strict Clippy,
Rust formatting, schema sync and production binary builds pass. Live OpenAI
teacher/S2 calls, product Save consent and full MVP acceptance remain separate.
