//! CLI helper that prints the SELECTED observer operator pubkeys for an epoch,
//! computed off-chain by replicating `prescribe_epoch`'s weighted-roulette
//! selection byte-for-byte.
//!
//! Used by the cross-language parity test in
//! `ar-io-sdk/src/solana/predict-prescribed-observers.test.ts` to assert that
//! the TypeScript `predictPrescribedObservers` helper reproduces the on-chain
//! selection exactly. The cranker mirrors this to learn which Gateway PDAs to
//! pass as `remaining_accounts`, so it supplies ~50 accounts instead of the
//! whole registry (avoids `MAX_TX_ACCOUNT_LOCKS = 64`).
//!
//! ## Parity contract (DO NOT let this drift)
//!
//! This is a faithful replication of the observer-selection loop in
//! `programs/ario-gar/src/instructions/epoch.rs` (the `prescribe_epoch`
//! handler, observer-selection block). It is intentionally NOT shared code
//! with the on-chain handler — that handler is audited consensus logic on the
//! reward-distribution path and is deliberately left untouched. The guard
//! against drift is twofold:
//!   1. The SDK parity test shells out to THIS example and asserts TS == Rust.
//!   2. The BPF integration test (when present) asserts the on-chain selection
//!      equals this reference for the same inputs.
//! If you change the on-chain roulette, update this file and both tests in the
//! same change.
//!
//! Mirrored invariants (see epoch.rs comments for rationale):
//!   - entropy: `hash = sha256(epoch.hashchain)`, re-hashed each round as
//!     `sha256(hash_bytes)` (`anchor_lang::solana_program::hash::hash`)
//!   - `random_value = u128::from_le_bytes(hash[..16]) % total_weight`
//!     (LITTLE-endian, first 16 bytes only)
//!   - `total_weight` = LIVE sum of `composite_weight` over the supplied slots
//!     (caller passes exactly `epoch.active_gateway_count` slots, in registry
//!     order) — the Codex 2026-05-28 live-total fix, NOT the tally snapshot
//!   - cumulative walk selects the first slot where
//!     `cumulative > random_value && composite_weight > 0`
//!   - anti-duplicate: a slot already selected is skipped, but the round still
//!     consumes the roulette hit (we `break` the inner walk either way)
//!   - bounded retries: `max_observers * 10` rounds (GAR-019)
//!   - the SELECTED value is `GatewaySlot::address` (the operator pubkey), which
//!     is what Gateway PDAs are derived from: `[GATEWAY_SEED, operator]`. This
//!     is `epoch.prescribed_observer_gateways`, NOT the `prescribed_observers`
//!     array (that one is later overwritten on-chain with each gateway's
//!     resolved `observer_address`).
//!
//! ## Usage
//!
//! Reads the problem from stdin (so the slot list can be arbitrarily long):
//!
//! ```text
//! line 1:      <hashchain_hex_64chars>        # epoch.hashchain, 32 bytes hex
//! line 2:      <max_observers_decimal>        # min(prescribed_observer_count, active_count)
//! lines 3..N:  <operator_base58> <composite_weight_decimal>   # one slot per line, registry order
//! ```
//!
//! Output: the selected operator pubkeys (base58), one per line, in selection
//! order. Empty output means nothing was selected (zero total weight / no slots).
//!
//! ```sh
//! printf '%s\n' "$HASHCHAIN_HEX" 3 "Op1... 100" "Op2... 50" \
//!   | cargo run -p ario-gar --example predict_prescribed_observers
//! ```

use std::io::{self, Read};
use std::str::FromStr;

use anchor_lang::solana_program::hash::hash;
use anchor_lang::solana_program::pubkey::Pubkey;

struct Slot {
    address: Pubkey,
    composite_weight: u64,
}

fn main() {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .expect("failed to read stdin");

    let mut lines = input.lines();

    let hashchain_hex = lines.next().expect("missing line 1: hashchain hex").trim();
    let hashchain = decode_hashchain(hashchain_hex);

    let max_observers: usize = lines
        .next()
        .expect("missing line 2: max_observers")
        .trim()
        .parse()
        .expect("max_observers must be a non-negative integer");

    let mut slots: Vec<Slot> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let addr = parts.next().expect("slot line missing operator pubkey");
        let weight = parts.next().expect("slot line missing composite_weight");
        slots.push(Slot {
            address: Pubkey::from_str(addr)
                .unwrap_or_else(|e| panic!("invalid operator pubkey '{}': {}", addr, e)),
            composite_weight: weight
                .parse()
                .unwrap_or_else(|e| panic!("invalid composite_weight '{}': {}", weight, e)),
        });
    }

    let selected = select_prescribed_observer_gateways(&hashchain, &slots, max_observers);

    let mut out = String::new();
    for op in &selected {
        out.push_str(&op.to_string());
        out.push('\n');
    }
    print!("{}", out);
}

/// Faithful replication of the `prescribe_epoch` observer-selection roulette.
/// `slots` are the registry slots `[0..epoch.active_gateway_count]`, in order.
/// Returns the selected operator pubkeys (`GatewaySlot::address`) in selection
/// order, at most `max_observers`.
fn select_prescribed_observer_gateways(
    hashchain: &[u8; 32],
    slots: &[Slot],
    max_observers: usize,
) -> Vec<Pubkey> {
    let active_count = slots.len();
    // Cap at the number of supplied slots, mirroring
    //   max_observers = min(prescribed_observer_count, active_count)
    // (the caller passes prescribed_observer_count as `max_observers`; we apply
    // the active_count clamp here so an over-large value can't loop forever).
    let max_observers = std::cmp::min(max_observers, active_count);

    // LIVE total weight (epoch.rs:523-526). saturating_add to match.
    let mut total_weight: u128 = 0;
    for slot in slots {
        total_weight = total_weight.saturating_add(slot.composite_weight as u128);
    }

    let mut selected: Vec<Pubkey> = Vec::new();
    if total_weight == 0 || active_count == 0 || max_observers == 0 {
        return selected;
    }

    // Initial entropy: sha256(hashchain) (epoch.rs:533).
    let mut h = hash(hashchain);

    // GAR-019: up to max_observers * 10 rounds.
    for _ in 0..max_observers * 10 {
        if selected.len() >= max_observers {
            break;
        }

        let hash_bytes = h.to_bytes();
        let random_value = u128::from_le_bytes(hash_bytes[..16].try_into().unwrap()) % total_weight;

        let mut cumulative: u128 = 0;
        for slot in slots {
            cumulative += slot.composite_weight as u128;
            if cumulative > random_value && slot.composite_weight > 0 {
                // Anti-duplicate (epoch.rs:563) — skip if already chosen, but
                // still break the walk: the roulette hit is consumed either way.
                if !selected.contains(&slot.address) {
                    selected.push(slot.address);
                }
                break;
            }
        }

        // Re-hash for the next round (epoch.rs:573).
        h = hash(&h.to_bytes());
    }

    selected
}

fn decode_hashchain(hex: &str) -> [u8; 32] {
    assert_eq!(
        hex.len(),
        64,
        "hashchain must be exactly 64 hex chars (32 bytes), got {}",
        hex.len()
    );
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .expect("hashchain contains a non-hex character");
    }
    out
}
