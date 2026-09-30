//! A simulated settings application.
//!
//! Two pages (General and Notifications), a sidebar, same-named switches in
//! different groups, an Apply button with a save-state label, and optional
//! modal sheets. It reports itself as [`StructureSnapshot`]s and reacts to
//! inputs the simulated device backend applied.

use interaction_contracts::desktop::binding::instance_fingerprint;
use interaction_contracts::desktop::device::SurfaceRef;
use interaction_contracts::desktop::ids::{ObjectRef, RequestId};
use interaction_contracts::desktop::primitive::{AxActionKind, Primitive};
use interaction_contracts::desktop::source::{
    AppIdentity, AxActionName, LayerKind, Rect, StructureSnapshot, UiObject, UiValue, WindowInfo,
};

pub const BUNDLE_ID: &str = "test.settings";
pub const APP_NAME: &str = "Settings";
pub const SIDEBAR_GROUP: &str = "Sidebar";
const MAIN_WINDOW: &str = "w1";
const SHEET_WINDOW: &str = "w2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Notifications,
}

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Notifications => "Notifications",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveState {
    Saved,
    Unsaved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    /// "Reset all settings?" with Cancel and Reset.
    ResetConfirm,
    /// "Unsaved changes" with Apply, Discard and Cancel.
    UnsavedChanges,
}

#[derive(Debug, Clone)]
struct SwitchState {
    id: &'static str,
    group: &'static str,
    label: &'static str,
    on: bool,
    y: f64,
}

/// Identity of one snapshot the app is asked to produce.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub snapshot_id: String,
    pub session_epoch: u64,
    pub surface: SurfaceRef,
    pub clock_domain: String,
    pub source_sequence: u64,
    pub observed_ms: u64,
}

