# Interaction Fixture

This standalone AppKit application backs the 20 fixture task profiles. Its bundle identifier is `com.lingxi.computer-user.mvp-fixture`; its visible application and window name is always `Interaction Fixture`.

Build the app from the repository root with `scripts/build-macos-mvp-fixture.sh`. The build uses Swift 6, complete strict-concurrency checking, warnings as errors, and the `-parse-as-library` entry-point mode. The resulting app is `target/native-macos/Interaction Fixture.app`.

The launcher starts one fresh process for each fixture profile:

```sh
"target/native-macos/Interaction Fixture.app/Contents/MacOS/MvpFixture" \
  --profile /absolute/private/reset.json \
  --snapshot /absolute/private/snapshot.json
```

The profile must be a regular, non-symlink file no larger than 128 KiB. Its only top-level fields are `version`, `task_id`, `seed`, and `initial_state`. The app rejects objective, allowed-scope, evaluator, oracle, and expected-result fields. It resets from the supplied initial state on each process launch. There are no in-app reset or task-completion controls. Task identity and reset paths are never shown in the UI; reset paths are not included in the snapshot.

The fixture renders native, visible, accessibility-addressable controls for settings and forms; native pop-up and radio controls; scrollable list tables; and attached modal sheets. Every profile has shared Settings and Overview navigation, with background navigation disabled while a modal sheet is open. Returning from Overview reconstructs the content; if a list row was selected, the rebuilt table restores that selection from fixture state without recording another semantic selection change. Its 200-row archive is an `NSTableView` that creates row views for the visible viewport. The record-status case sorts after a status edit and rebuilds its row records while retaining their IDs. Language, Aqua/Dark Aqua appearance, window size, spacing, and the two-column settings layout follow each profile's frozen variant.

SET-01 through SET-05 place each target control in a visible titled Settings group matching its frozen scope: Notifications, Search, or Display. FORM-01 through FORM-07 likewise place each form's target field or control in its scoped Profile, Preferences, Account, or Advanced preferences group. FORM-06 keeps validation feedback with the Account field, and FORM-07 keeps its advanced preference entries and Alert threshold in the Advanced preferences group. Group titles are localized with the task profile language.

SET-03 exposes a real Save button and a visible manual-save receipt. SET-04 marks a changed value unsaved, waits 800 ms, then writes an automatic-save receipt. Region-code validation uses `CA-` followed by four digits. Modal cases use blocking `NSWindow` sheets; the background controls remain ordinary controls but are unavailable while a sheet is attached.

FORM-04 exposes its existing Summary grouping stack as a labeled `AXRadioGroup` with three natural, direct `AXRadioButton` children. The frozen task still targets the group with a Text choice; selecting a child is an internal execution detail. The group has no own value or input action, so its selected Text must be derived from an exclusive selected child in a complete fresh Source.

The editable FORM-01/FORM-02 text controls and FORM-05 checkbox also handle accessibility value writes by applying the requested value to the visible AppKit control and forwarding it through the same fixture state update used by ordinary editing and checkbox actions. The state update ignores an unchanged value, so an accessibility write produces at most one semantic audit increment even if AppKit also emits its normal delegate or action callback.

The app prints one ready record to stdout after the main window appears:

```json
{"pid":12345,"window_number":1}
```

Append `--background` after the snapshot path to show the window behind other windows without the fixture calling `makeKeyAndOrderFront` or `NSApplication.activate`. This keeps the same UI and snapshot behavior. Live tests must independently verify that the foreground app/window is preserved; the launch option alone does not prove this.

It emits no other stdout content. Errors use generic stderr codes and do not include profile contents or paths. The snapshot contains only the current task identity and actual UI-backed state:

```text
{version, task_id, seed, values, selected_row, modal, persistence, audit}
```

`selected_row` is null until a row is selected; otherwise it records the row ID, visible label, owner, status, and 1-based position. Modal state contains `open`, `choice`, and the visible modal control values. Persistence records `saved`, `kind`, explicit/automatic counts, and the visible receipt's kind and revision. Audit counters record semantic value changes, modal-background changes, other-row changes, state revision, Settings revisits, and actual table row-view bindings. There are no goal-match, completion, or expected-answer fields.

Every snapshot update is written through a same-directory temporary file, fsynced, atomically renamed, and set to mode `0600`. The snapshot path is not exposed through the UI or the snapshot contents. The external runner owns the private run directory and its cleanup.
