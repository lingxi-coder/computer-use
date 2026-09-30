//! The ledger of held inputs this system owns (design 14.4).
//!
//! A down is written here before it is submitted, so a lost acknowledgement
//! still leaves the release owed. Cleanup only ever releases what is in here.

use std::collections::{BTreeMap, BTreeSet};

use interaction_contracts::desktop::primitive::Primitive;

/// `(session_epoch, intent_id)`.
pub type Key = (u64, String);

#[derive(Debug, Clone)]
pub struct OwnedInput {
    pub record: Key,
    pub release: Primitive,
}

#[derive(Debug, Default)]
pub struct InputLedger {
    owned: BTreeMap<String, OwnedInput>,
}

impl InputLedger {
    /// Record a possible hold before the down is submitted.
    pub fn track_down(&mut self, key: &Key, primitive: &Primitive) {
        if let (Some((token, _)), Some(release)) = (primitive.held_token(), primitive.release_of())
        {
            self.owned.insert(
                token,
                OwnedInput {
                    record: key.clone(),
                    release,
                },
            );
        }
    }

    /// A down that was definitely not applied is not owed a release.
    pub fn untrack_down(&mut self, key: &Key, primitive: &Primitive) {
        if let Some((token, true)) = primitive.held_token()
            && self.owner_of(&token) == Some(key)
        {
            self.owned.remove(&token);
        }
    }

    /// The matching up was confirmed, so nothing is owed any more.
    pub fn confirm_up(&mut self, key: &Key, primitive: &Primitive) {
        if let Some((token, false)) = primitive.held_token()
            && self.owner_of(&token) == Some(key)
        {
            self.owned.remove(&token);
        }
    }

    pub fn owner_of(&self, token: &str) -> Option<&Key> {
        self.owned.get(token).map(|owned| &owned.record)
    }

    pub fn holds(&self, key: &Key) -> bool {
        self.owned.values().any(|owned| &owned.record == key)
    }

    pub fn tokens_of(&self, key: &Key) -> Vec<String> {
        self.owned
            .iter()
            .filter(|(_, owned)| &owned.record == key)
            .map(|(token, _)| token.clone())
            .collect()
    }

    pub fn release_of(&self, token: &str) -> Option<Primitive> {
        self.owned.get(token).map(|owned| owned.release.clone())
    }

    pub fn take(&mut self, token: &str) -> Option<OwnedInput> {
        self.owned.remove(token)
    }

    pub fn record_keys(&self) -> BTreeSet<Key> {
        self.owned
            .values()
            .map(|owned| owned.record.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.owned.len()
    }

    pub fn is_empty(&self) -> bool {
        self.owned.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::primitive::MouseButton;

    fn key(id: &str) -> Key {
        (1, id.into())
    }

    #[test]
    fn a_down_is_owed_a_release_until_the_up_is_confirmed() {
        let mut ledger = InputLedger::default();
        let down = Primitive::KeyDown { key: "W".into() };
        ledger.track_down(&key("a"), &down);
        assert!(ledger.holds(&key("a")));
        assert_eq!(ledger.tokens_of(&key("a")), vec!["key:W".to_string()]);
        assert_eq!(
            ledger.release_of("key:W"),
            Some(Primitive::KeyUp { key: "W".into() })
        );
        ledger.confirm_up(&key("a"), &Primitive::KeyUp { key: "W".into() });
        assert!(ledger.is_empty());
    }

    #[test]
    fn only_the_owner_can_settle_or_untrack_an_input() {
        let mut ledger = InputLedger::default();
        let down = Primitive::PointerDown {
            button: MouseButton::Left,
        };
        ledger.track_down(&key("a"), &down);
        ledger.confirm_up(
            &key("b"),
            &Primitive::PointerUp {
                button: MouseButton::Left,
            },
        );
        ledger.untrack_down(&key("b"), &down);
        assert_eq!(ledger.len(), 1, "another record cannot clear what a owns");
        ledger.untrack_down(&key("a"), &down);
        assert!(ledger.is_empty(), "a rejected down owes nothing");
    }

    #[test]
    fn atomic_primitives_and_moves_own_nothing() {
        let mut ledger = InputLedger::default();
        ledger.track_down(&key("a"), &Primitive::PointerMove { x: 1, y: 1 });
        ledger.track_down(&key("a"), &Primitive::Scroll { dx: 0, dy: 1 });
        assert!(ledger.is_empty());
    }
}
