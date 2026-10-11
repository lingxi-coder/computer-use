//! Bounded scroll search. Each input consumes a new container binding and is
//! followed by a complete read; offset progress is never destination proof.

use interaction_contracts::desktop::binding::{
    BindRequest, BindScope, BindingOutcome, TargetDescription,
};
use interaction_contracts::desktop::ids::RequestId;
use interaction_contracts::desktop::planning::{ScrollDirection, ScrollToSpec};
use interaction_contracts::desktop::source::{Rect, ScrollAxisState};

use super::*;

#[derive(Debug, Clone)]
pub(super) struct ScrollRun {
    spec: ScrollToSpec,
    destination: TargetSpec,
    last_sequence: Option<u64>,
    previous: Option<ScrollAxisState>,
    scroll_actions: u32,
    prepared_frame: Option<Rect>,
    context: Option<(u64, u64, Option<String>, String, u32)>,
}

impl ScrollRun {
    pub(super) fn new(spec: ScrollToSpec, destination: TargetSpec) -> Self {
        Self {
            spec,
            destination,
            last_sequence: None,
            previous: None,
            scroll_actions: 0,
            prepared_frame: None,
            context: None,
        }
    }

    fn decreasing(&self) -> bool {
        matches!(
            self.spec.direction,
            ScrollDirection::Up | ScrollDirection::Left
        )
    }

    fn edge(&self, axis: &ScrollAxisState) -> bool {
        if self.decreasing() {
            axis.min.is_some_and(|min| axis.value <= min)
        } else {
            axis.max.is_some_and(|max| axis.value >= max)
        }
    }
}

impl SkillRun {
    pub(crate) fn scroll_targets(&self) -> Option<(&SemanticTargetRef, &SemanticTargetRef)> {
        self.scroll.as_ref().map(|scroll| {
            (
                &scroll.spec.container.semantic_target,
                &scroll.destination.semantic_target,
            )
        })
    }

    pub(super) fn advance_scroll(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        if let Phase::Ended(result) = &self.phase {
            return SkillStep::Done(result.clone());
        }
        let step = self.scroll_step(view);
        if let SkillStep::Done(result) = &step {
            self.phase = Phase::Ended(result.clone());
        }
        step
    }

