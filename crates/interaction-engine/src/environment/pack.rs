//! An environment pack: what the system knows about one application
//! (design 9.3). It is data, not code, and it registers control semantics;
//! the model never declares an effect class for a control.

use interaction_contracts::desktop::binding::normalize_label;
use interaction_contracts::desktop::effect::EffectClass;
use interaction_contracts::desktop::ids::SemanticTargetRef;
use interaction_contracts::desktop::source::{AxActionName, StructureSnapshot, UiObject, UiValue};
use interaction_contracts::desktop::task::{
    AllowedControlTarget, RowContainerRole, RowContainerScope, RowTableScope, RowTargetSpec,
    ScopedCommitBinding, ScopedCommitKind, ScopedCommitReceiptBinding, TargetSpec,
};

use super::read::focused_window_id;

#[derive(Debug, Clone)]
pub struct EffectRule {
    /// Only controls with this role, when set.
    pub role: Option<String>,
    /// Matches when the normalised label contains this text.
    pub label_contains: String,
    pub class: EffectClass,
}

/// A control the pack knows by name, so a plan step can refer to it without
/// the model describing it.
#[derive(Debug, Clone)]
pub struct ControlDef {
    pub semantic: String,
    pub label: String,
    pub role: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedControlValueKind {
    Bool,
    Text,
    Number,
    Choice(&'static [&'static str]),
    RadioChoice(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedControlContextKind {
    Unspecified,
    Group,
    Form,
}

/// A trusted primary control resolved from the user's single-control scope.
/// `control` is the pack's canonical semantic/label/role triple; the remaining
/// fields bind its accepted value and localized public context.
#[derive(Debug, Clone, Copy)]
pub struct ScopedControl<'a> {
    pub control: &'a ControlDef,
    pub value_kind: ScopedControlValueKind,
    pub context_kind: ScopedControlContextKind,
    pub context_label: Option<&'static str>,
}

/// A fixed, source-visible validation message associated with one registered
/// form control. This metadata grants no action; it only lets verification
/// require the exact feedback the fixture exposes for that control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedValidationFeedback {
    pub control_semantic: &'static str,
    pub role: &'static str,
    pub description: &'static str,
    pub context_label: &'static str,
    pub valid_text: &'static str,
    pub invalid_text: &'static str,
}

/// A fixed application capability to select a matching row by setting its
/// actual AXSelected attribute to true. The scope is public structure only;
/// it contains no row values, Source refs, or task-specific selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOperationDef {
    pub semantic_target: String,
    pub table_scope: RowTableScope,
    pub selection_effect: EffectClass,
}

#[derive(Debug, Clone)]
pub struct AppPack {
    pub bundle_id: String,
    pub name: String,
    pub rules: Vec<EffectRule>,
    pub controls: Vec<ControlDef>,
    /// Explicit static row-selection capabilities. Observation-only packs do
    /// not inherit capabilities from their bundle identifier.
    pub row_operations: Vec<RowOperationDef>,
    /// The role of controls that switch pages.
    pub nav_role: String,
    /// `description` of the control that reports the save state.
    pub save_state_description: Option<String>,
}

/// Label words that raise the class of any control, whatever a pack says.
/// Inference only ever raises a class, it never lowers one.
const DESTRUCTIVE_WORDS: [&str; 6] = ["delete", "remove", "discard", "reset", "erase", "clear"];
const EXTERNAL_WORDS: [&str; 5] = ["send", "publish", "post", "pay", "purchase"];
const MVP_FIXTURE_BUNDLE: &str = "com.lingxi.computer-user.mvp-fixture";

#[derive(Debug, Clone, Copy)]
struct ScopedControlRegistration {
    semantic: &'static str,
    label: &'static str,
    role: &'static str,
    value_kind: ScopedControlValueKind,
    context_kind: ScopedControlContextKind,
    context_label: &'static str,
}

/// Only these frozen fixture controls are registered for the first native MVP
/// lane. The visible label and exact group/form context are Source facts;
/// English and Chinese use distinct semantic ids so translations never alias.
const MVP_SCOPED_CONTROLS: [ScopedControlRegistration; 26] = [
    ScopedControlRegistration {
        semantic: "setting-email-alerts-en",
        label: "Email alerts",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "Notifications",
    },
    ScopedControlRegistration {
        semantic: "setting-email-alerts-zh",
        label: "邮件提醒",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "通知",
    },
    ScopedControlRegistration {
        semantic: "setting-weekly-digest-en",
        label: "Weekly digest",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "Notifications",
    },
    ScopedControlRegistration {
        semantic: "setting-weekly-digest-zh",
        label: "每周摘要",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "通知",
    },
    ScopedControlRegistration {
        semantic: "setting-include-archived-en",
        label: "Include archived items",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "Search",
    },
    ScopedControlRegistration {
        semantic: "setting-include-archived-zh",
        label: "包含已归档项目",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "搜索",
    },
    ScopedControlRegistration {
        semantic: "setting-quiet-hours-en",
        label: "Quiet hours",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "Notifications",
    },
    ScopedControlRegistration {
        semantic: "setting-quiet-hours-zh",
        label: "免打扰时段",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "通知",
    },
    ScopedControlRegistration {
        semantic: "setting-compact-rows-en",
        label: "Compact rows",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "Display",
    },
    ScopedControlRegistration {
        semantic: "setting-compact-rows-zh",
        label: "紧凑行",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "显示",
    },
    ScopedControlRegistration {
        semantic: "setting-system-show-alerts-en",
        label: "Show alerts",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "System notifications",
    },
    ScopedControlRegistration {
        semantic: "setting-system-show-alerts-zh",
        label: "显示提醒",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Group,
        context_label: "系统通知",
    },
    ScopedControlRegistration {
        semantic: "form-display-name-en",
        label: "Display name",
        role: "AXTextField",
        value_kind: ScopedControlValueKind::Text,
        context_kind: ScopedControlContextKind::Form,
        context_label: "Profile",
    },
    ScopedControlRegistration {
        semantic: "form-display-name-zh",
        label: "显示名称",
        role: "AXTextField",
        value_kind: ScopedControlValueKind::Text,
        context_kind: ScopedControlContextKind::Form,
        context_label: "个人资料",
    },
    ScopedControlRegistration {
        semantic: "form-notes-en",
        label: "Notes",
        role: "AXTextArea",
        value_kind: ScopedControlValueKind::Text,
        context_kind: ScopedControlContextKind::Form,
        context_label: "Profile",
    },
    ScopedControlRegistration {
        semantic: "form-notes-zh",
        label: "备注",
        role: "AXTextArea",
        value_kind: ScopedControlValueKind::Text,
        context_kind: ScopedControlContextKind::Form,
        context_label: "个人资料",
    },
    ScopedControlRegistration {
        semantic: "form-notify-on-change-en",
        label: "Notify on change",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Form,
        context_label: "Preferences",
    },
    ScopedControlRegistration {
        semantic: "form-notify-on-change-zh",
        label: "更改时通知",
        role: "AXCheckBox",
        value_kind: ScopedControlValueKind::Bool,
        context_kind: ScopedControlContextKind::Form,
        context_label: "偏好设置",
    },
    ScopedControlRegistration {
        semantic: "form-reminder-cadence-en",
        label: "Reminder cadence",
        role: "AXPopUpButton",
        value_kind: ScopedControlValueKind::Choice(&["Daily", "Weekly", "Monthly"]),
        context_kind: ScopedControlContextKind::Form,
        context_label: "Preferences",
    },
    ScopedControlRegistration {
        semantic: "form-reminder-cadence-zh",
        label: "提醒频率",
        role: "AXPopUpButton",
        value_kind: ScopedControlValueKind::Choice(&["每天", "每周", "每月"]),
        context_kind: ScopedControlContextKind::Form,
        context_label: "偏好设置",
    },
    ScopedControlRegistration {
        semantic: "form-summary-grouping-en",
        label: "Summary grouping",
        role: "AXRadioGroup",
        value_kind: ScopedControlValueKind::RadioChoice(&["By day", "By week", "By month"]),
        context_kind: ScopedControlContextKind::Form,
        context_label: "Preferences",
    },
    ScopedControlRegistration {
        semantic: "form-summary-grouping-zh",
        label: "摘要分组",
        role: "AXRadioGroup",
        value_kind: ScopedControlValueKind::RadioChoice(&["按天", "按周", "按月"]),
        context_kind: ScopedControlContextKind::Form,
        context_label: "偏好设置",
    },
    ScopedControlRegistration {
        semantic: "form-region-code-en",
        label: "Region code",
        role: "AXTextField",
        value_kind: ScopedControlValueKind::Text,
        context_kind: ScopedControlContextKind::Form,
        context_label: "Account",
    },
    ScopedControlRegistration {
        semantic: "form-region-code-zh",
        label: "地区代码",
        role: "AXTextField",
        value_kind: ScopedControlValueKind::Text,
        context_kind: ScopedControlContextKind::Form,
        context_label: "账户",
    },
    ScopedControlRegistration {
        semantic: "form-alert-threshold-en",
        label: "Alert threshold",
        role: "AXTextField",
        value_kind: ScopedControlValueKind::Number,
        context_kind: ScopedControlContextKind::Form,
        context_label: "Advanced preferences",
    },
    ScopedControlRegistration {
        semantic: "form-alert-threshold-zh",
        label: "提醒阈值",
        role: "AXTextField",
        value_kind: ScopedControlValueKind::Number,
        context_kind: ScopedControlContextKind::Form,
        context_label: "高级偏好设置",
    },
];

#[derive(Debug, Clone, Copy)]
struct ScopedFormScrollRegistration {
    control_semantic: &'static str,
    container_semantic: &'static str,
    container_label: &'static str,
}

const MVP_SCOPED_FORM_SCROLLS: [ScopedFormScrollRegistration; 2] = [
    ScopedFormScrollRegistration {
        control_semantic: "form-alert-threshold-en",
        container_semantic: "scroll-preferences",
        container_label: "Preferences content",
    },
    ScopedFormScrollRegistration {
        control_semantic: "form-alert-threshold-zh",
        container_semantic: "scroll-preferences-zh",
        container_label: "偏好设置内容",
    },
];