/// One input that reached the application.
#[derive(Debug, Clone)]
pub struct Applied {
    pub primitive: Primitive,
    /// The digest of the element the intent was bound to.
    pub binding_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyEffect {
    Toggled { id: String, on: bool },
    ValueSet { id: String },
    PageChanged(Page),
    Saved,
    ModalOpened(Modal),
    ModalClosed,
    Reset,
    Ignored(&'static str),
}

pub struct SimApp {
    page: Page,
    switches: Vec<SwitchState>,
    saved: Vec<bool>,
    language: String,
    save_state: SaveState,
    modal: Option<Modal>,
    pending_page: Option<Page>,
    /// Leaving the Notifications page with unsaved changes opens a sheet.
    pub sheet_on_unsaved_navigation: bool,
}

impl Default for SimApp {
    fn default() -> Self {
        Self::new()
    }
}

fn default_switches() -> Vec<SwitchState> {
    vec![
        SwitchState {
            id: "sw-push-daily",
            group: "Push",
            label: "Daily picks",
            on: true,
            y: 60.0,
        },
        SwitchState {
            id: "sw-push-weekly",
            group: "Push",
            label: "Weekly digest",
            on: true,
            y: 90.0,
        },
        SwitchState {
            id: "sw-email-daily",
            group: "Email",
            label: "Daily picks",
            on: true,
            y: 190.0,
        },
        SwitchState {
            id: "sw-email-promo",
            group: "Email",
            label: "Promotions",
            on: false,
            y: 220.0,
        },
    ]
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn parse_switch_value(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

impl SimApp {
    pub fn new() -> Self {
        let switches = default_switches();
        let saved = switches.iter().map(|s| s.on).collect();
        Self {
            page: Page::Notifications,
            switches,
            saved,
            language: "English".into(),
            save_state: SaveState::Saved,
            modal: None,
            pending_page: None,
            sheet_on_unsaved_navigation: false,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn save_state(&self) -> SaveState {
        self.save_state
    }

    pub fn modal(&self) -> Option<Modal> {
        self.modal
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn switch_on(&self, group: &str, label: &str) -> Option<bool> {
        self.switches
            .iter()
            .find(|s| s.group == group && s.label == label)
            .map(|s| s.on)
    }

    /// Put a modal sheet in front of the window, as a surprise would.
    pub fn inject_modal(&mut self, modal: Modal) {
        self.modal = Some(modal);
    }

    pub fn dismiss_modal(&mut self) {
        self.modal = None;
        self.pending_page = None;
    }

    /// The window and objects as they are now, plus the active layer.
    fn layout(&self) -> (Vec<WindowInfo>, Vec<UiObject>, String) {
        let main_title = format!("{APP_NAME} — {}", self.page.title());
        let mut windows = vec![WindowInfo {
            window_id: MAIN_WINDOW.into(),
            title: Some(main_title),
            frame: rect(0.0, 0.0, 600.0, 400.0),
            z_index: if self.modal.is_some() { 1 } else { 0 },
            kind: LayerKind::Window,
            modal: false,
            owner_bundle_id: BUNDLE_ID.into(),
        }];
        let mut objects = Vec::new();
        let mut add = |reference: &str,
                       parent: Option<&str>,
                       window: &str,
                       role: &str,
                       title: Option<&str>,
                       description: Option<&str>,
                       value: Option<UiValue>,
                       frame: Rect,
                       actions: &[AxActionName],
                       group: Option<&str>| {
            objects.push(UiObject {
                instance_ref: ObjectRef::new(reference).expect("fixed ids are valid"),
                parent: parent.map(|p| ObjectRef::new(p).expect("fixed ids are valid")),
                window_id: window.into(),
                role: role.into(),
                subrole: None,
                title: title.map(String::from),
                description: description.map(String::from),
                value,
                enabled: true,
                focused: false,
                frame,
                actions: actions.to_vec(),
                in_view: true,
                group: group.map(String::from),
                secure: false,
            });
        };

        add(
            "group-sidebar",
            None,
            MAIN_WINDOW,
            "group",
            Some(SIDEBAR_GROUP),
            None,
            None,
            rect(0.0, 30.0, 140.0, 300.0),
            &[],
            None,
        );
        for (reference, page, y) in [
            ("tab-general", Page::General, 40.0),
            ("tab-notifications", Page::Notifications, 72.0),
        ] {
            add(
                reference,
                Some("group-sidebar"),
                MAIN_WINDOW,
                "tab",
                Some(page.title()),
                None,
                Some(UiValue::Bool(self.page == page)),
                rect(10.0, y, 120.0, 24.0),
                &[AxActionName::Press],
                Some(SIDEBAR_GROUP),
            );
        }

        match self.page {
            Page::Notifications => {
                add(
                    "group-push",
                    None,
                    MAIN_WINDOW,
                    "group",
                    Some("Push"),
                    None,
                    None,
                    rect(150.0, 40.0, 430.0, 110.0),
                    &[],
                    None,
                );
                add(
                    "group-email",
                    None,
                    MAIN_WINDOW,
                    "group",
                    Some("Email"),
                    None,
                    None,
                    rect(150.0, 170.0, 430.0, 110.0),
                    &[],
                    None,
                );
                for switch in &self.switches {
                    let parent = if switch.group == "Push" {
                        "group-push"
                    } else {
                        "group-email"
                    };
                    add(
                        switch.id,
                        Some(parent),
                        MAIN_WINDOW,
                        "switch",
                        Some(switch.label),
                        None,
                        Some(UiValue::Bool(switch.on)),
                        rect(400.0, switch.y, 40.0, 20.0),
                        &[AxActionName::Press, AxActionName::SetValue],
                        Some(switch.group),
                    );
                }
                add(
                    "btn-apply",
                    None,
                    MAIN_WINDOW,
                    "button",
                    Some("Apply"),
                    None,
                    None,
                    rect(150.0, 330.0, 80.0, 28.0),
                    &[AxActionName::Press],
                    None,
                );
                let state = match self.save_state {
                    SaveState::Saved => "saved",
                    SaveState::Unsaved => "unsaved",
                };
                add(
                    "txt-save-state",
                    None,
                    MAIN_WINDOW,
                    "text",
                    None,
                    Some("save-state"),
                    Some(UiValue::Text(state.into())),
                    rect(250.0, 334.0, 200.0, 20.0),
                    &[],
                    None,
                );
            }
            Page::General => {
                add(
                    "popup-language",
                    None,
                    MAIN_WINDOW,
                    "popup",
                    Some("Language"),
                    None,
                    Some(UiValue::Text(self.language.clone())),
                    rect(400.0, 60.0, 120.0, 24.0),
                    &[AxActionName::Press, AxActionName::SetValue],
                    None,
                );
                add(
                    "btn-reset",
                    None,
                    MAIN_WINDOW,
                    "button",
                    Some("Reset to defaults"),
                    None,
                    None,
                    rect(150.0, 330.0, 140.0, 28.0),
                    &[AxActionName::Press],
                    None,
                );
            }
        }

        let mut active = MAIN_WINDOW.to_string();
        if let Some(modal) = self.modal {
            active = SHEET_WINDOW.into();
            let title = match modal {
                Modal::ResetConfirm => "Reset all settings?",
                Modal::UnsavedChanges => "Unsaved changes",
            };
            windows.push(WindowInfo {
                window_id: SHEET_WINDOW.into(),
                title: Some(title.into()),
                frame: rect(150.0, 120.0, 300.0, 160.0),
                z_index: 0,
                kind: LayerKind::Sheet,
                modal: true,
                owner_bundle_id: BUNDLE_ID.into(),
            });
            add(
                "modal-root",
                None,
                SHEET_WINDOW,
                "sheet",
                Some(title),
                None,
                None,
                rect(150.0, 120.0, 300.0, 160.0),
                &[],
                None,
            );
            let buttons: &[(&str, &str, f64)] = match modal {
                Modal::ResetConfirm => &[
                    ("modal-cancel", "Cancel", 170.0),
                    ("modal-reset", "Reset", 350.0),
                ],
                Modal::UnsavedChanges => &[
                    ("modal-apply", "Apply", 170.0),
                    ("modal-discard", "Discard", 260.0),
                    ("modal-cancel", "Cancel", 350.0),
                ],
            };
            for (reference, label, x) in buttons {
                add(
                    reference,
                    Some("modal-root"),
                    SHEET_WINDOW,
                    "button",
                    Some(label),
                    None,
                    None,
                    rect(*x, 240.0, 80.0, 28.0),
                    &[AxActionName::Press],
                    None,
                );
            }
        }
        (windows, objects, active)
    }

    fn build(&self, meta: &SnapshotMeta, complete: bool, keep: Option<usize>) -> StructureSnapshot {
        let (windows, mut objects, active) = self.layout();
        if let Some(keep) = keep {
            objects.truncate(keep);
        }
        StructureSnapshot {
            snapshot_id: RequestId::new(meta.snapshot_id.clone()).expect("snapshot ids are valid"),
            session_epoch: meta.session_epoch,
            surface: meta.surface.clone(),
            clock_domain: meta.clock_domain.clone(),
            source_sequence: meta.source_sequence,
            observed_ms: meta.observed_ms,
            app: AppIdentity {
                bundle_id: BUNDLE_ID.into(),
                name: APP_NAME.into(),
                pid: 4242,
            },
            windows,
            active_layer: Some(active),
            complete,
            incomplete_reason: (!complete).then(|| "accessibility read timed out".to_string()),
            objects,
        }
    }

    pub fn snapshot(&self, meta: &SnapshotMeta) -> StructureSnapshot {
        self.build(meta, true, None)
    }

    /// A read that timed out after `keep` objects.
    pub fn partial_snapshot(&self, meta: &SnapshotMeta, keep: usize) -> StructureSnapshot {
        self.build(meta, false, Some(keep))
    }

    /// The fingerprint digest of one element right now, as a binding proof.
    pub fn fingerprint_of(&self, meta: &SnapshotMeta, instance: &str) -> Option<String> {
        let snapshot = self.snapshot(meta);
        let object = snapshot
            .objects
            .iter()
            .find(|object| object.instance_ref.as_str() == instance)?;
        Some(instance_fingerprint(&snapshot, object).digest())
    }

    /// React to one input the device applied.
    pub fn apply(&mut self, meta: &SnapshotMeta, applied: &Applied) -> ApplyEffect {
        let snapshot = self.snapshot(meta);
        let active = snapshot.active_layer.clone().unwrap_or_default();
        let target = match &applied.primitive {
            Primitive::AxAction { .. } => {
                let Some(proof) = &applied.binding_fingerprint else {
                    return ApplyEffect::Ignored("an AX action needs a binding proof");
                };
                snapshot
                    .objects
                    .iter()
                    .find(|object| &instance_fingerprint(&snapshot, object).digest() == proof)
            }
            Primitive::Click { x, y, .. } => snapshot
                .objects
                .iter()
                .filter(|object| {
                    object.window_id == active
                        && object.frame.contains(f64::from(*x), f64::from(*y))
                })
                .min_by(|a, b| {
                    (a.frame.width * a.frame.height)
                        .partial_cmp(&(b.frame.width * b.frame.height))
                        .expect("finite areas")
                }),
            _ => return ApplyEffect::Ignored("primitive is not modelled"),
        };
        let Some(target) = target else {
            return ApplyEffect::Ignored("nothing matches the target");
        };
        if target.window_id != active {
            return ApplyEffect::Ignored("blocked by the modal sheet");
        }
        let id = target.instance_ref.as_str().to_string();
        let value = match &applied.primitive {
            Primitive::AxAction {
                action: AxActionKind::SetValue { value },
            } => Some(value.clone()),
            _ => None,
        };
        self.act(&id, value)
    }

    fn set_switch(&mut self, id: &str, on: bool) -> ApplyEffect {
        if let Some(switch) = self.switches.iter_mut().find(|s| s.id == id) {
            switch.on = on;
            self.save_state = SaveState::Unsaved;
            ApplyEffect::Toggled { id: id.into(), on }
        } else {
            ApplyEffect::Ignored("no such switch")
        }
    }

    fn navigate(&mut self, page: Page) -> ApplyEffect {
        if page == self.page {
            return ApplyEffect::PageChanged(page);
        }
        if self.sheet_on_unsaved_navigation
            && self.save_state == SaveState::Unsaved
            && self.page == Page::Notifications
        {
            self.pending_page = Some(page);
            self.modal = Some(Modal::UnsavedChanges);
            return ApplyEffect::ModalOpened(Modal::UnsavedChanges);
        }
        self.page = page;
        ApplyEffect::PageChanged(page)
    }

    fn save(&mut self) {
        self.saved = self.switches.iter().map(|s| s.on).collect();
        self.save_state = SaveState::Saved;
    }

    fn act(&mut self, id: &str, value: Option<String>) -> ApplyEffect {
        match id {
            "tab-general" => self.navigate(Page::General),
            "tab-notifications" => self.navigate(Page::Notifications),
            "btn-apply" => {
                self.save();
                ApplyEffect::Saved
            }
            "btn-reset" => {
                self.modal = Some(Modal::ResetConfirm);
                ApplyEffect::ModalOpened(Modal::ResetConfirm)
            }
            "modal-apply" => {
                self.save();
                self.modal = None;
                if let Some(page) = self.pending_page.take() {
                    self.page = page;
                }
                ApplyEffect::Saved
            }
            "modal-discard" => {
                for (switch, was) in self.switches.iter_mut().zip(&self.saved) {
                    switch.on = *was;
                }
                self.save_state = SaveState::Saved;
                self.modal = None;
                if let Some(page) = self.pending_page.take() {
                    self.page = page;
                }
                ApplyEffect::ModalClosed
            }
            "modal-cancel" => {
                self.modal = None;
                self.pending_page = None;
                ApplyEffect::ModalClosed
            }
            "modal-reset" => {
                self.switches = default_switches();
                self.language = "English".into();
                self.save_state = SaveState::Unsaved;
                self.modal = None;
                ApplyEffect::Reset
            }
            "popup-language" => match value {
                Some(language) => {
                    self.language = language;
                    ApplyEffect::ValueSet { id: id.into() }
                }
                None => ApplyEffect::Ignored("the popup needs a value"),
            },
            switch if switch.starts_with("sw-") => {
                let current = self.switches.iter().find(|s| s.id == switch).map(|s| s.on);
                match (value.as_deref().map(parse_switch_value), current) {
                    (Some(Some(on)), Some(_)) => self.set_switch(switch, on),
                    (Some(None), _) => ApplyEffect::Ignored("unrecognised switch value"),
                    (None, Some(on)) => self.set_switch(switch, !on),
                    (_, None) => ApplyEffect::Ignored("no such switch"),
                }
            }
            _ => ApplyEffect::Ignored("not an actionable element"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(sequence: u64) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: format!("snap-{sequence}"),
            session_epoch: 1,
            surface: SurfaceRef {
                id: "main".into(),
                epoch: 1,
            },
            clock_domain: "sim/1/monotonic".into(),
            source_sequence: sequence,
            observed_ms: sequence * 10,
        }
    }

    fn ax(value: Option<&str>, proof: String) -> Applied {
        Applied {
            primitive: Primitive::AxAction {
                action: match value {
                    Some(value) => AxActionKind::SetValue {
                        value: value.into(),
                    },
                    None => AxActionKind::Press,
                },
            },
            binding_fingerprint: Some(proof),
        }
    }

    #[test]
    fn snapshots_are_valid_and_contain_same_named_switches_in_two_groups() {
        let app = SimApp::new();
        let snapshot = app.snapshot(&meta(1));
        snapshot.validate().unwrap();
        let daily: Vec<_> = snapshot
            .objects
            .iter()
            .filter(|o| o.title.as_deref() == Some("Daily picks"))
            .collect();
        assert_eq!(daily.len(), 2);
        assert_ne!(daily[0].group, daily[1].group);
    }

    #[test]
    fn an_ax_set_value_acts_on_exactly_the_bound_element() {
        let mut app = SimApp::new();
        let proof = app.fingerprint_of(&meta(1), "sw-push-daily").unwrap();
        let effect = app.apply(&meta(2), &ax(Some("off"), proof));
        assert_eq!(
            effect,
            ApplyEffect::Toggled {
                id: "sw-push-daily".into(),
                on: false
            }
        );
        assert_eq!(app.switch_on("Push", "Daily picks"), Some(false));
        assert_eq!(
            app.switch_on("Email", "Daily picks"),
            Some(true),
            "the same-named switch is untouched"
        );
        assert_eq!(app.save_state(), SaveState::Unsaved);
    }

    #[test]
    fn a_stale_binding_proof_changes_nothing() {
        let mut app = SimApp::new();
        let effect = app.apply(&meta(1), &ax(Some("off"), "fp1:stale".into()));
        assert!(matches!(effect, ApplyEffect::Ignored(_)));
        assert_eq!(app.switch_on("Push", "Daily picks"), Some(true));
    }

    #[test]
    fn a_click_hits_the_smallest_element_under_the_point() {
        let mut app = SimApp::new();
        let click = Applied {
            primitive: Primitive::Click {
                button: interaction_contracts::desktop::primitive::MouseButton::Left,
                x: 420,
                y: 70,
                modifiers: Default::default(),
            },
            binding_fingerprint: None,
        };
        assert_eq!(
            app.apply(&meta(1), &click),
            ApplyEffect::Toggled {
                id: "sw-push-daily".into(),
                on: false
            }
        );
    }

    #[test]
    fn pressing_a_tab_changes_the_page_and_the_window_title() {
        let mut app = SimApp::new();
        let proof = app.fingerprint_of(&meta(1), "tab-general").unwrap();
        assert_eq!(
            app.apply(&meta(2), &ax(None, proof)),
            ApplyEffect::PageChanged(Page::General)
        );
        let snapshot = app.snapshot(&meta(3));
        assert_eq!(
            snapshot.windows[0].title.as_deref(),
            Some("Settings — General")
        );
        assert!(
            snapshot
                .objects
                .iter()
                .any(|o| o.instance_ref.as_str() == "btn-reset")
        );
        assert!(
            !snapshot
                .objects
                .iter()
                .any(|o| o.group.as_deref() == Some("Push"))
        );
    }

    #[test]
    fn a_modal_sheet_takes_the_active_layer_and_blocks_the_window_behind_it() {
        let mut app = SimApp::new();
        let proof = app.fingerprint_of(&meta(1), "sw-push-daily").unwrap();
        app.inject_modal(Modal::UnsavedChanges);
        let snapshot = app.snapshot(&meta(2));
        assert_eq!(snapshot.active_layer.as_deref(), Some("w2"));
        assert!(snapshot.windows.iter().any(|w| w.modal));
        let effect = app.apply(&meta(3), &ax(Some("off"), proof));
        assert!(matches!(effect, ApplyEffect::Ignored(_)), "{effect:?}");
        assert_eq!(app.switch_on("Push", "Daily picks"), Some(true));
    }

    #[test]
    fn leaving_with_unsaved_changes_opens_the_sheet_when_enabled() {
        let mut app = SimApp::new();
        app.sheet_on_unsaved_navigation = true;
        let switch = app.fingerprint_of(&meta(1), "sw-push-daily").unwrap();
        app.apply(&meta(2), &ax(Some("off"), switch));
        let tab = app.fingerprint_of(&meta(3), "tab-general").unwrap();
        assert_eq!(
            app.apply(&meta(4), &ax(None, tab)),
            ApplyEffect::ModalOpened(Modal::UnsavedChanges)
        );
        assert_eq!(
            app.page(),
            Page::Notifications,
            "navigation waits for the answer"
        );
        let apply = app.fingerprint_of(&meta(5), "modal-apply").unwrap();
        assert_eq!(app.apply(&meta(6), &ax(None, apply)), ApplyEffect::Saved);
        assert_eq!(app.page(), Page::General);
        assert_eq!(app.save_state(), SaveState::Saved);
    }

    #[test]
    fn discarding_reverts_the_unsaved_switch_changes() {
        let mut app = SimApp::new();
        app.sheet_on_unsaved_navigation = true;
        let switch = app.fingerprint_of(&meta(1), "sw-push-daily").unwrap();
        app.apply(&meta(2), &ax(Some("off"), switch));
        let tab = app.fingerprint_of(&meta(3), "tab-general").unwrap();
        app.apply(&meta(4), &ax(None, tab));
        let discard = app.fingerprint_of(&meta(5), "modal-discard").unwrap();
        app.apply(&meta(6), &ax(None, discard));
        assert_eq!(app.switch_on("Push", "Daily picks"), Some(true));
    }

    #[test]
    fn a_partial_snapshot_says_it_is_incomplete() {
        let snapshot = SimApp::new().partial_snapshot(&meta(1), 3);
        snapshot.validate().unwrap();
        assert!(!snapshot.complete);
        assert_eq!(snapshot.objects.len(), 3);
    }
}
