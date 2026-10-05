//! Deterministic `client_order_id` (design D11, crash-recovery spec): the same pair, leg, action
//! and sequence number always give the same id, so after a restart the id can be recomputed from
//! the pair data and an order whose result is unknown is looked up under its original id.
//!
//! Layout (lowercase ASCII alphanumerics only, fixed-width fields so it parses unambiguously):
//! `<sim|demo><l|s><o|c><seq: 4 base36><hash: 13 base36><uuid head: 0–8 alnum>`, at most 31
//! characters. The spec allows `[A-Za-z0-9_-]` and 36 characters (unverified per exchange, D11);
//! this stays inside the narrower intersection (OKX `clOrdId` is alphanumeric, ≤ 32) on purpose.
//! `hash` is 64-bit FNV-1a of the full pair uuid, so uuids that differ only after the readable
//! head, in case or in separators still get different ids (a collision needs a 64-bit hash
//! collision AND the same head). Leg, action and seq are encoded injectively.

use super::ports::{Leg, OrderAction};

/// Which execution mode an id belongs to; the prefix marks simulated orders (`sim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdPrefix {
    Sim,
    Demo,
}

impl IdPrefix {
    pub const fn as_str(self) -> &'static str {
        match self {
            IdPrefix::Sim => "sim",
            IdPrefix::Demo => "demo",
        }
    }

    /// The prefix of an id produced by [`client_order_id`], if any.
    pub fn of(client_order_id: &str) -> Option<IdPrefix> {
        [IdPrefix::Sim, IdPrefix::Demo].into_iter().find(|p| {
            client_order_id.strip_prefix(p.as_str()).is_some_and(|rest| {
                let b = rest.as_bytes();
                let lower_alnum = |c: &u8| c.is_ascii_digit() || c.is_ascii_lowercase();
                b.len() >= FIXED_LEN
                    && b.len() <= FIXED_LEN + HEAD_LEN
                    && matches!(b[0], b'l' | b's')
                    && matches!(b[1], b'o' | b'c')
                    && b.iter().all(lower_alnum)
            })
        })
    }
}

/// Upper bound on the id length (spec: ≤ 36).
pub const MAX_LEN: usize = 36;

const SEQ_LEN: usize = 4; // u16::MAX = "1ekf"
const HASH_LEN: usize = 13; // u64::MAX = "3w5e11264sgsf"
const HEAD_LEN: usize = 8;
/// Length after the mode prefix without the uuid head: leg + action + seq + hash.
const FIXED_LEN: usize = 2 + SEQ_LEN + HASH_LEN;
const _: () = assert!(4 + FIXED_LEN + HEAD_LEN <= MAX_LEN);