const MVP_SCOPED_VALIDATION_FEEDBACK: [ScopedValidationFeedback; 2] = [
    ScopedValidationFeedback {
        control_semantic: "form-region-code-en",
        role: "AXStaticText",
        description: "fixture-region-code-validation",
        context_label: "Account",
        valid_text: "Region code is valid",
        invalid_text: "Enter a region code in the form CA-1234",
    },
    ScopedValidationFeedback {
        control_semantic: "form-region-code-zh",
        role: "AXStaticText",
        description: "fixture-region-code-validation",
        context_label: "账户",
        valid_text: "地区代码有效",
        invalid_text: "请输入 CA-1234 格式的地区代码",
    },
];

#[derive(Debug, Clone, Copy)]
struct ScopedCommitRegistration {
    control_semantic: &'static str,
    action_semantic: Option<&'static str>,
    action_label: Option<&'static str>,
    receipt_group: &'static str,
    kind: ScopedCommitKind,
    text_prefix: &'static str,
}

const MVP_SCOPED_COMMITS: [ScopedCommitRegistration; 4] = [
    ScopedCommitRegistration {
        control_semantic: "setting-include-archived-en",
        action_semantic: Some("save-include-archived-en"),
        action_label: Some("Save"),
        receipt_group: "Search",
        kind: ScopedCommitKind::Manual,
        text_prefix: "Saved manually · revision ",
    },
    ScopedCommitRegistration {
        control_semantic: "setting-include-archived-zh",
        action_semantic: Some("save-include-archived-zh"),
        action_label: Some("保存"),
        receipt_group: "搜索",
        kind: ScopedCommitKind::Manual,
        text_prefix: "已手动保存 · 版本 ",
    },
    ScopedCommitRegistration {
        control_semantic: "setting-quiet-hours-en",
        action_semantic: None,
        action_label: None,
        receipt_group: "Notifications",
        kind: ScopedCommitKind::Automatic,
        text_prefix: "Saved automatically · revision ",
    },
    ScopedCommitRegistration {
        control_semantic: "setting-quiet-hours-zh",
        action_semantic: None,
        action_label: None,
        receipt_group: "通知",
        kind: ScopedCommitKind::Automatic,
        text_prefix: "已自动保存 · 版本 ",
    },
];

const MVP_COMMIT_RECEIPT_DESCRIPTION: &str = "fixture-persistence-receipt";

/// Static registered semantics for proposal construction. No live observations,
/// values, process/window identities or execution capabilities are exported.
/// The catalog is bounded independently of the model context's outer limit.
pub(crate) fn planning_catalog(packs: &[AppPack]) -> String {
    const LIMIT: usize = 16 * 1024;
    let mut apps = Vec::new();
    for pack in packs.iter().take(16) {
        if pack.bundle_id.len() > 256 || pack.name.len() > 256 || pack.nav_role.len() > 128 {
            continue;
        }
        let controls: Vec<_> = pack
            .controls
            .iter()
            .take(64)
            .filter(|c| c.semantic.len() <= 256 && c.label.len() <= 512 && c.role.len() <= 128)
            .map(|c| {
                let mut entry = serde_json::json!({
                    "semantic_target": c.semantic,
                    "label": c.label,
                    "role_hint": c.role,
                    "registered_effect": pack.effect_class(&c.role, &c.label)
                });
                if pack.bundle_id == MVP_FIXTURE_BUNDLE
                    && let Some(registration) = MVP_SCOPED_CONTROLS
                        .iter()
                        .find(|registration| registration.semantic == c.semantic)
                {
                    let context_kind = match registration.context_kind {
                        ScopedControlContextKind::Group => "group",
                        ScopedControlContextKind::Form => "form",
                        ScopedControlContextKind::Unspecified => "unspecified",
                    };
                    let value_kind = match registration.value_kind {
                        ScopedControlValueKind::Bool => "bool",
                        ScopedControlValueKind::Text => "text",
                        ScopedControlValueKind::Number => "number",
                        ScopedControlValueKind::Choice(_) => "choice",
                        ScopedControlValueKind::RadioChoice(_) => "radio_choice",
                    };
                    entry["scope_context"] = serde_json::json!({
                        "kind": context_kind,
                        "label": registration.context_label
                    });
                    entry["value_kind"] = serde_json::json!(value_kind);
                    match registration.value_kind {
                        ScopedControlValueKind::Choice(choices)
                        | ScopedControlValueKind::RadioChoice(choices) => {
                            entry["choice_values"] = serde_json::json!(choices);
                        }
                        ScopedControlValueKind::Bool
                        | ScopedControlValueKind::Text
                        | ScopedControlValueKind::Number => {}
                    }
                }
                entry
            })
            .collect();
        let rules: Vec<_> = pack.rules.iter().take(16)
            .filter(|r| r.label_contains.len() <= 512 && r.role.as_ref().is_none_or(|v| v.len() <= 128))
            .map(|r| serde_json::json!({"role": r.role, "label_contains": r.label_contains, "class": r.class}))
            .collect();
        let row_operations: Vec<_> = pack
            .row_operations()
            .iter()
            .take(8)
            .map(|operation| {
                serde_json::json!({
                    "semantic_target": &operation.semantic_target,
                    "table_scope": &operation.table_scope,
                    "selected_effect": operation.selection_effect,
                    "selection": "AXSelected=true"
                })
            })
            .collect();
        apps.push(serde_json::json!({"app": pack.bundle_id, "name": pack.name,
            "controls": controls, "effect_rules": rules, "row_operations": row_operations,
            "navigation_role": pack.nav_role}));
        if serde_json::to_vec(&apps)
            .expect("catalog values serialize")
            .len()
            + 128
            > LIMIT
        {
            apps.pop();
            break;
        }
    }
    serde_json::json!({"kind": "registered_app_catalog", "version": 1, "apps": apps}).to_string()
}

impl AppPack {
    /// The closed local browser fixture's only executable semantic target.
    /// This registers the effect class without pretending the page is a
    /// native application or native Source observation.
    pub fn browser_fixture() -> Self {
        Self {
            bundle_id: "browser.fixture".into(),
            name: "Browser fixture".into(),
            rules: vec![EffectRule {
                role: Some("checkbox".into()),
                label_contains: "background option".into(),
                class: EffectClass::ReversibleEdit,
            }],
            controls: vec![ControlDef {
                semantic: "browser.fixture.background_option".into(),
                label: "Background option".into(),
                role: "checkbox".into(),
            }],
            row_operations: Vec::new(),
            nav_role: "tab".into(),
            save_state_description: None,
        }
    }

    pub fn registered_native(bundle: &str) -> Option<Self> {
        let (name, role, label, nav) = match bundle {
            MVP_FIXTURE_BUNDLE => ("Interaction Fixture", "AXTextField", "", "AXRadioButton"),
            "com.lingxi.computer-user.guard-fixture" => (
                "Computer Use Guard Fixture",
                "AXTextField",
                "mvp document",
                "AXRadioButton",
            ),
            "com.apple.TextEdit" => ("TextEdit", "AXTextArea", "", "AXRadioButton"),
            _ => return None,
        };
        let mut pack = Self::observe_only(bundle);
        if bundle == MVP_FIXTURE_BUNDLE {
            pack.row_operations = mvp_fixture_row_operations();
        }
        pack.name = name.into();
        pack.nav_role = nav.into();
        pack.rules.push(EffectRule {
            role: Some(role.into()),
            label_contains: label.into(),
            class: EffectClass::ReversibleEdit,
        });
        if bundle == MVP_FIXTURE_BUNDLE {
            // Exact page controls are the only navigation radios in this app.
            // Register them before the general radio-edit rule so a live
            // selected radio cannot make the summary-grouping radios look like
            // page navigation.
            for page in ["settings", "overview", "设置", "概览"] {
                pack.rules.push(EffectRule {
                    role: Some("AXRadioButton".into()),
                    label_contains: page.into(),
                    class: EffectClass::Navigation,
                });
            }
            // `registered` normalizes the observed label to lowercase before
            // matching rules, so keep the English literal normalized here.
            for label in ["summary grouping", "摘要分组"] {
                pack.rules.push(EffectRule {
                    role: Some("AXRadioGroup".into()),
                    label_contains: label.into(),
                    class: EffectClass::ReversibleEdit,
                });
            }
            for (role, class) in [
                ("AXTextArea", EffectClass::ReversibleEdit),
                ("AXCheckBox", EffectClass::ReversibleEdit),
                ("AXPopUpButton", EffectClass::ReversibleEdit),
                ("AXRadioButton", EffectClass::ReversibleEdit),
                ("AXScrollArea", EffectClass::Navigation),
            ] {
                pack.rules.push(EffectRule {
                    role: Some(role.into()),
                    label_contains: String::new(),
                    class,
                });
            }
            for label in ["save", "保存"] {
                pack.rules.push(EffectRule {
                    role: Some("AXButton".into()),
                    label_contains: label.into(),
                    class: EffectClass::Commit,
                });
            }
            pack.rules.push(EffectRule {
                role: Some("AXMenuItem".into()),
                label_contains: String::new(),
                class: EffectClass::ReversibleEdit,
            });

            for (semantic, label, role) in [
                ("page-settings", "Settings", "AXRadioButton"),
                ("page-overview", "Overview", "AXRadioButton"),
                ("page-settings-zh", "设置", "AXRadioButton"),
                ("page-overview-zh", "概览", "AXRadioButton"),
                ("scroll-preferences", "Preferences content", "AXScrollArea"),
                ("scroll-preferences-zh", "偏好设置内容", "AXScrollArea"),
                ("scroll-saved-views", "Saved views", "AXScrollArea"),
                ("scroll-saved-views-zh", "已保存视图", "AXScrollArea"),
                ("scroll-projects", "Projects", "AXScrollArea"),
                ("scroll-projects-zh", "项目", "AXScrollArea"),
                ("scroll-archive", "Archive", "AXScrollArea"),
                ("scroll-archive-zh", "归档", "AXScrollArea"),
                ("scroll-records", "Records", "AXScrollArea"),
                ("scroll-records-zh", "记录", "AXScrollArea"),
            ] {
                pack.controls.push(ControlDef {
                    semantic: semantic.into(),
                    label: label.into(),
                    role: role.into(),
                });
            }
            pack.controls
                .extend(MVP_SCOPED_CONTROLS.iter().map(|control| ControlDef {
                    semantic: control.semantic.into(),
                    label: control.label.into(),
                    role: control.role.into(),
                }));
            pack.controls
                .extend(MVP_SCOPED_COMMITS.iter().filter_map(|commit| {
                    Some(ControlDef {
                        semantic: commit.action_semantic?.into(),
                        label: commit.action_label?.into(),
                        role: "AXButton".into(),
                    })
                }));
        } else if bundle == "com.lingxi.computer-user.guard-fixture" {
            pack.rules.push(EffectRule {
                role: Some(nav.into()),
                label_contains: "".into(),
                class: EffectClass::Navigation,
            });
            pack.controls.push(ControlDef {
                semantic: "document".into(),
                label: "MVP document".into(),
                role: role.into(),
            });
            pack.rules.push(EffectRule {
                role: Some("AXCheckBox".into()),
                label_contains: "mvp toggle".into(),
                class: EffectClass::ReversibleEdit,
            });
            pack.controls.push(ControlDef {
                semantic: "keyboard-toggle".into(),
                label: "Key fixture toggle".into(),
                role: "AXCheckBox".into(),
            });
            pack.controls.push(ControlDef {
                semantic: "toggle".into(),
                label: "MVP toggle".into(),
                role: "AXCheckBox".into(),
            });
            pack.controls.push(ControlDef {
                semantic: "scroll-destination".into(),
                label: "MVP scroll destination".into(),
                role: "AXCheckBox".into(),
            });
            pack.rules.push(EffectRule {
                role: Some("AXCheckBox".into()),
                label_contains: "mvp scroll destination".into(),
                class: EffectClass::ReversibleEdit,
            });
            pack.controls.push(ControlDef {
                semantic: "scroll-document".into(),
                label: "MVP scroll document".into(),
                role: "AXScrollArea".into(),
            });
            pack.rules.push(EffectRule {
                role: Some("AXScrollArea".into()),
                label_contains: "mvp scroll document".into(),
                class: EffectClass::Navigation,
            });
            pack.controls.push(ControlDef {
                semantic: "menu-control".into(),
                label: "Fixture native menu".into(),
                role: "AXMenuButton".into(),
            });
            pack.rules.push(EffectRule {
                role: Some("AXMenuButton".into()),
                label_contains: "fixture native menu".into(),
                class: EffectClass::ReversibleEdit,
            });
            for label in ["fixture menu first item", "fixture menu second item"] {
                pack.rules.push(EffectRule {
                    role: Some("AXMenuItem".into()),
                    label_contains: label.into(),
                    class: EffectClass::ReversibleEdit,
                });
            }
            // Kept on the isolated guard fixture so BackgroundAX eligibility
            // cannot leak into the ordinary MVP fixture profile.
            pack.controls.push(ControlDef {
                semantic: "background-toggle".into(),
                label: "Background toggle".into(),
                role: "AXCheckBox".into(),
            });
            pack.rules.push(EffectRule {
                role: Some("AXCheckBox".into()),
                label_contains: "background toggle".into(),
                class: EffectClass::ReversibleEdit,
            });
        }
        Some(pack)
    }
    /// The pack for the simulated settings application.
    pub fn settings_fixture() -> Self {
        let rule = |role: Option<&str>, label: &str, class| EffectRule {
            role: role.map(String::from),
            label_contains: label.into(),
            class,
        };
        Self {
            bundle_id: "test.settings".into(),
            name: "Settings".into(),
            rules: vec![
                rule(Some("button"), "apply", EffectClass::Commit),
                rule(Some("button"), "save", EffectClass::Commit),
                rule(Some("button"), "reset", EffectClass::Destructive),
                rule(Some("button"), "discard", EffectClass::Destructive),
                rule(Some("button"), "cancel", EffectClass::Navigation),
            ],
            controls: vec![
                ControlDef {
                    semantic: "apply-button".into(),
                    label: "Apply".into(),
                    role: "button".into(),
                },
                ControlDef {
                    semantic: "reset-button".into(),
                    label: "Reset to defaults".into(),
                    role: "button".into(),
                },
            ],
            row_operations: Vec::new(),
            nav_role: "tab".into(),
            save_state_description: Some("save-state".into()),
        }
    }

