//! The root task transition table (design 13.5). Anything not listed is refused.

use interaction_contracts::desktop::task::{RootEvent, RootState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionError {
    NotAllowed {
        from: RootState,
        event: RootEvent,
    },
    /// An answer arrived for a root that never asked a question.
    MissingOrigin,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAllowed { from, event } => {
                write!(f, "event {event:?} is not allowed in state {from:?}")
            }
            Self::MissingOrigin => f.write_str("no question is waiting for an answer"),
        }
    }
}

impl std::error::Error for TransitionError {}

/// The next state, or why the move is refused.
///
/// `origin` is where a NEEDS_INPUT root came from; it decides where the
/// answer sends it back to.
pub fn transition(
    from: RootState,
    event: RootEvent,
    origin: Option<RootState>,
) -> Result<RootState, TransitionError> {
    use RootEvent::*;
    use RootState::*;
    let refused = Err(TransitionError::NotAllowed { from, event });
    match (from, event) {
        (Draft, SubmitGoal) => Ok(CompilingGoal),
        (CompilingGoal, DraftPassed) => Ok(AwaitingConfirmation),
        (CompilingGoal, AmbiguityFound) => Ok(NeedsInput),
        (CompilingGoal, CompileFailed) => Ok(Failed),
        (NeedsInput, UserAnswered) => match origin {
            Some(CompilingGoal) => Ok(CompilingGoal),
            Some(Running | WaitingEvidence | WaitingPlan) => Ok(Running),
            _ => Err(TransitionError::MissingOrigin),
        },
        (AwaitingConfirmation, UserConfirmed) => Ok(Ready),
        (AwaitingConfirmation, UserEdited) => Ok(CompilingGoal),
        (Ready, AdmissionPassed) => Ok(Running),
        (Running, AwaitEvidence) => Ok(WaitingEvidence),
        (Running, AwaitPlan) => Ok(WaitingPlan),
        (WaitingEvidence, EvidenceArrived) => Ok(Running),
        (WaitingPlan, PlanInstalled) => Ok(Running),
        (Running | WaitingEvidence | WaitingPlan, NeedUserInfo) => Ok(NeedsInput),
        (Running | WaitingEvidence | WaitingPlan | NeedsInput, PauseRequested) => Ok(Pausing),
        (Pausing, InputsReleased) => Ok(Suspended),
        (Suspended, ResumeRequested) => Ok(NeedsRevalidation),
        // A root that was being cancelled when the process died is finished,
        // not revalidated: the cancellation flag is already durable.
        (state, ProcessRestarted) if !state.is_terminal() && state != Cancelling => {
            Ok(NeedsRevalidation)
        }
        (NeedsRevalidation, RevalidationPassed) => Ok(Running),
        (Running, PredicatesSatisfied) => Ok(Completed),
        // Waiting timeouts follow the failure category, so they can fail too.
        (Running | WaitingEvidence | WaitingPlan, Unrecoverable) => Ok(Failed),
        (Running, InputEndedUnverified) => Ok(Unverified),
        // A stop already in progress is not turned into an expiry.
        (state, DeadlineExpired) if !state.is_terminal() && state != Cancelling => Ok(Expired),
        (state, StopRequested) if !state.is_terminal() => Ok(Cancelling),
        (Cancelling, CancelFinished) => Ok(Cancelled),
        _ => refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RootEvent::*;
    use RootState::*;

    const ALL_STATES: [RootState; 17] = [
        Draft,
        CompilingGoal,
        AwaitingConfirmation,
        NeedsInput,
        Ready,
        Running,
        WaitingEvidence,
        WaitingPlan,
        Pausing,
        Suspended,
        NeedsRevalidation,
        Cancelling,
        Completed,
        Failed,
        Unverified,
        Expired,
        Cancelled,
    ];

    const ALL_EVENTS: [RootEvent; 24] = [
        SubmitGoal,
        DraftPassed,
        AmbiguityFound,
        CompileFailed,
        UserConfirmed,
        UserEdited,
        AdmissionPassed,
        AwaitEvidence,
        AwaitPlan,
        EvidenceArrived,
        PlanInstalled,
        NeedUserInfo,
        UserAnswered,
        PauseRequested,
        InputsReleased,
        ResumeRequested,
        ProcessRestarted,
        RevalidationPassed,
        PredicatesSatisfied,
        Unrecoverable,
        InputEndedUnverified,
        DeadlineExpired,
        StopRequested,
        CancelFinished,
    ];

    fn non_terminal() -> Vec<RootState> {
        ALL_STATES
            .into_iter()
            .filter(|s| !s.is_terminal())
            .collect()
    }

    /// The design table, written out independently of `transition`.
    fn expected() -> Vec<(RootState, RootEvent, RootState)> {
        let mut rows = vec![
            (Draft, SubmitGoal, CompilingGoal),
            (CompilingGoal, DraftPassed, AwaitingConfirmation),
            (CompilingGoal, AmbiguityFound, NeedsInput),
            (CompilingGoal, CompileFailed, Failed),
            (AwaitingConfirmation, UserConfirmed, Ready),
            (AwaitingConfirmation, UserEdited, CompilingGoal),
            (Ready, AdmissionPassed, Running),
            (Running, AwaitEvidence, WaitingEvidence),
            (Running, AwaitPlan, WaitingPlan),
            (WaitingEvidence, EvidenceArrived, Running),
            (WaitingPlan, PlanInstalled, Running),
            (Pausing, InputsReleased, Suspended),
            (Suspended, ResumeRequested, NeedsRevalidation),
            (NeedsRevalidation, RevalidationPassed, Running),
            (Running, PredicatesSatisfied, Completed),
            (Running, InputEndedUnverified, Unverified),
            (Cancelling, CancelFinished, Cancelled),
        ];
        for state in [Running, WaitingEvidence, WaitingPlan] {
            rows.push((state, NeedUserInfo, NeedsInput));
            rows.push((state, Unrecoverable, Failed));
        }
        for state in [Running, WaitingEvidence, WaitingPlan, NeedsInput] {
            rows.push((state, PauseRequested, Pausing));
        }
        for state in non_terminal() {
            rows.push((state, StopRequested, Cancelling));
            if state != Cancelling {
                rows.push((state, ProcessRestarted, NeedsRevalidation));
                rows.push((state, DeadlineExpired, Expired));
            }
        }
        rows
    }

    #[test]
    fn exactly_the_listed_transitions_are_allowed() {
        let allowed = expected();
        let mut checked = 0;
        for from in ALL_STATES {
            for event in ALL_EVENTS {
                // UserAnswered needs an origin; it is covered separately below.
                if event == UserAnswered {
                    continue;
                }
                let want = allowed
                    .iter()
                    .find(|(f, e, _)| *f == from && *e == event)
                    .map(|(_, _, to)| *to);
                let got = transition(from, event, None).ok();
                assert_eq!(got, want, "{from:?} + {event:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, 17 * 23);
    }

    #[test]
    fn no_row_appears_twice_in_the_expected_table() {
        let rows = expected();
        for (index, (from, event, _)) in rows.iter().enumerate() {
            assert!(
                !rows[index + 1..]
                    .iter()
                    .any(|(f, e, _)| f == from && e == event),
                "duplicate row {from:?} + {event:?}"
            );
        }
    }

    #[test]
    fn an_answer_returns_to_where_the_question_came_from() {
        assert_eq!(
            transition(NeedsInput, UserAnswered, Some(CompilingGoal)),
            Ok(CompilingGoal)
        );
        for origin in [Running, WaitingEvidence, WaitingPlan] {
            assert_eq!(
                transition(NeedsInput, UserAnswered, Some(origin)),
                Ok(Running)
            );
        }
        assert_eq!(
            transition(NeedsInput, UserAnswered, None),
            Err(TransitionError::MissingOrigin)
        );
        assert_eq!(
            transition(NeedsInput, UserAnswered, Some(Suspended)),
            Err(TransitionError::MissingOrigin)
        );
        assert!(matches!(
            transition(Running, UserAnswered, Some(Running)),
            Err(TransitionError::NotAllowed { .. })
        ));
    }

    #[test]
    fn terminal_states_accept_nothing() {
        for state in [Completed, Failed, Unverified, Expired, Cancelled] {
            for event in ALL_EVENTS {
                assert!(
                    transition(state, event, Some(Running)).is_err(),
                    "{state:?} + {event:?}"
                );
            }
        }
    }

    #[test]
    fn cancelled_is_only_reachable_through_cancelling() {
        for from in ALL_STATES {
            for event in ALL_EVENTS {
                if transition(from, event, Some(Running)) == Ok(Cancelled) {
                    assert_eq!((from, event), (Cancelling, CancelFinished));
                }
            }
        }
    }

    #[test]
    fn a_stop_in_progress_is_neither_restarted_into_revalidation_nor_expired() {
        assert!(transition(Cancelling, ProcessRestarted, None).is_err());
        assert!(transition(Cancelling, DeadlineExpired, None).is_err());
        assert_eq!(
            transition(Cancelling, StopRequested, None),
            Ok(Cancelling),
            "stopping twice is harmless"
        );
    }
}
