mod common;

use common::world::{TEXT_COMMIT, World, engine_config};
use interaction_contracts::desktop::task::RootState;
use interaction_engine::deliberation::accounting::ModelUsage;
use interaction_engine::deliberation::computer_accounting::{
    ComputerCallCompletion, ComputerCallOutcome,
};
use interaction_engine::engine::Input;
use interaction_engine::engine::computer::{ComputerCommand, ComputerInput};

#[test]
fn declined_safety_confirmation_pauses_instead_of_falling_back_to_plan_input() {
    let mut config = engine_config();
    config.computer_proposals_enabled = true;
    let mut world = World::with_config(config);
    world.start(TEXT_COMMIT);
    world.until(
        |world| {
            world
                .computer_commands
                .iter()
                .any(|command| matches!(command, ComputerCommand::Begin { .. }))
        },
        5_000,
    );

    let identity = world
        .take_computer_commands()
        .into_iter()
        .find_map(|command| match command {
            ComputerCommand::Begin { identity, .. } => Some(identity),
            _ => None,
        })
        .expect("the approved apply sub-goal starts a Computer conversation");

    world
        .engine
        .computer_call_started(world.now, &identity, "openai", "computer-test")
        .unwrap();
    let before_inputs = world.inputs.borrow().len();
    let before_decisions = world.decision_requests.len();
    let before_submits = world
        .device_log
        .iter()
        .filter(|(_, command)| *command == "submit")
        .count();
    world.send(Input::Computer(Box::new(ComputerInput::Finished {
        completion: ComputerCallCompletion {
            identity: identity.clone(),
            at_ms: world.now,
            outcome: ComputerCallOutcome::Proposal,
            usage: ModelUsage {
                input_tokens: Some(3),
                output_tokens: Some(2),
                cost_micros: Some(1),
            },
        },
    })));
    assert!(world.take_computer_commands().iter().any(|command| {
        matches!(
            command,
            ComputerCommand::CallFinished {
                identity: finished,
                effective_outcome: ComputerCallOutcome::Proposal,
            } if finished == &identity
        )
    }));

    world.send(Input::Computer(Box::new(
        ComputerInput::SafetyUnconfirmed {
            identity,
            reason: "operator declined the safety confirmation".into(),
        },
    )));

    assert!(matches!(
        world.engine.root_state(),
        Some(RootState::Pausing | RootState::Suspended)
    ));
    assert_eq!(world.inputs.borrow().len(), before_inputs);
    assert_eq!(world.decision_requests.len(), before_decisions);
    assert_eq!(
        before_submits,
        world
            .device_log
            .iter()
            .filter(|(_, command)| *command == "submit")
            .count()
    );
    let (outcome, usage_json): (String, String) = world
        .engine
        .storage()
        .read()
        .query_row(
            "SELECT outcome, usage_json FROM computer_model_calls",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(outcome, "proposal");
    let usage: ModelUsage = serde_json::from_str(&usage_json).unwrap();
    assert_eq!(usage.input_tokens, Some(3));
    assert_eq!(usage.output_tokens, Some(2));
    assert_eq!(usage.cost_micros, Some(1));
}

#[test]
fn computer_lane_is_disabled_by_default_and_emits_no_commands() {
    let mut world = World::new();
    world.start(TEXT_COMMIT);
    assert!(world.take_computer_commands().is_empty());
}