    /// A pack for an application that is not on the execution whitelist. It
    /// knows nothing about the application, so every control is of unknown
    /// effect and nothing in it can be acted on; it only lets the page be read.
    pub fn observe_only(bundle_id: &str) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            name: bundle_id.into(),
            rules: vec![],
            controls: vec![],
            row_operations: Vec::new(),
            nav_role: "tab".into(),
            save_state_description: None,
        }
    }

    fn role_default(role: &str) -> EffectClass {
        match role {
            "switch" | "checkbox" | "radio" | "popup" | "textfield" | "slider" => {
                EffectClass::ReversibleEdit
            }
            "tab" | "link" | "disclosure" | "row" => EffectClass::Navigation,
            "text" | "group" | "sheet" | "window" => EffectClass::ReadOnly,
            _ => EffectClass::Unknown,
        }
    }

    fn inferred(label: &str) -> EffectClass {
        let label = normalize_label(label);
        let has = |words: &[&str]| words.iter().any(|word| label.contains(word));
        if has(&EXTERNAL_WORDS) {
            EffectClass::External
        } else if has(&DESTRUCTIVE_WORDS) {
            EffectClass::Destructive
        } else {
            EffectClass::ReadOnly
        }
    }

    fn registered(&self, role: Option<&str>, label: &str) -> Option<EffectClass> {
        let label = normalize_label(label);
        if self.bundle_id == "com.lingxi.computer-user.guard-fixture"
            && label == "key fixture toggle"
            && role.is_none_or(|role| role == "AXCheckBox")
            && self.controls.iter().any(|control| {
                control.semantic == "keyboard-toggle"
                    && control.role == "AXCheckBox"
                    && control.label == "Key fixture toggle"
            })
        {
            return Some(EffectClass::ReversibleEdit);
        }
        self.rules
            .iter()
            // A rule with a role only narrows the match when the caller knows the role.
            .filter(|rule| match (rule.role.as_deref(), role) {
                (Some(rule_role), Some(role)) => rule_role == role,
                _ => true,
            })
            .find(|rule| label.contains(&rule.label_contains))
            .map(|rule| rule.class)
    }

    /// The effect class of acting on one control.
    pub fn effect_class(&self, role: &str, label: &str) -> EffectClass {
        let trusted = self
            .registered(Some(role), label)
            .unwrap_or_else(|| Self::role_default(role));
        trusted.raise(Self::inferred(label))
    }

    /// Whether a live object is a registered page-navigation control.
    /// Existing packs keep their prior role-based navigation behavior; the
    /// Interaction Fixture narrows it to its two labeled, pressable page radios.
    pub fn is_navigation_target(&self, object: &UiObject) -> bool {
        if self.bundle_id != MVP_FIXTURE_BUNDLE {
            return object.role == self.nav_role;
        }
        mvp_page_name(object_label(object)).is_some()
            && object.role == "AXRadioButton"
            && object.actions.contains(&AxActionName::Press)
    }

    /// The stable page name represented by a live navigation control.
    /// Localized fixture labels map to the same canonical page; other packs
    /// retain their original label-based behavior.
    pub fn navigation_page(&self, object: &UiObject) -> Option<String> {
        if !self.is_navigation_target(object) {
            return None;
        }
        if self.bundle_id == MVP_FIXTURE_BUNDLE {
            return mvp_page_name(object_label(object)).map(str::to_owned);
        }
        let label = object_label(object);
        (!label.is_empty()).then(|| label.to_owned())
    }

    /// The registered control a plan step names, if the pack knows it.
    pub fn control(&self, target: &SemanticTargetRef) -> Option<&ControlDef> {
        self.controls
            .iter()
            .find(|control| control.semantic == target.as_str())
    }

    /// Resolve one exact frozen primary control from this trusted pack.
    /// The local fixture requires its canonical label, context kind, context
    /// label and role; other packs retain only the old unique ungrouped boolean
    /// lookup. Form controls are deliberately limited to their static entries.
    pub fn scoped_control(&self, target: &AllowedControlTarget) -> Option<ScopedControl<'_>> {
        target.validate().ok()?;
        let label = normalize_label(&target.control);
        if self.bundle_id == MVP_FIXTURE_BUNDLE {
            let context_kind = if target.form.is_some() {
                ScopedControlContextKind::Form
            } else if target.group.is_some() {
                ScopedControlContextKind::Group
            } else {
                return None;
            };
            let context = normalize_label(target.context_label()?);
            let mut matching = MVP_SCOPED_CONTROLS.iter().filter(|registered| {
                registered.context_kind == context_kind
                    && normalize_label(registered.label) == label
                    && normalize_label(registered.context_label) == context
            });
            let registration = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            let control = self.unique_control_for_semantic(registration.semantic)?;
            if control.label != registration.label || control.role != registration.role {
                return None;
            }
            return Some(ScopedControl {
                control,
                value_kind: registration.value_kind,
                context_kind: registration.context_kind,
                context_label: Some(registration.context_label),
            });
        }

        if target.group.is_some() || target.form.is_some() || target.disambiguator.is_some() {
            return None;
        }
        let mut matching = self.controls.iter().filter(|control| {
            normalize_label(&control.label) == label
                && matches!(
                    control.role.as_str(),
                    "button"
                        | "checkbox"
                        | "radio button"
                        | "radiobutton"
                        | "switch"
                        | "AXButton"
                        | "AXCheckBox"
                        | "AXRadioButton"
                        | "AXSwitch"
                )
        });
        let control = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        Some(ScopedControl {
            control,
            value_kind: ScopedControlValueKind::Bool,
            context_kind: ScopedControlContextKind::Unspecified,
            context_label: None,
        })
    }

    /// Return trusted validation evidence only for the exact localized Region
    /// code form control. The Source reader still has to establish that one
    /// matching visible feedback object exists in the current complete read.
    pub fn scoped_validation_feedback(
        &self,
        target: &AllowedControlTarget,
    ) -> Option<ScopedValidationFeedback> {
        let scoped = self.scoped_control(target)?;
        if scoped.context_kind != ScopedControlContextKind::Form
            || scoped.value_kind != ScopedControlValueKind::Text
            || scoped.control.role != "AXTextField"
        {
            return None;
        }
        let feedback = MVP_SCOPED_VALIDATION_FEEDBACK
            .iter()
            .find(|feedback| feedback.control_semantic == scoped.control.semantic)?;
        let control = self.unique_control_for_semantic(feedback.control_semantic)?;
        if target.control != control.label
            || target.context_label() != Some(feedback.context_label)
            || control.role != "AXTextField"
            || control.semantic != scoped.control.semantic
            || scoped.context_label != Some(feedback.context_label)
        {
            return None;
        }
        Some(*feedback)
    }

    /// The one registered scroll container that may prepare an offscreen
    /// numeric form field. The public plan still names the frozen field as its
    /// destination; this helper only supplies the Host-owned container.
    pub fn scoped_form_scroll_container(
        &self,
        target: &AllowedControlTarget,
    ) -> Option<&ControlDef> {
        let scoped = self.scoped_control(target)?;
        if scoped.context_kind != ScopedControlContextKind::Form
            || scoped.value_kind != ScopedControlValueKind::Number
            || scoped.control.role != "AXTextField"
            || target.control != scoped.control.label
            || target.context_label() != scoped.context_label
        {
            return None;
        }
        let registration = MVP_SCOPED_FORM_SCROLLS
            .iter()
            .find(|entry| entry.control_semantic == scoped.control.semantic)?;
        let container = self.unique_control_for_semantic(registration.container_semantic)?;
        (container.role == "AXScrollArea" && container.label == registration.container_label)
            .then_some(container)
    }

    /// Return the one persistence operation frozen for this exact scoped
    /// fixture control. Labels and task request data never create this grant.
    pub fn scoped_commit_binding(&self, control: &ControlDef) -> Option<ScopedCommitBinding> {
        if self.bundle_id != MVP_FIXTURE_BUNDLE {
            return None;
        }
        let registration = MVP_SCOPED_COMMITS
            .iter()
            .find(|commit| commit.control_semantic == control.semantic)?;
        let registered_control = self.unique_control_for_semantic(registration.control_semantic)?;
        let control_registration = MVP_SCOPED_CONTROLS
            .iter()
            .find(|candidate| candidate.semantic == registration.control_semantic)?;
        if control.semantic != registered_control.semantic
            || control.label != registered_control.label
            || control.role != registered_control.role
            || control.label != control_registration.label
            || control.role != control_registration.role
            || control_registration.context_label != registration.receipt_group
        {
            return None;
        }
        let action = match (registration.action_semantic, registration.action_label) {
            (Some(semantic), Some(label)) => {
                let action = self.unique_control_for_semantic(semantic)?;
                if action.role != "AXButton"
                    || action.label != label
                    || self.effect_class(&action.role, &action.label) != EffectClass::Commit
                {
                    return None;
                }
                Some(TargetSpec {
                    semantic_target: SemanticTargetRef::new(semantic).ok()?,
                    label: label.into(),
                    group: Some(registration.receipt_group.into()),
                    role_hint: Some(action.role.clone()),
                    row: None,
                })
            }
            (None, None) => None,
            _ => return None,
        };
        let binding = ScopedCommitBinding {
            action,
            receipt: ScopedCommitReceiptBinding {
                group: registration.receipt_group.into(),
                role_hint: "AXStaticText".into(),
                description: MVP_COMMIT_RECEIPT_DESCRIPTION.into(),
                kind: registration.kind,
                text_prefix: registration.text_prefix.into(),
            },
        };
        binding.validate().ok()?;
        Some(binding)
    }

    /// Static page-navigation targets allowed as the auxiliary path for a
    /// scoped fixture goal. This is separate from the one mutable control in
    /// `AllowedControlScope` and is returned only for explicitly registered
    /// SET and FORM primary scopes.
    pub fn scoped_revisit_pages(&self, control: &ControlDef) -> Option<(&ControlDef, &ControlDef)> {
        if self.bundle_id != MVP_FIXTURE_BUNDLE {
            return None;
        }
        let registration = MVP_SCOPED_CONTROLS
            .iter()
            .find(|registered| registered.semantic == control.semantic)?;
        let registered_control = self.unique_control_for_semantic(registration.semantic)?;
        if control.semantic != registered_control.semantic
            || control.label != registered_control.label
            || control.role != registered_control.role
            || registered_control.label != registration.label
            || registered_control.role != registration.role
        {
            return None;
        }
        let (home, home_label, away, away_label) = if control.semantic.ends_with("-zh") {
            ("page-settings-zh", "设置", "page-overview-zh", "概览")
        } else if control.semantic.ends_with("-en") {
            ("page-settings", "Settings", "page-overview", "Overview")
        } else {
            return None;
        };
        let home = self.unique_page_control(home, home_label)?;
        let away = self.unique_page_control(away, away_label)?;
        Some((home, away))
    }

    fn unique_control_for_semantic(&self, semantic: &str) -> Option<&ControlDef> {
        let mut matching = self
            .controls
            .iter()
            .filter(|control| control.semantic == semantic);
        let control = matching.next()?;
        matching.next().is_none().then_some(control)
    }

    fn unique_page_control(&self, semantic: &str, label: &str) -> Option<&ControlDef> {
        let control = self.unique_control_for_semantic(semantic)?;
        if control.label != label
            || control.role != "AXRadioButton"
            || self.effect_class(&control.role, &control.label) != EffectClass::Navigation
        {
            return None;
        }
        let mut same_label = self.controls.iter().filter(|candidate| {
            candidate.role == "AXRadioButton"
                && normalize_label(&candidate.label) == normalize_label(label)
        });
        let unique = same_label.next()?;
        (unique.semantic == semantic && same_label.next().is_none()).then_some(control)
    }

    /// Static semantic row operations supported by this exact application
    /// profile. The fixture scope strings mirror its public scroll labels and
    /// full column headers; dynamic row predicates remain in the frozen goal.
    pub fn row_operations(&self) -> &[RowOperationDef] {
        &self.row_operations
    }

    /// Effect class for one selected=true row operation. A role or a dynamic
    /// label cannot create a capability; dangerous label terms only raise the
    /// registered class.
    pub fn row_selection_effect_class(
        &self,
        semantic_target: &SemanticTargetRef,
        row: &RowTargetSpec,
        target_label: &str,
    ) -> Option<EffectClass> {
        row.validate().ok()?;
        if row.conditions.iter().any(|condition| {
            !row.table_scope
                .column_headers
                .contains(&condition.column_name)
        }) {
            return None;
        }
        let operation = self.row_operations().iter().find(|operation| {
            operation.semantic_target == semantic_target.as_str()
                && operation_scope_matches(&operation.table_scope, &row.table_scope)
        })?;
        Some(
            operation
                .selection_effect
                .raise(Self::inferred(target_label)),
        )
    }

    /// One fixed keyboard profile, registered by trusted app data.
    pub fn keyboard_toggle(&self, target: &SemanticTargetRef) -> Option<&ControlDef> {
        if self.bundle_id != "com.lingxi.computer-user.guard-fixture"
            || target.as_str() != "keyboard-toggle"
        {
            return None;
        }
        self.control(target)
            .filter(|control| control.role == "AXCheckBox" && control.label == "Key fixture toggle")
    }

    /// The class for a control known only by a label, such as a plan step.
    pub fn effect_class_for_label(&self, label: &str) -> EffectClass {
        self.registered(None, label)
            .map(|class| class.raise(Self::inferred(label)))
            .unwrap_or(EffectClass::Unknown)
            .raise(Self::inferred(label))
    }

    /// Current page evidence from the active non-modal window, or from the
    /// fixture's complete live selection controls when its title is static.
    pub fn page_of(&self, snapshot: &StructureSnapshot) -> Option<String> {
        let layer = focused_window_id(snapshot)?;
        let main = snapshot.windows.iter().find(|window| {
            window.window_id == layer && !window.modal && window.owner_bundle_id == self.bundle_id
        })?;
        if self.bundle_id == MVP_FIXTURE_BUNDLE {
            if !snapshot.complete {
                return None;
            }
            let controls: Vec<_> = snapshot
                .objects
                .iter()
                .filter(|object| {
                    object.window_id == main.window_id
                        && object.enabled
                        && object.in_view
                        && self.is_navigation_target(object)
                })
                .collect();
            if controls.len() != 2 {
                return None;
            }
            let mut pages = Vec::with_capacity(2);
            let mut selected = None;
            for control in controls {
                let page = mvp_page_name(object_label(control))?;
                let UiValue::Bool(is_selected) = control.value.as_ref()? else {
                    return None;
                };
                if pages.contains(&page) {
                    return None;
                }
                pages.push(page);
                if *is_selected && selected.replace(page).is_some() {
                    return None;
                }
            }
            return selected.map(str::to_owned);
        }
        let title = main.title.as_deref()?;
        Some(
            title
                .rsplit_once(" — ")
                .map_or(title, |(_, page)| page)
                .to_string(),
        )
    }
}