    fn scroll_step(&mut self, view: &mut SkillView<'_>) -> SkillStep {
        let Some(scroll) = self.scroll.as_mut() else {
            return fail(
                FailureCode::ContextUnresolved,
                "scroll search has no parameters",
            );
        };
        if !(1..=120).contains(&scroll.spec.pixels)
            || !(1..=16).contains(&scroll.spec.max_actions)
            || scroll.spec.container.semantic_target == scroll.destination.semantic_target
            || scroll.spec.container.semantic_target != self.target
        {
            return fail(
                FailureCode::ContextUnresolved,
                "invalid scroll search parameters",
            );
        }
        let preparing = matches!(self.phase, Phase::Judging(Follow::PrepareScroll, _));
        let known = match self.phase {
            Phase::Start => None,
            Phase::Judging(Follow::Verify | Follow::PrepareScroll, known) => Some(known),
            _ => {
                return fail(
                    FailureCode::ContextUnresolved,
                    "the previous scroll input has no reported outcome",
                );
            }
        };
        let floor = self.input_floor;
        let env = match evidence(view.state, floor) {
            Ok(env) => env,
            Err(step) => return step,
        };
        if !view.state.usable(view.now_ms, view.max_age_ms)
            || env.snapshot.observed_ms > view.now_ms
            || view.now_ms.saturating_sub(env.snapshot.observed_ms) > view.max_age_ms
            || scroll
                .last_sequence
                .is_some_and(|last| env.snapshot.source_sequence <= last)
        {
            return SkillStep::AwaitEvidence {
                after_ms: floor
                    .unwrap_or(0)
                    .max(env.snapshot.observed_ms.saturating_add(1)),
            };
        }
        let context = (
            env.snapshot.session_epoch,
            env.layer_revision,
            env.page.clone(),
            env.snapshot.app.bundle_id.clone(),
            env.snapshot.app.pid,
        );
        if scroll
            .context
            .as_ref()
            .is_some_and(|initial| initial != &context)
        {
            return fail(
                FailureCode::ContextUnresolved,
                "the scroll search context changed",
            );
        }
        scroll.context.get_or_insert(context);
        scroll.last_sequence = Some(env.snapshot.source_sequence);

        // Recreate from this read even when source instance IDs are recycled or
        // replaced. Never move the old binding's proof onto a new instance.
        let spec = &scroll.spec.container;
        let request = BindRequest {
            request_id: RequestId::new(format!("scroll-container-{}", env.revision)).unwrap(),
            semantic_target: spec.semantic_target.clone(),
            description: TargetDescription {
                label: spec.label.clone(),
                group: spec.group.clone(),
                role_hint: spec.role_hint.clone(),
            },
            scope: BindScope {
                app: env.snapshot.app.bundle_id.clone(),
                account: None,
            },
            choice: None,
            max_age_ms: view.max_age_ms,
        };
        let bound = view.bindings.bind(env, &request, view.now_ms);
        if !matches!(bound.outcome, BindingOutcome::Bound { .. }) {
            return fail(
                FailureCode::ContextUnresolved,
                "the current scroll container cannot be bound",
            );
        }
        let binding = view.bindings.current(&self.target).unwrap().clone();
        let Some(object) = object_for(env, &binding.fingerprint_digest) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the scroll container is not in the active layer",
            );
        };
        self.binding = binding.binding_ref.clone();

        // BindingReducer scopes the candidate set to current descendants, so
        // a matching label elsewhere cannot prove that this search succeeded.
        let destination = view.bindings.bind_in_container(
            env,
            &scroll.destination,
            &binding,
            view.now_ms,
            view.max_age_ms,
        );
        match destination.outcome {
            BindingOutcome::Bound { .. } => {
                return SkillStep::Done(if self.actions == 0 {
                    SkillResult::AlreadyDone
                } else {
                    SkillResult::Succeeded
                });
            }
            BindingOutcome::NotFound {
                observation_complete: true,
            } => {}
            _ => {
                return fail(
                    FailureCode::ContextUnresolved,
                    "the scroll destination cannot be resolved inside the container",
                );
            }
        }
        if known.is_some_and(|known| known.unknown) {
            return fail(
                FailureCode::EffectUnknown,
                "the scroll outcome is unknown; no further input is authorized",
            );
        }

        let axis = object
            .scroll
            .as_ref()
            .and_then(|state| match scroll.spec.direction {
                ScrollDirection::Up | ScrollDirection::Down => state.vertical.as_ref(),
                ScrollDirection::Left | ScrollDirection::Right => state.horizontal.as_ref(),
            });
        let Some(axis) = axis.filter(|axis| axis.valid()) else {
            return fail(
                FailureCode::ContextUnresolved,
                "the scroll container has no valid metrics for this direction",
            );
        };
        if !preparing && let Some(previous) = &scroll.previous {
            let progress = if scroll.decreasing() {
                axis.value < previous.value
            } else {
                axis.value > previous.value
            };
            if !progress && !scroll.edge(axis) {
                return fail(
                    FailureCode::EffectUnknown,
                    "the scroll produced no directed progress and no explicit boundary",
                );
            }
        }
        if scroll.edge(axis) {
            return SkillStep::Done(SkillResult::NotFound {
                observation_complete: true,
                // AX values may be normalized and applications may jump over
                // virtual rows. Endpoint traversal cannot prove full coverage.
                search_complete: false,
                reason: "the destination is absent from the current container at the directed scroll boundary".into(),
            });
        }
        if scroll.scroll_actions >= scroll.spec.max_actions {
            return fail(
                FailureCode::BudgetExhausted,
                "the scroll search exhausted its action quota",
            );
        }
        if self.scroll_pointer_preparation {
            let (x, y) = object.frame.center();
            let center = (x.round() as i32, y.round() as i32);
            if preparing {
                // A matching rounded point cannot attest unchanged geometry:
                // same-center resizing and subpixel motion change the proof.
                if scroll.prepared_frame != Some(object.frame) {
                    return fail(
                        FailureCode::ContextUnresolved,
                        "the scroll container frame changed during pointer preparation",
                    );
                }
            } else {
                scroll.prepared_frame = Some(object.frame);
                return self.act(
                    binding,
                    Primitive::PointerPosition {
                        x: center.0,
                        y: center.1,
                    },
                    ExecutionPath::Pointer,
                    Follow::PrepareScroll,
                    "move the pointer to the current scroll container before scrolling",
                );
            }
        }
        scroll.previous = Some(*axis);
        scroll.scroll_actions += 1;
        let pixels = scroll.spec.pixels as i32;
        // Primitive::Scroll carries raw CG wheel axes, not content offsets:
        // negative Y scrolls the viewport down, and negative X scrolls right.
        // Keep semantic direction and the source offset progress check aligned
        // while translating exactly once at this input boundary.
        let (dx, dy) = match scroll.spec.direction {
            ScrollDirection::Up => (0, pixels),
            ScrollDirection::Down => (0, -pixels),
            ScrollDirection::Left => (pixels, 0),
            ScrollDirection::Right => (-pixels, 0),
        };
        self.act(
            binding,
            Primitive::Scroll { dx, dy },
            ExecutionPath::Pointer,
            Follow::Verify,
            "scroll the current container once and inspect fresh evidence",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{EnvRig, meta};
    use interaction_contracts::desktop::ids::ObjectRef;
    use interaction_contracts::desktop::source::{ScrollState, StructureSnapshot, UiObject};

    fn spec(id: &str, label: &str, role: &str) -> TargetSpec {
        TargetSpec {
            row: None,
            semantic_target: SemanticTargetRef::new(id).unwrap(),
            label: label.into(),
            group: None,
            role_hint: Some(role.into()),
        }
    }

    struct Rig {
        env: EnvRig,
        template: StructureSnapshot,
        run: SkillRun,
    }

    fn scroll_area(snapshot: &StructureSnapshot) -> &UiObject {
        snapshot
            .objects
            .iter()
            .find(|object| object.role == "AXScrollArea")
            .expect("fixture has its actual scroll area")
    }

    fn scroll_area_mut(snapshot: &mut StructureSnapshot) -> &mut UiObject {
        snapshot
            .objects
            .iter_mut()
            .find(|object| object.role == "AXScrollArea")
            .expect("fixture has its actual scroll area")
    }

    impl Rig {
        fn new(value: f64, max_actions: u32) -> Self {
            let mut env = EnvRig::new();
            let mut template = env.app.snapshot(&meta(1, 100));
            let window_root = template
                .objects
                .iter()
                .find(|object| object.role == "AXWindow" && object.parent.is_none())
                .expect("template has its actual AXWindow root")
                .clone();
            let mut records_group = window_root.clone();
            records_group.instance_ref = ObjectRef::new("records-group").unwrap();
            records_group.parent = Some(window_root.instance_ref.clone());
            records_group.role = "AXGroup".into();
            records_group.subrole = None;
            records_group.title = Some("Records".into());
            records_group.description = None;
            records_group.value = None;
            records_group.group = None;
            records_group.actions.clear();
            let mut container = window_root.clone();
            container.instance_ref = ObjectRef::new("container-1").unwrap();
            container.parent = Some(records_group.instance_ref.clone());
            container.role = "AXScrollArea".into();
            container.title = Some("Records".into());
            container.description = None;
            container.group = Some("Records".into());
            container.enabled = true;
            container.in_view = true;
            container.secure = false;
            container.frame.height = 400.0;
            container.scroll = Some(ScrollState {
                horizontal: None,
                vertical: Some(ScrollAxisState {
                    value,
                    min: Some(0.0),
                    max: Some(600.0),
                }),
            });
            let mut recycled = container.clone();
            recycled.instance_ref = ObjectRef::new("recycled-row").unwrap();
            recycled.parent = Some(container.instance_ref.clone());
            recycled.role = "AXRow".into();
            recycled.title = Some("Other record".into());
            recycled.scroll = None;
            recycled.identity_continuity =
                interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
            template.objects = vec![window_root, records_group, container, recycled];
            sim_world::app::refresh_simulated_observer_coverage(&mut template);
            env.state.admit(template.clone(), 100, &env.pack).unwrap();
            env.now_ms = 100;
            env.sequence = 1;
            let container = spec("scroll/records", "Records", "AXScrollArea");
            let request = BindRequest {
                request_id: RequestId::new("initial-container").unwrap(),
                semantic_target: container.semantic_target.clone(),
                description: TargetDescription {
                    label: container.label.clone(),
                    group: None,
                    role_hint: container.role_hint.clone(),
                },
                scope: BindScope {
                    app: template.app.bundle_id.clone(),
                    account: None,
                },
                choice: None,
                max_age_ms: 2000,
            };
            env.bindings
                .bind(env.state.latest().unwrap(), &request, 100);
            let binding = env
                .bindings
                .current(&container.semantic_target)
                .unwrap()
                .binding_ref
                .clone();
            let run = SkillRun::new(
                Skill::ScrollTo,
                container.semantic_target.clone(),
                None,
                binding,
            )
            .with_scroll(
                ScrollToSpec {
                    container,
                    direction: ScrollDirection::Down,
                    pixels: 120,
                    max_actions,
                },
                spec("scroll/destination", "Wanted", "AXRow"),
            );
            Self { env, template, run }
        }

        fn read(&mut self, value: Option<f64>, destination: Option<bool>, replace_container: bool) {
            self.read_with_range(
                value,
                destination,
                replace_container,
                Some(0.0),
                Some(600.0),
            );
        }

        fn read_with_range(
            &mut self,
            value: Option<f64>,
            destination: Option<bool>,
            replace_container: bool,
            min: Option<f64>,
            max: Option<f64>,
        ) {
            self.env.sequence += 1;
            self.env.now_ms += 100;
            let mut snapshot = self.template.clone();
            snapshot.objects.truncate(3);
            snapshot.snapshot_id = RequestId::new(format!("snap-{}", self.env.sequence)).unwrap();
            snapshot.source_sequence = self.env.sequence;
            snapshot.observed_ms = self.env.now_ms;
            let container = scroll_area_mut(&mut snapshot);
            if replace_container {
                container.instance_ref =
                    ObjectRef::new(format!("container-{}", self.env.sequence)).unwrap();
            }
            container.scroll = value.map(|value| ScrollState {
                horizontal: None,
                vertical: Some(ScrollAxisState { value, min, max }),
            });
            if let Some(inside) = destination {
                let mut row = container.clone();
                row.instance_ref = ObjectRef::new("recycled-row").unwrap();
                row.parent = inside.then(|| container.instance_ref.clone());
                row.role = "AXRow".into();
                row.title = Some("Wanted".into());
                row.scroll = None;
                row.identity_continuity =
                    interaction_contracts::desktop::source::ObjectIdentityContinuity::ReadScoped;
                snapshot.objects.push(row);
            }
            sim_world::app::refresh_simulated_observer_coverage(&mut snapshot);
            self.env
                .state
                .admit(snapshot, self.env.now_ms, &self.env.pack)
                .unwrap();
            self.env
                .bindings
                .revalidate(self.env.state.latest().unwrap());
        }

        fn advance(&mut self) -> SkillStep {
            self.run.advance(&mut SkillView {
                state: &self.env.state,
                bindings: &mut self.env.bindings,
                pack: &self.env.pack,
                now_ms: self.env.now_ms,
                max_age_ms: 2000,
            })
        }

        fn complete(&mut self) {
            self.run.acted(ActionOutcome::Completed {
                end_ms: self.env.now_ms + 10,
            });
        }
    }

    #[test]
    fn offset_progress_and_an_off_container_duplicate_do_not_succeed() {
        let mut rig = Rig::new(0.0, 16);
        let SkillStep::Act(first) = rig.advance() else {
            panic!()
        };
        assert_eq!(first.primitive, Primitive::Scroll { dx: 0, dy: -120 });
        rig.complete();
        rig.read(Some(120.0), Some(false), false);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        assert_eq!(rig.run.actions(), 2);
    }

    #[test]
    fn a_virtual_row_and_container_rebound_from_fresh_evidence_can_succeed() {
        let mut rig = Rig::new(0.0, 16);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        rig.complete();
        rig.read(Some(120.0), Some(true), true);
        assert_eq!(rig.advance(), SkillStep::Done(SkillResult::Succeeded));
        assert_eq!(rig.run.actions(), 1);
        assert_eq!(
            rig.env
                .bindings
                .current(&SemanticTargetRef::new("scroll/destination").unwrap())
                .unwrap()
                .instance
                .as_str(),
            "recycled-row"
        );
    }

    #[test]
    fn each_scroll_uses_a_new_container_proof_and_source_sequence() {
        let mut rig = Rig::new(0.0, 16);
        let SkillStep::Act(first) = rig.advance() else {
            panic!()
        };
        rig.complete();
        assert!(matches!(rig.advance(), SkillStep::AwaitEvidence { .. }));
        rig.read(Some(120.0), None, true);
        let SkillStep::Act(second) = rig.advance() else {
            panic!()
        };
        assert_ne!(first.binding.binding_ref, second.binding.binding_ref);
        assert_ne!(first.binding.instance, second.binding.instance);
        assert!(second.binding.observed_ms >= rig.run.input_floor().unwrap());
    }

    #[test]
    fn missing_metrics_and_no_movement_are_unresolved() {
        for value in [None, Some(0.0)] {
            let mut rig = Rig::new(0.0, 16);
            assert!(matches!(rig.advance(), SkillStep::Act(_)));
            rig.complete();
            rig.read(value, None, false);
            assert!(matches!(
                rig.advance(),
                SkillStep::Done(SkillResult::Failed {
                    code: FailureCode::ContextUnresolved | FailureCode::EffectUnknown,
                    ..
                })
            ));
            assert_eq!(rig.run.actions(), 1);
        }
    }

    #[test]
    fn stale_evidence_cannot_start_a_scroll() {
        let mut rig = Rig::new(0.0, 16);
        rig.env.now_ms = 3000;
        assert!(matches!(rig.advance(), SkillStep::AwaitEvidence { .. }));
        assert_eq!(rig.run.actions(), 0);
    }

    #[test]
    fn incomplete_reads_and_pending_refresh_do_not_authorize_input() {
        let mut rig = Rig::new(0.0, 16);
        assert!(rig.env.state.begin_refresh().is_some());
        assert!(matches!(rig.advance(), SkillStep::AwaitEvidence { .. }));
        assert_eq!(rig.run.actions(), 0);

        let mut rig = Rig::new(0.0, 16);
        let mut snapshot = rig.template.clone();
        snapshot.source_sequence = 2;
        snapshot.snapshot_id = RequestId::new("partial-2").unwrap();
        snapshot.observed_ms = 200;
        snapshot.complete = false;
        snapshot.incomplete_reason = Some("traversal budget".into());
        rig.env.state.admit(snapshot, 200, &rig.env.pack).unwrap();
        rig.env.now_ms = 200;
        assert!(matches!(rig.advance(), SkillStep::AwaitEvidence { .. }));
        assert_eq!(rig.run.actions(), 0);
    }

    #[test]
    fn invalid_axis_metrics_do_not_authorize_input() {
        let mut rig = Rig::new(0.0, 16);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        rig.complete();
        let mut snapshot = rig.template.clone();
        snapshot.source_sequence = 2;
        snapshot.snapshot_id = RequestId::new("invalid-metrics-2").unwrap();
        snapshot.observed_ms = 200;
        scroll_area_mut(&mut snapshot)
            .scroll
            .as_mut()
            .unwrap()
            .vertical
            .as_mut()
            .unwrap()
            .value = f64::NAN;
        assert!(rig.env.state.admit(snapshot, 200, &rig.env.pack).is_err());
        rig.env.now_ms = 200;
        assert!(matches!(rig.advance(), SkillStep::AwaitEvidence { .. }));
        assert_eq!(rig.run.actions(), 1);
    }

    #[test]
    fn direction_uses_the_declared_axis_and_a_single_bounded_scroll() {
        for (direction, expected) in [
            (ScrollDirection::Up, Primitive::Scroll { dx: 0, dy: 120 }),
            (ScrollDirection::Down, Primitive::Scroll { dx: 0, dy: -120 }),
            (ScrollDirection::Left, Primitive::Scroll { dx: 120, dy: 0 }),
            (
                ScrollDirection::Right,
                Primitive::Scroll { dx: -120, dy: 0 },
            ),
        ] {
            let mut rig = Rig::new(300.0, 16);
            rig.run.scroll.as_mut().unwrap().spec.direction = direction;
            let mut snapshot = rig.template.clone();
            snapshot.source_sequence = 2;
            snapshot.snapshot_id = RequestId::new("direction-2").unwrap();
            snapshot.observed_ms = 200;
            if matches!(direction, ScrollDirection::Left | ScrollDirection::Right) {
                let metrics = scroll_area_mut(&mut snapshot).scroll.as_mut().unwrap();
                metrics.horizontal = metrics.vertical.take();
            }
            rig.env.state.admit(snapshot, 200, &rig.env.pack).unwrap();
            rig.env.now_ms = 200;
            let SkillStep::Act(action) = rig.advance() else {
                panic!()
            };
            assert_eq!(action.primitive, expected);
            assert_eq!(rig.run.actions(), 1);
        }
    }

    #[test]
    fn an_unknown_input_can_be_verified_by_a_fresh_destination_without_more_input() {
        let mut rig = Rig::new(0.0, 16);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        rig.run.acted(ActionOutcome::Unknown { at_ms: 110 });
        rig.read(Some(120.0), Some(true), false);
        assert_eq!(rig.advance(), SkillStep::Done(SkillResult::Succeeded));
        assert_eq!(rig.run.actions(), 1);
    }

    #[test]
    fn unknown_scroll_never_requests_another_input_even_with_offset_progress() {
        let mut rig = Rig::new(0.0, 16);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        rig.run.acted(ActionOutcome::Unknown { at_ms: 110 });
        rig.read(Some(120.0), None, false);
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(rig.run.actions(), 1);
    }

    #[test]
    fn a_middle_start_cannot_claim_complete_search_at_the_boundary() {
        let mut rig = Rig::new(480.0, 16);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        rig.complete();
        rig.read(Some(600.0), None, false);
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::NotFound {
                observation_complete: true,
                search_complete: false,
                ..
            })
        ));
    }

    #[test]
    fn exhausting_the_local_quota_is_not_absence_or_success() {
        let mut rig = Rig::new(0.0, 1);
        assert!(matches!(rig.advance(), SkillStep::Act(_)));
        rig.complete();
        rig.read(Some(120.0), None, false);
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::BudgetExhausted,
                ..
            })
        ));
        assert_eq!(rig.run.actions(), 1);
    }

    #[test]
    fn actual_value_without_a_directed_bound_can_progress_but_never_prove_an_edge() {
        for min in [None, Some(0.0)] {
            let mut rig = Rig::new(0.0, 2);
            rig.read_with_range(Some(0.0), None, false, min, None);
            assert!(matches!(rig.advance(), SkillStep::Act(_)));
            rig.complete();
            // In particular, an actual value of 1.0 is not an inferred
            // normalized maximum when AXMaxValue is unavailable.
            rig.read_with_range(Some(1.0), None, false, min, None);
            assert!(matches!(rig.advance(), SkillStep::Act(_)));
            rig.complete();
            rig.read_with_range(Some(2.0), None, false, min, None);
            assert!(matches!(
                rig.advance(),
                SkillStep::Done(SkillResult::Failed {
                    code: FailureCode::BudgetExhausted,
                    ..
                })
            ));
            assert_eq!(rig.run.actions(), 2);
        }
    }

    #[test]
    fn native_preparation_moves_before_each_scroll_without_consuming_scroll_quota() {
        let mut rig = Rig::new(0.0, 2);
        rig.run = rig.run.with_scroll_pointer_preparation();
        let (x, y) = scroll_area(&rig.template).frame.center();
        let SkillStep::Act(first_move) = rig.advance() else {
            panic!()
        };
        assert_eq!(
            first_move.primitive,
            Primitive::PointerPosition {
                x: x.round() as i32,
                y: y.round() as i32
            }
        );
        assert!(!first_move.primitive.is_held_class());
        assert!(first_move.primitive.is_pointer_class());
        assert!(first_move.primitive.needs_hit_test());
        assert_eq!(rig.run.actions(), 1);
        rig.complete();
        assert!(matches!(rig.advance(), SkillStep::AwaitEvidence { .. }));
        rig.read(Some(0.0), None, true);
        let SkillStep::Act(first_scroll) = rig.advance() else {
            panic!()
        };
        assert_eq!(
            first_scroll.primitive,
            Primitive::Scroll { dx: 0, dy: -120 }
        );
        assert_ne!(
            first_move.binding.binding_ref,
            first_scroll.binding.binding_ref
        );
        assert_ne!(first_move.binding.instance, first_scroll.binding.instance);

        rig.complete();
        rig.read(Some(120.0), None, false);
        assert!(
            matches!(rig.advance(), SkillStep::Act(action) if matches!(action.primitive, Primitive::PointerPosition { .. }))
        );
        rig.complete();
        // PointerPosition needs a new source read, but does not change AXValue.
        rig.read(Some(120.0), None, false);
        assert!(
            matches!(rig.advance(), SkillStep::Act(action) if matches!(action.primitive, Primitive::Scroll { .. }))
        );
        assert_eq!(rig.run.actions(), 4);
        assert_eq!(rig.run.scroll.as_ref().unwrap().scroll_actions, 2);
        rig.complete();
        rig.read(Some(240.0), None, false);
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::BudgetExhausted,
                ..
            })
        ));
        assert_eq!(rig.run.actions(), 4);
    }

    #[test]
    fn unknown_native_pointer_preparation_never_authorizes_a_scroll() {
        let mut rig = Rig::new(0.0, 2);
        rig.run = rig.run.with_scroll_pointer_preparation();
        assert!(
            matches!(rig.advance(), SkillStep::Act(action) if matches!(action.primitive, Primitive::PointerPosition { .. }))
        );
        rig.run.acted(ActionOutcome::Unknown { at_ms: 110 });
        rig.read(Some(0.0), None, false);
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::EffectUnknown,
                ..
            })
        ));
        assert_eq!(rig.run.actions(), 1);
        assert_eq!(rig.run.scroll.as_ref().unwrap().scroll_actions, 0);
    }

    #[test]
    fn visible_destination_needs_no_native_pointer_preparation() {
        let mut rig = Rig::new(0.0, 2);
        rig.run = rig.run.with_scroll_pointer_preparation();
        rig.read(Some(0.0), Some(true), false);
        assert_eq!(rig.advance(), SkillStep::Done(SkillResult::AlreadyDone));
        assert_eq!(rig.run.actions(), 0);
    }

    #[test]
    fn changed_center_during_native_preparation_cannot_scroll_at_the_old_pointer() {
        let mut rig = Rig::new(0.0, 2);
        rig.run = rig.run.with_scroll_pointer_preparation();
        assert!(
            matches!(rig.advance(), SkillStep::Act(action) if matches!(action.primitive, Primitive::PointerPosition { .. }))
        );
        rig.complete();
        scroll_area_mut(&mut rig.template).frame.x += 40.0;
        rig.read(Some(0.0), None, true);
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::Failed {
                code: FailureCode::ContextUnresolved,
                ..
            })
        ));
        assert_eq!(rig.run.actions(), 1);
        assert_eq!(rig.run.scroll.as_ref().unwrap().scroll_actions, 0);
    }

    #[test]
    fn any_frame_change_during_preparation_blocks_scroll_even_at_the_same_rounded_point() {
        for resize in [true, false] {
            let mut rig = Rig::new(0.0, 2);
            rig.run = rig.run.with_scroll_pointer_preparation();
            let original = scroll_area(&rig.template).frame;
            assert!(
                matches!(rig.advance(), SkillStep::Act(action) if matches!(action.primitive, Primitive::PointerPosition { .. }))
            );
            rig.complete();
            let frame = &mut scroll_area_mut(&mut rig.template).frame;
            if resize {
                frame.width += 2.0;
                frame.height += 2.0;
                frame.x -= 1.0;
                frame.y -= 1.0;
                assert_eq!(frame.center(), original.center());
            } else {
                frame.x += 0.1;
                frame.y += 0.1;
            }
            let (old_x, old_y) = original.center();
            let (new_x, new_y) = frame.center();
            assert_eq!(
                (old_x.round() as i32, old_y.round() as i32),
                (new_x.round() as i32, new_y.round() as i32)
            );
            rig.read(Some(0.0), None, true);
            assert!(matches!(
                rig.advance(),
                SkillStep::Done(SkillResult::Failed {
                    code: FailureCode::ContextUnresolved,
                    ..
                })
            ));
            assert_eq!(rig.run.actions(), 1);
            assert_eq!(rig.run.scroll.as_ref().unwrap().scroll_actions, 0);
        }
    }

    #[test]
    fn opposite_edge_to_boundary_with_complete_reads_does_not_prove_virtual_coverage() {
        let mut rig = Rig::new(0.0, 16);
        for value in [120.0, 240.0, 360.0, 480.0, 600.0] {
            assert!(matches!(rig.advance(), SkillStep::Act(_)));
            rig.complete();
            rig.read(Some(value), None, false);
        }
        assert!(matches!(
            rig.advance(),
            SkillStep::Done(SkillResult::NotFound {
                observation_complete: true,
                search_complete: false,
                ..
            })
        ));
        assert_eq!(rig.run.actions(), 5);
    }
}