/// The id of order number `seq` (0 first; a new number only for a genuinely new order, never for
/// a retry of an order whose result is unknown) of `leg`'s `action` in pair `pair_uuid`.
pub fn client_order_id(prefix: IdPrefix, pair_uuid: &str, leg: Leg, action: OrderAction, seq: u16) -> String {
    let leg = match leg {
        Leg::Long => 'l',
        Leg::Short => 's',
    };
    let action = match action {
        OrderAction::Open => 'o',
        OrderAction::Close => 'c',
    };
    let head: String =
        pair_uuid.chars().filter(char::is_ascii_alphanumeric).take(HEAD_LEN).map(|c| c.to_ascii_lowercase()).collect();
    format!(
        "{}{leg}{action}{}{}{head}",
        prefix.as_str(),
        base36(u64::from(seq), SEQ_LEN),
        base36(fnv1a64(pair_uuid.as_bytes()), HASH_LEN)
    )
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// Zero-padded lowercase base36 of exactly `width` digits (`width` must fit the value).
fn base36(mut v: u64, width: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = vec![b'0'; width];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(v % 36) as usize];
        v /= 36;
    }
    debug_assert_eq!(v, 0, "value does not fit {width} base36 digits");
    String::from_utf8(out).expect("ascii")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const ACTIONS: [OrderAction; 2] = [OrderAction::Open, OrderAction::Close];

    fn valid(id: &str) -> bool {
        !id.is_empty() && id.len() <= MAX_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    #[test]
    fn same_input_same_output_and_the_mode_prefix_is_visible() {
        let u = "6f1c2d3e-4b5a-4c6d-8e7f-0123456789ab";
        let a = client_order_id(IdPrefix::Sim, u, Leg::Long, OrderAction::Open, 0);
        let b = client_order_id(IdPrefix::Sim, u, Leg::Long, OrderAction::Open, 0);
        assert_eq!(a, b);
        assert!(valid(&a), "{a:?}");
        assert!(a.starts_with("sim"), "{a}");
        let d = client_order_id(IdPrefix::Demo, u, Leg::Long, OrderAction::Open, 0);
        assert!(d.starts_with("demo"), "{d}");
        assert_ne!(a, d);
        assert_eq!(IdPrefix::of(&a), Some(IdPrefix::Sim));
        assert_eq!(IdPrefix::of(&d), Some(IdPrefix::Demo));
        assert_eq!(IdPrefix::of("x123"), None);
        assert_eq!(IdPrefix::of(""), None);
    }

    #[test]
    fn leg_action_and_seq_each_change_the_id() {
        let u = "pair-1";
        let mut seen = HashSet::new();
        for leg in Leg::BOTH {
            for action in ACTIONS {
                for seq in [0u16, 1, 2, 35, 36, 1295, 1296, u16::MAX] {
                    let id = client_order_id(IdPrefix::Sim, u, leg, action, seq);
                    assert!(valid(&id), "{id:?}");
                    assert!(seen.insert(id.clone()), "duplicate {id} for {leg:?} {action:?} {seq}");
                }
            }
        }
    }

    #[test]
    fn hostile_or_long_pair_uuids_still_give_valid_ids() {
        let long = "x".repeat(500);
        for u in ["", "-", "ÄÖÜ✓", "a b/c\\d\"e';--", long.as_str(), "6F1C2D3E-4B5A-4C6D-8E7F-0123456789AB"] {
            for prefix in [IdPrefix::Sim, IdPrefix::Demo] {
                let id = client_order_id(prefix, u, Leg::Short, OrderAction::Close, u16::MAX);
                assert!(valid(&id), "{u:?} -> {id:?}");
                assert_eq!(IdPrefix::of(&id), Some(prefix));
            }
        }
    }

    #[test]
    fn pair_uuids_that_differ_only_in_separators_or_tail_get_different_ids() {
        let pairs = [
            ("a-b", "ab"),
            ("6f1c2d3e-4b5a-4c6d-8e7f-0123456789ab", "6f1c2d3e-4b5a-4c6d-8e7f-0123456789ac"),
            ("ABC", "abc"),
        ];
        for (x, y) in pairs {
            assert_ne!(
                client_order_id(IdPrefix::Sim, x, Leg::Long, OrderAction::Open, 0),
                client_order_id(IdPrefix::Sim, y, Leg::Long, OrderAction::Open, 0),
                "{x} vs {y}"
            );
        }
    }

    /// Property-style: many pseudo-random inputs (fixed seed, no new dependency). Every id is
    /// valid, recomputes identically, and distinct inputs give distinct ids.
    #[test]
    fn property_valid_deterministic_and_injective_over_many_inputs() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        let alphabet: Vec<char> = "0123456789abcdefABCDEF-_ ./é".chars().collect();
        let mut by_id: std::collections::HashMap<String, (IdPrefix, String, Leg, OrderAction, u16)> = Default::default();
        for _ in 0..20_000 {
            let len = (next() % 40) as usize;
            let u: String = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let prefix = if next() % 2 == 0 { IdPrefix::Sim } else { IdPrefix::Demo };
            let leg = Leg::BOTH[(next() % 2) as usize];
            let action = ACTIONS[(next() % 2) as usize];
            let seq = (next() % 70_000).min(u16::MAX as u64) as u16;
            let id = client_order_id(prefix, &u, leg, action, seq);
            assert!(valid(&id), "{u:?} -> {id:?}");
            assert_eq!(id, client_order_id(prefix, &u, leg, action, seq), "recompute differs");
            assert_eq!(IdPrefix::of(&id), Some(prefix));
            let input = (prefix, u.clone(), leg, action, seq);
            if let Some(prev) = by_id.insert(id.clone(), input.clone()) {
                assert_eq!(prev, input, "collision on {id}");
            }
        }
    }
}