fn mvp_fixture_row_operations() -> Vec<RowOperationDef> {
    let make_scope = |scroll_labels: &[&str], headers: &[&str]| RowTableScope {
        scroll_path: scroll_labels
            .iter()
            .map(|label| RowContainerScope {
                role: RowContainerRole::AxScrollArea,
                label: (*label).into(),
            })
            .collect(),
        column_headers: headers.iter().map(|header| (*header).into()).collect(),
    };
    let make_pair = |semantic: &str,
                     english_path: &[&str],
                     chinese_path: &[&str],
                     english_headers: &[&str],
                     chinese_headers: &[&str]| {
        [
            (english_path, english_headers),
            (chinese_path, chinese_headers),
        ]
        .into_iter()
        .map(|(path, headers)| RowOperationDef {
            semantic_target: semantic.into(),
            table_scope: make_scope(path, headers),
            selection_effect: EffectClass::Navigation,
        })
        .collect::<Vec<_>>()
    };

    [
        make_pair(
            "saved-view-row",
            &["Saved views"],
            &["已保存视图"],
            &["Entry"],
            &["条目"],
        ),
        make_pair(
            "project-row",
            &["Preferences content", "Projects"],
            &["偏好设置内容", "项目"],
            &["Name", "Owner", "Status"],
            &["名称", "负责人", "状态"],
        ),
        make_pair(
            "archive-row",
            &["Preferences content", "Archive"],
            &["偏好设置内容", "归档"],
            &["Archive item"],
            &["归档项目"],
        ),
        make_pair(
            "record-row",
            &["Records"],
            &["记录"],
            &["Record", "Status", "Updated"],
            &["记录", "状态", "更新时间"],
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn operation_scope_matches(expected: &RowTableScope, actual: &RowTableScope) -> bool {
    expected.scroll_path == actual.scroll_path
        && expected.column_headers.len() == actual.column_headers.len()
        && expected.column_headers.iter().all(|header| {
            actual
                .column_headers
                .iter()
                .any(|candidate| candidate == header)
        })
}

fn object_label(object: &UiObject) -> &str {
    object
        .title
        .as_deref()
        .or(object.description.as_deref())
        .unwrap_or("")
}

fn mvp_page_name(label: &str) -> Option<&'static str> {
    match normalize_label(label).as_str() {
        "settings" | "设置" => Some("Settings"),
        "overview" | "概览" => Some("Overview"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::task::{
        AllowedControlTarget, ExpectedValue, RowColumnConditionSpec,
    };

    #[test]
    fn planning_catalog_contains_only_bounded_registered_semantics() {
        let fixture = AppPack::registered_native("com.lingxi.computer-user.guard-fixture").unwrap();
        let catalog = planning_catalog(&[fixture]);
        let value: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let app = &value["apps"][0];
        assert_eq!(app["controls"][0]["semantic_target"], "document");
        assert_eq!(app["controls"][0]["registered_effect"], "reversible_edit");
        assert_eq!(app["controls"][0]["role_hint"], "AXTextField");
        assert_eq!(app["navigation_role"], "AXRadioButton");
        for field in ["pid", "window_id", "value", "screenshot", "api_key"] {
            assert!(!catalog.contains(&format!("\"{field}\"")));
        }
        let mut large = AppPack::settings_fixture();
        large.rules[0].label_contains = "x".repeat(512);
        large.rules = vec![large.rules[0].clone(); 16];
        large.controls[0].label = "x".repeat(512);
        large.controls = vec![large.controls[0].clone(); 16];
        let overflow_catalog = planning_catalog(&vec![large; 100]);
        assert!(overflow_catalog.len() <= 16 * 1024);
        let overflow_value: serde_json::Value = serde_json::from_str(&overflow_catalog).unwrap();
        assert!(overflow_value["apps"].as_array().unwrap().is_empty());
    }

    #[test]
    fn background_toggle_is_registered_only_on_the_guard_fixture() {
        let semantic = SemanticTargetRef::new("background-toggle").unwrap();
        let guard = AppPack::registered_native("com.lingxi.computer-user.guard-fixture").unwrap();
        let control = guard.control(&semantic).unwrap();
        assert_eq!(control.label, "Background toggle");
        assert_eq!(control.role, "AXCheckBox");
        assert_eq!(
            guard.effect_class(&control.role, &control.label),
            EffectClass::ReversibleEdit
        );

        let mvp = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        assert!(mvp.control(&semantic).is_none());
    }

    fn allowed_control(
        control: &str,
        group: &str,
        disambiguator: Option<&str>,
    ) -> AllowedControlTarget {
        AllowedControlTarget {
            control: control.into(),
            group: Some(group.into()),
            form: None,
            disambiguator: disambiguator.map(str::to_owned),
        }
    }

    fn allowed_form(control: &str, form: &str) -> AllowedControlTarget {
        AllowedControlTarget {
            control: control.into(),
            group: None,
            form: Some(form.into()),
            disambiguator: None,
        }
    }

    #[test]
    fn scoped_fixture_settings_require_exact_registered_label_group_and_role() {
        let pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let cases = [
            ("Email alerts", "Notifications", "setting-email-alerts-en"),
            ("邮件提醒", "通知", "setting-email-alerts-zh"),
            ("Weekly digest", "Notifications", "setting-weekly-digest-en"),
            ("每周摘要", "通知", "setting-weekly-digest-zh"),
            (
                "Include archived items",
                "Search",
                "setting-include-archived-en",
            ),
            ("包含已归档项目", "搜索", "setting-include-archived-zh"),
            ("Quiet hours", "Notifications", "setting-quiet-hours-en"),
            ("免打扰时段", "通知", "setting-quiet-hours-zh"),
            ("Compact rows", "Display", "setting-compact-rows-en"),
            ("紧凑行", "显示", "setting-compact-rows-zh"),
            (
                "Show alerts",
                "System notifications",
                "setting-system-show-alerts-en",
            ),
            ("显示提醒", "系统通知", "setting-system-show-alerts-zh"),
        ];
        for (label, group, semantic) in cases {
            let scoped = pack
                .scoped_control(&allowed_control(label, group, Some(group)))
                .expect("the exact fixture checkbox is registered");
            assert_eq!(scoped.value_kind, ScopedControlValueKind::Bool);
            assert_eq!(scoped.context_kind, ScopedControlContextKind::Group);
            assert_eq!(scoped.context_label, Some(group));
            let control = scoped.control;
            assert_eq!(control.semantic, semantic);
            assert_eq!(control.label, label);
            assert_eq!(control.role, "AXCheckBox");
            assert!(pack.scoped_revisit_pages(control).is_some());
        }

        for unsupported in [
            allowed_control("Weekly digest", "Display", None),
            allowed_control("Show alerts", "Workspace notifications", None),
            allowed_control("每周摘要", "Notifications", None),
        ] {
            assert!(pack.scoped_control(&unsupported).is_none());
        }
    }

    #[test]
    fn scoped_commit_bindings_are_exact_manual_or_automatic_fixture_capabilities() {
        let pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        for (label, group, action_semantic, kind, prefix) in [
            (
                "Include archived items",
                "Search",
                Some("save-include-archived-en"),
                ScopedCommitKind::Manual,
                "Saved manually · revision ",
            ),
            (
                "包含已归档项目",
                "搜索",
                Some("save-include-archived-zh"),
                ScopedCommitKind::Manual,
                "已手动保存 · 版本 ",
            ),
            (
                "Quiet hours",
                "Notifications",
                None,
                ScopedCommitKind::Automatic,
                "Saved automatically · revision ",
            ),
            (
                "免打扰时段",
                "通知",
                None,
                ScopedCommitKind::Automatic,
                "已自动保存 · 版本 ",
            ),
        ] {
            let scoped = pack
                .scoped_control(&allowed_control(label, group, Some(group)))
                .unwrap();
            let binding = pack.scoped_commit_binding(scoped.control).unwrap();
            assert_eq!(binding.receipt.group, group);
            assert_eq!(binding.receipt.role_hint, "AXStaticText");
            assert_eq!(binding.receipt.description, MVP_COMMIT_RECEIPT_DESCRIPTION);
            assert_eq!(binding.receipt.kind, kind);
            assert_eq!(binding.receipt.text_prefix, prefix);
            assert_eq!(
                binding
                    .action
                    .as_ref()
                    .map(|action| action.semantic_target.as_str()),
                action_semantic
            );
            assert!(binding.validate().is_ok());
        }

        let mut altered_control = pack
            .control(&SemanticTargetRef::new("setting-include-archived-en").unwrap())
            .unwrap()
            .clone();
        altered_control.label = "Other setting".into();
        assert!(pack.scoped_commit_binding(&altered_control).is_none());

        let mut duplicate_save = pack.clone();
        duplicate_save.controls.push(ControlDef {
            semantic: "save-include-archived-en".into(),
            label: "Save".into(),
            role: "AXButton".into(),
        });
        let exact_control = duplicate_save
            .control(&SemanticTargetRef::new("setting-include-archived-en").unwrap())
            .unwrap()
            .clone();
        assert!(
            duplicate_save
                .scoped_commit_binding(&exact_control)
                .is_none()
        );
    }

    #[test]
    fn scoped_fixture_forms_resolve_localized_context_role_and_value_kind() {
        let pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let cases = [
            (
                "Display name",
                "Profile",
                "form-display-name-en",
                "AXTextField",
                ScopedControlValueKind::Text,
                "Settings",
                "Overview",
            ),
            (
                "显示名称",
                "个人资料",
                "form-display-name-zh",
                "AXTextField",
                ScopedControlValueKind::Text,
                "设置",
                "概览",
            ),
            (
                "Notes",
                "Profile",
                "form-notes-en",
                "AXTextArea",
                ScopedControlValueKind::Text,
                "Settings",
                "Overview",
            ),
            (
                "备注",
                "个人资料",
                "form-notes-zh",
                "AXTextArea",
                ScopedControlValueKind::Text,
                "设置",
                "概览",
            ),
            (
                "Alert threshold",
                "Advanced preferences",
                "form-alert-threshold-en",
                "AXTextField",
                ScopedControlValueKind::Number,
                "Settings",
                "Overview",
            ),
            (
                "提醒阈值",
                "高级偏好设置",
                "form-alert-threshold-zh",
                "AXTextField",
                ScopedControlValueKind::Number,
                "设置",
                "概览",
            ),
            (
                "Notify on change",
                "Preferences",
                "form-notify-on-change-en",
                "AXCheckBox",
                ScopedControlValueKind::Bool,
                "Settings",
                "Overview",
            ),
            (
                "更改时通知",
                "偏好设置",
                "form-notify-on-change-zh",
                "AXCheckBox",
                ScopedControlValueKind::Bool,
                "设置",
                "概览",
            ),
        ];
        for (label, form, semantic, role, value_kind, home, away) in cases {
            let scoped = pack
                .scoped_control(&allowed_form(label, form))
                .expect("the exact localized fixture form field is registered");
            assert_eq!(scoped.control.semantic, semantic);
            assert_eq!(scoped.control.label, label);
            assert_eq!(scoped.control.role, role);
            assert_eq!(scoped.value_kind, value_kind);
            assert_eq!(scoped.context_kind, ScopedControlContextKind::Form);
            assert_eq!(scoped.context_label, Some(form));
            let (home_control, away_control) = pack.scoped_revisit_pages(scoped.control).unwrap();
            assert_eq!(home_control.label, home);
            assert_eq!(away_control.label, away);
        }

        let choice_cases = [
            (
                "Reminder cadence",
                "Preferences",
                "form-reminder-cadence-en",
                &["Daily", "Weekly", "Monthly"][..],
                "Settings",
                "Overview",
            ),
            (
                "提醒频率",
                "偏好设置",
                "form-reminder-cadence-zh",
                &["每天", "每周", "每月"][..],
                "设置",
                "概览",
            ),
        ];
        for (label, form, semantic, choices, home, away) in choice_cases {
            let scoped = pack
                .scoped_control(&allowed_form(label, form))
                .expect("the exact localized popup control is registered");
            assert_eq!(scoped.control.semantic, semantic);
            assert_eq!(scoped.control.label, label);
            assert_eq!(scoped.control.role, "AXPopUpButton");
            assert_eq!(scoped.value_kind, ScopedControlValueKind::Choice(choices));
            assert_eq!(scoped.context_kind, ScopedControlContextKind::Form);
            assert_eq!(scoped.context_label, Some(form));
            let (home_control, away_control) = pack.scoped_revisit_pages(scoped.control).unwrap();
            assert_eq!(home_control.label, home);
            assert_eq!(away_control.label, away);
        }

        let radio_choice_cases = [
            (
                "Summary grouping",
                "Preferences",
                "form-summary-grouping-en",
                &["By day", "By week", "By month"][..],
                "Settings",
                "Overview",
            ),
            (
                "摘要分组",
                "偏好设置",
                "form-summary-grouping-zh",
                &["按天", "按周", "按月"][..],
                "设置",
                "概览",
            ),
        ];
        for (label, form, semantic, choices, home, away) in radio_choice_cases {
            let scoped = pack
                .scoped_control(&allowed_form(label, form))
                .expect("the exact localized radio group form is registered");
            assert_eq!(scoped.control.semantic, semantic);
            assert_eq!(scoped.control.label, label);
            assert_eq!(scoped.control.role, "AXRadioGroup");
            assert_eq!(
                scoped.value_kind,
                ScopedControlValueKind::RadioChoice(choices)
            );
            assert_eq!(scoped.context_kind, ScopedControlContextKind::Form);
            assert_eq!(scoped.context_label, Some(form));
            let (home_control, away_control) = pack.scoped_revisit_pages(scoped.control).unwrap();
            assert_eq!(home_control.label, home);
            assert_eq!(away_control.label, away);
        }

        for unsupported in [
            allowed_form("Display name", "Preferences"),
            allowed_form("Notes", "Account"),
            allowed_form("Notify on change", "Profile"),
            allowed_form("Reminder cadence", "Profile"),
            allowed_form("提醒频率", "Preferences"),
            allowed_form("Summary grouping", "Profile"),
            allowed_form("Summary grouping", "Notifications"),
            allowed_form("摘要分组", "Preferences"),
            allowed_form("Email", "Account"),
            allowed_form("Alert threshold", "Profile"),
            allowed_form("Alert threshold", "Account"),
            allowed_form("提醒阈值", "Advanced preferences"),
            allowed_control("Display name", "Profile", None),
        ] {
            assert!(pack.scoped_control(&unsupported).is_none());
        }
        let notify = pack
            .scoped_control(&allowed_form("Notify on change", "Preferences"))
            .unwrap();
        assert_eq!(notify.value_kind, ScopedControlValueKind::Bool);
        assert_eq!(notify.context_kind, ScopedControlContextKind::Form);
    }

    #[test]
    fn numeric_form_scroll_is_bound_to_the_exact_localized_number_target() {
        let pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        for (label, form, semantic, container_semantic, container_label) in [
            (
                "Alert threshold",
                "Advanced preferences",
                "form-alert-threshold-en",
                "scroll-preferences",
                "Preferences content",
            ),
            (
                "提醒阈值",
                "高级偏好设置",
                "form-alert-threshold-zh",
                "scroll-preferences-zh",
                "偏好设置内容",
            ),
        ] {
            let scope = allowed_form(label, form);
            let scoped = pack.scoped_control(&scope).unwrap();
            assert_eq!(scoped.control.semantic, semantic);
            assert_eq!(scoped.value_kind, ScopedControlValueKind::Number);
            let container = pack.scoped_form_scroll_container(&scope).unwrap();
            assert_eq!(container.semantic, container_semantic);
            assert_eq!(container.label, container_label);
            assert_eq!(container.role, "AXScrollArea");
        }

        for unsupported in [
            allowed_form("Alert threshold", "Profile"),
            allowed_form("Alert threshold", "Account"),
            allowed_form("Display name", "Profile"),
            allowed_form("Reminder cadence", "Preferences"),
            allowed_control("Alert threshold", "Advanced preferences", None),
        ] {
            assert!(pack.scoped_form_scroll_container(&unsupported).is_none());
        }
        assert!(
            AppPack::observe_only(MVP_FIXTURE_BUNDLE)
                .scoped_form_scroll_container(&allowed_form(
                    "Alert threshold",
                    "Advanced preferences"
                ))
                .is_none()
        );

        let mut duplicate_container = pack.clone();
        duplicate_container.controls.push(ControlDef {
            semantic: "scroll-preferences".into(),
            label: "Preferences content".into(),
            role: "AXScrollArea".into(),
        });
        assert!(
            duplicate_container
                .scoped_form_scroll_container(&allowed_form(
                    "Alert threshold",
                    "Advanced preferences"
                ))
                .is_none()
        );
    }

    #[test]
    fn region_code_validation_feedback_is_exact_localized_form_metadata() {
        let pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        for (label, form, semantic, valid, invalid) in [
            (
                "Region code",
                "Account",
                "form-region-code-en",
                "Region code is valid",
                "Enter a region code in the form CA-1234",
            ),
            (
                "地区代码",
                "账户",
                "form-region-code-zh",
                "地区代码有效",
                "请输入 CA-1234 格式的地区代码",
            ),
        ] {
            let scope = allowed_form(label, form);
            let scoped = pack.scoped_control(&scope).unwrap();
            assert_eq!(scoped.control.semantic, semantic);
            assert_eq!(scoped.control.label, label);
            assert_eq!(scoped.control.role, "AXTextField");
            assert_eq!(scoped.value_kind, ScopedControlValueKind::Text);
            assert_eq!(scoped.context_kind, ScopedControlContextKind::Form);
            assert_eq!(scoped.context_label, Some(form));

            let feedback = pack.scoped_validation_feedback(&scope).unwrap();
            assert_eq!(feedback.control_semantic, semantic);
            assert_eq!(feedback.role, "AXStaticText");
            assert_eq!(feedback.description, "fixture-region-code-validation");
            assert_eq!(feedback.context_label, form);
            assert_eq!(feedback.valid_text, valid);
            assert_eq!(feedback.invalid_text, invalid);
        }

        for unsupported in [
            allowed_form("Region code", "Profile"),
            allowed_form("Region code", "Preferences"),
            allowed_form("地区代码", "Preferences"),
            allowed_form("Display name", "Profile"),
            allowed_control("Region code", "Account", None),
        ] {
            assert!(pack.scoped_validation_feedback(&unsupported).is_none());
        }
        assert!(
            AppPack::observe_only(MVP_FIXTURE_BUNDLE)
                .scoped_validation_feedback(&allowed_form("Region code", "Account"))
                .is_none()
        );
    }

    #[test]
    fn scoped_form_controls_are_exact_pack_definitions_not_bundle_capabilities() {
        let scope = allowed_form("Display name", "Profile");
        let mut bad_label = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        bad_label
            .controls
            .iter_mut()
            .find(|control| control.semantic == "form-display-name-en")
            .unwrap()
            .label = "Different field".into();
        assert!(bad_label.scoped_control(&scope).is_none());

        let mut bad_role = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        bad_role
            .controls
            .iter_mut()
            .find(|control| control.semantic == "form-display-name-en")
            .unwrap()
            .role = "AXTextArea".into();
        assert!(bad_role.scoped_control(&scope).is_none());

        let mut duplicate_semantic = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        duplicate_semantic.controls.push(ControlDef {
            semantic: "form-display-name-en".into(),
            label: "Display name".into(),
            role: "AXTextField".into(),
        });
        assert!(duplicate_semantic.scoped_control(&scope).is_none());

        let choice_scope = allowed_form("Reminder cadence", "Preferences");
        let mut wrong_choice_role = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        wrong_choice_role
            .controls
            .iter_mut()
            .find(|control| control.semantic == "form-reminder-cadence-en")
            .unwrap()
            .role = "AXTextField".into();
        assert!(wrong_choice_role.scoped_control(&choice_scope).is_none());

        let mut duplicate_choice = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        duplicate_choice.controls.push(ControlDef {
            semantic: "form-reminder-cadence-en".into(),
            label: "Reminder cadence".into(),
            role: "AXPopUpButton".into(),
        });
        assert!(duplicate_choice.scoped_control(&choice_scope).is_none());

        let radio_scope = allowed_form("Summary grouping", "Preferences");
        let mut wrong_radio_group_role = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        wrong_radio_group_role
            .controls
            .iter_mut()
            .find(|control| control.semantic == "form-summary-grouping-en")
            .unwrap()
            .role = "AXRadioButton".into();
        assert!(
            wrong_radio_group_role
                .scoped_control(&radio_scope)
                .is_none()
        );

        let mut duplicate_radio_group = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        duplicate_radio_group.controls.push(ControlDef {
            semantic: "form-summary-grouping-en".into(),
            label: "Summary grouping".into(),
            role: "AXRadioGroup".into(),
        });
        assert!(duplicate_radio_group.scoped_control(&radio_scope).is_none());

        let observation_only = AppPack::observe_only(MVP_FIXTURE_BUNDLE);
        assert!(observation_only.scoped_control(&scope).is_none());
        assert!(
            observation_only
                .scoped_revisit_pages(&ControlDef {
                    semantic: "form-display-name-en".into(),
                    label: "Display name".into(),
                    role: "AXTextField".into(),
                })
                .is_none()
        );
    }

    #[test]
    fn planning_catalog_includes_all_bounded_registered_fixture_form_controls() {
        let fixture = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let expected: Vec<_> = MVP_SCOPED_CONTROLS
            .iter()
            .filter(|control| control.context_kind == ScopedControlContextKind::Form)
            .collect();
        let catalog = planning_catalog(std::slice::from_ref(&fixture));
        let value: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let controls = value["apps"][0]["controls"].as_array().unwrap();
        assert_eq!(controls.len(), fixture.controls.len());
        assert!(catalog.len() <= 16 * 1024);
        for expected in expected {
            assert!(controls.iter().any(|control| {
                let value_kind = match expected.value_kind {
                    ScopedControlValueKind::Bool => "bool",
                    ScopedControlValueKind::Text => "text",
                    ScopedControlValueKind::Number => "number",
                    ScopedControlValueKind::Choice(_) => "choice",
                    ScopedControlValueKind::RadioChoice(_) => "radio_choice",
                };
                let choices_match = match expected.value_kind {
                    ScopedControlValueKind::Choice(choices)
                    | ScopedControlValueKind::RadioChoice(choices) => {
                        control["choice_values"] == serde_json::json!(choices)
                    }
                    ScopedControlValueKind::Bool
                    | ScopedControlValueKind::Text
                    | ScopedControlValueKind::Number => control.get("choice_values").is_none(),
                };
                control["semantic_target"] == expected.semantic
                    && control["label"] == expected.label
                    && control["role_hint"] == expected.role
                    && control["scope_context"]["kind"] == "form"
                    && control["scope_context"]["label"] == expected.context_label
                    && control["value_kind"] == value_kind
                    && choices_match
            }));
        }
    }

    #[test]
    fn scoped_fixture_registration_is_not_inherited_by_an_observation_only_pack() {
        let observe_only = AppPack::observe_only(MVP_FIXTURE_BUNDLE);
        assert!(
            observe_only
                .scoped_control(&allowed_control(
                    "Weekly digest",
                    "Notifications",
                    Some("Notifications"),
                ))
                .is_none()
        );
        assert!(
            observe_only
                .scoped_revisit_pages(&ControlDef {
                    semantic: "setting-weekly-digest-en".into(),
                    label: "Weekly digest".into(),
                    role: "AXCheckBox".into(),
                })
                .is_none()
        );
    }

    #[test]
    fn scoped_fixture_and_navigation_capabilities_require_unique_exact_pack_definitions() {
        let scope = allowed_control("Weekly digest", "Notifications", None);
        let mut bad_scope_pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let bad_scope = bad_scope_pack
            .controls
            .iter_mut()
            .find(|control| control.semantic == "setting-weekly-digest-en")
            .unwrap();
        bad_scope.label = "Different checkbox".into();
        assert!(bad_scope_pack.scoped_control(&scope).is_none());

        let mut bad_role_pack = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        bad_role_pack
            .controls
            .iter_mut()
            .find(|control| control.semantic == "setting-weekly-digest-en")
            .unwrap()
            .role = "AXButton".into();
        assert!(bad_role_pack.scoped_control(&scope).is_none());

        let mut observe_only = AppPack::observe_only(MVP_FIXTURE_BUNDLE);
        observe_only.controls.push(ControlDef {
            semantic: "setting-weekly-digest-en".into(),
            label: "Different checkbox".into(),
            role: "AXCheckBox".into(),
        });
        assert!(observe_only.scoped_control(&scope).is_none());

        let mut bad_navigation = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let target = bad_navigation
            .scoped_control(&scope)
            .unwrap()
            .control
            .clone();
        bad_navigation
            .controls
            .iter_mut()
            .find(|control| control.semantic == "page-overview")
            .unwrap()
            .label = "Other page".into();
        assert!(bad_navigation.scoped_revisit_pages(&target).is_none());

        let mut bad_navigation_role = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let target = bad_navigation_role
            .scoped_control(&scope)
            .unwrap()
            .control
            .clone();
        bad_navigation_role
            .controls
            .iter_mut()
            .find(|control| control.semantic == "page-overview")
            .unwrap()
            .role = "AXButton".into();
        assert!(bad_navigation_role.scoped_revisit_pages(&target).is_none());

        let mut duplicate_navigation = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let target = duplicate_navigation
            .scoped_control(&scope)
            .unwrap()
            .control
            .clone();
        duplicate_navigation.controls.push(ControlDef {
            semantic: "duplicate-overview".into(),
            label: "Overview".into(),
            role: "AXRadioButton".into(),
        });
        assert!(duplicate_navigation.scoped_revisit_pages(&target).is_none());

        let mut duplicate_semantic = AppPack::registered_native(MVP_FIXTURE_BUNDLE).unwrap();
        let target = duplicate_semantic
            .scoped_control(&scope)
            .unwrap()
            .control
            .clone();
        duplicate_semantic.controls.push(ControlDef {
            semantic: "page-settings".into(),
            label: "Settings".into(),
            role: "AXRadioButton".into(),
        });
        assert!(duplicate_semantic.scoped_revisit_pages(&target).is_none());
    }

    fn row_spec(scroll_labels: &[&str], headers: &[&str]) -> RowTargetSpec {
        RowTargetSpec {
            table_scope: RowTableScope {
                scroll_path: scroll_labels
                    .iter()
                    .map(|label| RowContainerScope {
                        role: RowContainerRole::AxScrollArea,
                        label: (*label).into(),
                    })
                    .collect(),
                column_headers: headers.iter().map(|header| (*header).into()).collect(),
            },
            conditions: vec![RowColumnConditionSpec {
                column_name: headers[0].into(),
                value: ExpectedValue::Text {
                    value: "public row value".into(),
                },
            }],
        }
    }

    #[test]
    fn row_operations_are_static_exact_scoped_and_localized() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let cases = [
            ("saved-view-row", vec!["Saved views"], vec!["Entry"]),
            ("saved-view-row", vec!["已保存视图"], vec!["条目"]),
            (
                "project-row",
                vec!["Preferences content", "Projects"],
                vec!["Name", "Owner", "Status"],
            ),
            (
                "project-row",
                vec!["偏好设置内容", "项目"],
                vec!["名称", "负责人", "状态"],
            ),
            (
                "archive-row",
                vec!["Preferences content", "Archive"],
                vec!["Archive item"],
            ),
            (
                "archive-row",
                vec!["偏好设置内容", "归档"],
                vec!["归档项目"],
            ),
            (
                "record-row",
                vec!["Records"],
                vec!["Record", "Status", "Updated"],
            ),
            ("record-row", vec!["记录"], vec!["记录", "状态", "更新时间"]),
        ];
        for (semantic, scroll_path, headers) in cases {
            assert_eq!(
                pack.row_selection_effect_class(
                    &SemanticTargetRef::new(semantic).unwrap(),
                    &row_spec(&scroll_path, &headers),
                    "ordinary row",
                ),
                Some(EffectClass::Navigation),
                "{semantic} must match its exact fixture scope"
            );
        }
        assert!(
            AppPack::observe_only("other.app")
                .row_selection_effect_class(
                    &SemanticTargetRef::new("archive-row").unwrap(),
                    &row_spec(&["Preferences content", "Archive"], &["Archive item"]),
                    "Archive item",
                )
                .is_none(),
            "unknown applications cannot gain a row operation from role or label"
        );
        assert!(
            AppPack::observe_only("com.lingxi.computer-user.mvp-fixture")
                .row_selection_effect_class(
                    &SemanticTargetRef::new("archive-row").unwrap(),
                    &row_spec(&["Preferences content", "Archive"], &["Archive item"]),
                    "Archive item",
                )
                .is_none(),
            "an observation-only pack never inherits the native profile"
        );
    }

    #[test]
    fn row_scope_requires_exact_scroll_path_and_header_set() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let semantic = SemanticTargetRef::new("project-row").unwrap();
        let project_path = ["Preferences content", "Projects"];
        let mut unordered = row_spec(&project_path, &["Status", "Name", "Owner"]);
        unordered.conditions[0].column_name = "Name".into();
        assert_eq!(
            pack.row_selection_effect_class(&semantic, &unordered, "row"),
            Some(EffectClass::Navigation),
            "column headers are an exact set, not an order"
        );

        assert!(
            pack.row_selection_effect_class(
                &semantic,
                &row_spec(
                    &["Preferences content", "Other list"],
                    &["Name", "Owner", "Status"]
                ),
                "row"
            )
            .is_none()
        );
        assert!(
            pack.row_selection_effect_class(
                &SemanticTargetRef::new("archive-row").unwrap(),
                &row_spec(&["Archive"], &["Archive item"]),
                "row"
            )
            .is_none(),
            "the Archive table requires its actual Preferences content ancestor"
        );
        assert!(
            pack.row_selection_effect_class(
                &semantic,
                &row_spec(&["Projects"], &["Name", "Owner", "Status"]),
                "row"
            )
            .is_none(),
            "the inner Projects scroll must not erase the real outer Preferences content scroll"
        );
        assert!(
            pack.row_selection_effect_class(
                &semantic,
                &row_spec(&project_path, &["Name", "Owner"]),
                "row"
            )
            .is_none()
        );
        assert!(
            pack.row_selection_effect_class(
                &semantic,
                &row_spec(&project_path, &["Name", "Owner", "Status", "Updated"]),
                "row"
            )
            .is_none()
        );
        assert!(
            pack.row_selection_effect_class(
                &semantic,
                &row_spec(&project_path, &["name", "Owner", "Status"]),
                "row"
            )
            .is_none(),
            "header matching is exact and case-sensitive"
        );

        let mut mismatched_condition = row_spec(&project_path, &["Name", "Owner", "Status"]);
        mismatched_condition.conditions[0].column_name = "name".into();
        assert!(
            pack.row_selection_effect_class(&semantic, &mismatched_condition, "row")
                .is_none(),
            "condition headers must preserve the exact registered spelling"
        );
    }

    #[test]
    fn row_labels_only_raise_the_registered_selection_effect() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let semantic = SemanticTargetRef::new("saved-view-row").unwrap();
        let spec = row_spec(&["Saved views"], &["Entry"]);
        assert_eq!(
            pack.row_selection_effect_class(&semantic, &spec, "Quarterly review"),
            Some(EffectClass::Navigation)
        );
        assert_eq!(
            pack.row_selection_effect_class(&semantic, &spec, "Delete this saved view"),
            Some(EffectClass::Destructive)
        );
        assert_eq!(
            pack.row_selection_effect_class(&semantic, &spec, "Send this view"),
            Some(EffectClass::External)
        );
    }

    #[test]
    fn planning_catalog_exposes_only_static_row_capabilities() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let catalog = planning_catalog(&[pack]);
        let value: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        let rows = value["apps"][0]["row_operations"].as_array().unwrap();
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().any(|row| {
            row["semantic_target"] == "archive-row"
                && row["table_scope"]["scroll_path"][0]["label"] == "Preferences content"
                && row["table_scope"]["scroll_path"][1]["label"] == "Archive"
                && row["table_scope"]["column_headers"][0] == "Archive item"
                && row["selection"] == "AXSelected=true"
        }));
        assert!(rows.iter().any(|row| {
            row["semantic_target"] == "archive-row"
                && row["table_scope"]["scroll_path"][0]["label"] == "偏好设置内容"
                && row["table_scope"]["scroll_path"][1]["label"] == "归档"
                && row["table_scope"]["column_headers"][0] == "归档项目"
        }));
        assert!(rows.iter().any(|row| {
            row["semantic_target"] == "project-row"
                && row["table_scope"]["scroll_path"][0]["label"] == "Preferences content"
                && row["table_scope"]["scroll_path"][1]["label"] == "Projects"
                && row["table_scope"]["column_headers"]
                    == serde_json::json!(["Name", "Owner", "Status"])
        }));
        for field in ["pid", "window_id", "instance_ref", "row_index", "selected"] {
            assert!(!catalog.contains(&format!("\"{field}\"")));
        }
    }

    #[test]
    fn native_toggle_registers_only_its_known_checkbox_semantics() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.guard-fixture").unwrap();
        assert_eq!(
            pack.effect_class("AXCheckBox", "MVP toggle"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(
            pack.effect_class("AXCheckBox", "Unregistered toggle"),
            EffectClass::Unknown
        );
        assert_eq!(
            pack.control(&SemanticTargetRef::new("toggle").unwrap())
                .unwrap()
                .role,
            "AXCheckBox"
        );
        assert_eq!(
            AppPack::observe_only("other.app").effect_class("AXCheckBox", "MVP toggle"),
            EffectClass::Unknown
        );
    }

    fn mvp_snapshot() -> StructureSnapshot {
        let mut snapshot = sim_world::app::SimApp::new().snapshot(&crate::testkit::meta(1, 10));
        let active = snapshot
            .global_focus
            .known_window_for_app(&snapshot.app)
            .unwrap()
            .to_owned();
        let template = snapshot.objects.first().unwrap().clone();
        snapshot.app.bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        crate::testkit::focus_snapshot_window(&mut snapshot, active.clone());
        let window = snapshot
            .windows
            .iter_mut()
            .find(|window| window.window_id == active)
            .unwrap();
        window.owner_bundle_id = "com.lingxi.computer-user.mvp-fixture".into();
        window.title = Some("Interaction Fixture".into());
        let mut root = template.clone();
        root.title = window.title.clone();
        let root_ref = root.instance_ref.clone();
        snapshot.objects.clear();

        let radio = |id: &str, label: &str, selected: bool| {
            let mut object = template.clone();
            object.instance_ref = interaction_contracts::desktop::ids::ObjectRef::new(id).unwrap();
            object.parent = Some(root_ref.clone());
            object.window_id = active.clone();
            object.role = "AXRadioButton".into();
            object.subrole = None;
            object.title = Some(label.into());
            object.description = None;
            object.value = Some(UiValue::Bool(selected));
            object.enabled = true;
            object.focused = false;
            object.actions = vec![AxActionName::Press];
            object.in_view = true;
            object.group = None;
            object.secure = false;
            object
        };
        snapshot.objects = vec![
            root,
            radio("settings-page", "Settings", true),
            radio("overview-page", "Overview", false),
        ];
        sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
        snapshot
    }

    #[test]
    fn mvp_fixture_effects_are_role_and_label_scoped() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        assert_eq!(pack.nav_role, "AXRadioButton");
        assert_eq!(
            pack.effect_class("AXRadioButton", "Settings"),
            EffectClass::Navigation
        );
        assert_eq!(
            pack.effect_class("AXRadioButton", "Summary grouping"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(
            pack.effect_class("AXRadioGroup", "Summary grouping"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(
            pack.effect_class("AXRadioGroup", "摘要分组"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(
            pack.effect_class("AXTextArea", "Notes"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(pack.effect_class("AXButton", "Save"), EffectClass::Commit);
        assert_eq!(
            pack.effect_class("AXMenuItem", "Weekly"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(
            pack.effect_class("AXMenuItem", "Delete this saved view"),
            EffectClass::Destructive
        );
        assert_eq!(
            pack.effect_class("AXMenuItem", "Publish this view"),
            EffectClass::External
        );
        assert_eq!(
            AppPack::observe_only("other.app").effect_class("AXRadioButton", "Settings"),
            EffectClass::Unknown
        );
    }

    #[test]
    fn mvp_page_requires_two_live_registered_radios_and_one_selected_value() {
        let pack = AppPack::registered_native("com.lingxi.computer-user.mvp-fixture").unwrap();
        let mut snapshot = mvp_snapshot();
        assert_eq!(pack.page_of(&snapshot).as_deref(), Some("Settings"));

        // A real radio in the same window is still not a page selector.
        let mut summary = snapshot.objects[1].clone();
        summary.instance_ref =
            interaction_contracts::desktop::ids::ObjectRef::new("summary").unwrap();
        summary.identity_continuity =
            interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
        summary.title = Some("By week".into());
        summary.value = Some(UiValue::Bool(true));
        snapshot.objects.push(summary);
        sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
        assert_eq!(pack.page_of(&snapshot).as_deref(), Some("Settings"));
        assert!(!pack.is_navigation_target(&snapshot.objects[3]));

        // The fixed window title cannot supply page state; the selected live
        // value is the evidence that changes the reported page.
        snapshot.objects[1].value = Some(UiValue::Bool(false));
        snapshot.objects[2].value = Some(UiValue::Bool(true));
        assert_eq!(pack.page_of(&snapshot).as_deref(), Some("Overview"));
        snapshot.objects[1].value = Some(UiValue::Bool(true));
        snapshot.objects[2].value = Some(UiValue::Bool(false));

        snapshot.objects[2].value = None;
        assert_eq!(pack.page_of(&snapshot), None);
        snapshot.objects[2].value = Some(UiValue::Bool(true));
        assert_eq!(pack.page_of(&snapshot), None);
        snapshot.objects[2].value = Some(UiValue::Bool(false));
        snapshot.complete = false;
        assert_eq!(pack.page_of(&snapshot), None);
        snapshot.complete = true;
        snapshot.objects[1].title = Some("设置".into());
        snapshot.objects[2].title = Some("概览".into());
        assert_eq!(pack.page_of(&snapshot).as_deref(), Some("Settings"));
        snapshot.objects[1].actions.clear();
        assert_eq!(pack.page_of(&snapshot), None);
    }

    #[test]
    fn registered_controls_get_their_registered_class() {
        let pack = AppPack::settings_fixture();
        assert_eq!(pack.effect_class("button", "Apply"), EffectClass::Commit);
        assert_eq!(
            pack.effect_class("button", "Cancel"),
            EffectClass::Navigation
        );
        assert_eq!(
            pack.effect_class("switch", "Daily picks"),
            EffectClass::ReversibleEdit
        );
        assert_eq!(pack.effect_class("tab", "General"), EffectClass::Navigation);
    }

    #[test]
    fn registered_controls_are_found_by_their_semantic_name() {
        let pack = AppPack::settings_fixture();
        let apply = pack
            .control(&SemanticTargetRef::new("apply-button").unwrap())
            .unwrap();
        assert_eq!(
            (apply.label.as_str(), apply.role.as_str()),
            ("Apply", "button")
        );
        assert_eq!(
            pack.effect_class(&apply.role, &apply.label),
            EffectClass::Commit
        );
        assert!(
            pack.control(&SemanticTargetRef::new("nonsense").unwrap())
                .is_none()
        );
    }

    #[test]
    fn an_unregistered_button_is_unknown_and_never_guessed_harmless() {
        let pack = AppPack::settings_fixture();
        assert_eq!(
            pack.effect_class("button", "Frobnicate"),
            EffectClass::Unknown
        );
        assert_eq!(
            pack.effect_class("mystery", "Whatever"),
            EffectClass::Unknown
        );
    }

    #[test]
    fn label_words_can_only_raise_a_class() {
        let pack = AppPack::settings_fixture();
        // A switch is a reversible edit, unless its label says otherwise.
        assert_eq!(
            pack.effect_class("switch", "Delete account data"),
            EffectClass::Destructive
        );
        assert_eq!(
            pack.effect_class("switch", "Send weekly report"),
            EffectClass::External
        );
        // A registered destructive control stays destructive whatever its label.
        assert_eq!(
            pack.effect_class("button", "Discard"),
            EffectClass::Destructive
        );
        // Unknown cannot be talked down by a harmless word.
        assert_eq!(
            pack.effect_class("button", "Frobnicate"),
            EffectClass::Unknown
        );
    }

    #[test]
    fn a_plan_step_is_classified_from_its_label_alone() {
        let pack = AppPack::settings_fixture();
        assert_eq!(
            pack.effect_class_for_label("apply button"),
            EffectClass::Commit
        );
        assert_eq!(
            pack.effect_class_for_label("reset button"),
            EffectClass::Destructive
        );
        assert_eq!(
            pack.effect_class_for_label("something else"),
            EffectClass::Unknown
        );
    }
    #[test]
    fn page_uses_the_active_window_instead_of_an_auxiliary_window() {
        let mut snapshot = sim_world::app::SimApp::new().snapshot(&crate::testkit::meta(1, 10));
        let pack = AppPack::settings_fixture();
        let expected = pack.page_of(&snapshot);
        let mut auxiliary = snapshot.windows[0].clone();
        auxiliary.window_id = "sharing-indicator".into();
        auxiliary.title = Some("Window".into());
        snapshot.windows.insert(0, auxiliary);
        assert_eq!(pack.page_of(&snapshot), expected);
        crate::testkit::focus_elsewhere(&mut snapshot);
        assert_eq!(pack.page_of(&snapshot), None);
    }
    #[test]
    fn keyboard_profile_is_exact_static_app_semantic_role_and_label() {
        let target = SemanticTargetRef::new("keyboard-toggle").unwrap();
        let mut pack =
            AppPack::registered_native("com.lingxi.computer-user.guard-fixture").unwrap();
        assert!(pack.keyboard_toggle(&target).is_some());
        assert_eq!(
            pack.effect_class_for_label("Key fixture toggle"),
            EffectClass::ReversibleEdit
        );
        assert_ne!(
            pack.effect_class_for_label("Key fixture toggle extra"),
            EffectClass::ReversibleEdit
        );
        assert!(
            pack.keyboard_toggle(&SemanticTargetRef::new("toggle").unwrap())
                .is_none()
        );
        pack.bundle_id = "com.apple.TextEdit".into();
        assert!(pack.keyboard_toggle(&target).is_none());
        pack.bundle_id = "com.lingxi.computer-user.guard-fixture".into();
        let control = pack
            .controls
            .iter_mut()
            .find(|c| c.semantic == target.as_str())
            .unwrap();
        control.role = "AXButton".into();
        assert!(pack.keyboard_toggle(&target).is_none());
    }
}
