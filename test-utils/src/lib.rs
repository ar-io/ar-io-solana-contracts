//! Shared test helpers for AR.IO Solana programs.
//!
//! Today this provides one thing: parsing Anchor `#[event]` emissions out
//! of `solana-program-test` transaction logs so integration tests can
//! assert events fire with correct payloads.
//!
//! Anchor's `emit!` macro writes a `sol_log_data` syscall, which appears
//! in the transaction log as `Program data: <base64>`. The decoded blob
//! is `[discriminator(8) || borsh_payload]` where the discriminator is
//! `sha256("event:<EventName>")[0..8]` — surfaced by anchor as the
//! `Discriminator::DISCRIMINATOR` const on the event type.
//!
//! ## BPF dispatch is required for event capture
//!
//! `solana-program-test` 2.1.0's `SyscallStubs` overrides `sol_log` (so
//! `msg!` works in native dispatch) but **not** `sol_log_data`. That
//! means events only land in `log_messages` when the test runs the
//! actual BPF program, not the native processor shim.
//!
//! The pattern: fresh-build the BPF first, then run tests with
//! `BPF_OUT_DIR` pointing at `contracts/target/deploy`. Wrap each event
//! integration test with the [`bpf_required!`] macro so `cargo test`
//! without `BPF_OUT_DIR` cleanly skips them instead of false-failing.
//!
//! ```bash
//! bash contracts/build-sbf.sh
//! BPF_OUT_DIR="$(pwd)/contracts/target/deploy" cargo test -p ario-core
//! ```
//!
//! ## Typical usage
//!
//! ```ignore
//! use ario_test_utils::{bpf_required, expect_event};
//!
//! #[tokio::test]
//! async fn my_event_test() {
//!     bpf_required!();
//!     // ... build & submit a tx ...
//!     let result = ctx.banks_client.process_transaction_with_metadata(tx).await.unwrap();
//!     let logs = result.metadata.expect("metadata").log_messages;
//!
//!     let event = expect_event!(&logs, TransferEvent);
//!     assert_eq!(event.from, payer.pubkey());
//!     assert_eq!(event.amount, 1_000_000);
//! }
//! ```

use anchor_lang::{AnchorDeserialize, Discriminator};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

const PROGRAM_DATA_PREFIX: &str = "Program data: ";

/// Find the first event of type `T` in `logs`.
///
/// Returns `None` if no event with the matching discriminator is found.
/// Use [`parse_all_events`] when an instruction may emit multiple events
/// of the same type (rare — by convention, batched instructions emit at
/// most one summary event).
pub fn parse_event<T>(logs: &[String]) -> Option<T>
where
    T: Discriminator + AnchorDeserialize,
{
    decoded_event_blobs(logs).find_map(|blob| try_decode::<T>(&blob))
}

/// Find every event of type `T` in `logs`, in order of appearance.
pub fn parse_all_events<T>(logs: &[String]) -> Vec<T>
where
    T: Discriminator + AnchorDeserialize,
{
    decoded_event_blobs(logs)
        .filter_map(|blob| try_decode::<T>(&blob))
        .collect()
}

/// Returns true if any event of type `T` is present.
pub fn has_event<T>(logs: &[String]) -> bool
where
    T: Discriminator + AnchorDeserialize,
{
    parse_event::<T>(logs).is_some()
}

fn decoded_event_blobs(logs: &[String]) -> impl Iterator<Item = Vec<u8>> + '_ {
    logs.iter().filter_map(|line| {
        let payload = line.strip_prefix(PROGRAM_DATA_PREFIX)?;
        B64.decode(payload.trim()).ok()
    })
}

fn try_decode<T>(blob: &[u8]) -> Option<T>
where
    T: Discriminator + AnchorDeserialize,
{
    if blob.len() < 8 {
        return None;
    }
    if &blob[..8] != T::DISCRIMINATOR {
        return None;
    }
    T::try_from_slice(&blob[8..]).ok()
}

/// Parse the first event of type `T` from the given log slice or panic
/// with a clear message identifying the missing type. Use this in tests
/// where the event is mandatory (happy path).
#[macro_export]
macro_rules! expect_event {
    ($logs:expr, $event_ty:ty) => {{
        match $crate::parse_event::<$event_ty>($logs) {
            Some(ev) => ev,
            None => panic!(
                "Expected event {} not found in transaction logs. Captured logs:\n{}",
                std::any::type_name::<$event_ty>(),
                $logs.join("\n")
            ),
        }
    }};
}

/// Assert that no event of type `T` was emitted. Use on revert paths
/// where the instruction failed and should not have logged anything.
///
/// Does not require `T: Debug` — events emitted by anchor's `#[event]`
/// macro do not implement `Debug` by default, and we don't want to
/// force every event author to derive it just for the negative test.
/// If you need to see the payload that fired, look at the captured
/// `Program data:` lines in the test output.
#[macro_export]
macro_rules! assert_no_event {
    ($logs:expr, $event_ty:ty) => {{
        if $crate::has_event::<$event_ty>($logs) {
            panic!(
                "Unexpected event {} present in transaction logs.\nLogs:\n{}",
                std::any::type_name::<$event_ty>(),
                $logs.join("\n")
            );
        }
    }};
}

/// Skip the current `#[tokio::test]` cleanly when `BPF_OUT_DIR` is unset.
///
/// Event capture in `solana-program-test` 2.1.0 only works with BPF
/// dispatch (see crate-level docs). Without `BPF_OUT_DIR`, event
/// integration tests would false-fail because `log_messages` won't
/// include `Program data:` lines.
///
/// Place this as the first line of any `#[tokio::test]` that asserts
/// on emitted events. CI runs the suite with `BPF_OUT_DIR` set; local
/// `cargo test` without BPF skips gracefully with a hint.
#[macro_export]
macro_rules! bpf_required {
    () => {{
        if std::env::var("BPF_OUT_DIR").is_err() {
            eprintln!(
                "[ario-test-utils] SKIP: this test requires BPF_OUT_DIR. \
                 Run: bash contracts/build-sbf.sh && \
                 BPF_OUT_DIR=\"$(pwd)/contracts/target/deploy\" cargo test ..."
            );
            return;
        }
    }};
}

/// Assert that exactly `n` events of type `T` were emitted.
///
/// Useful for batched instructions where we want to confirm a single
/// summary event fired (not zero, not more). Returns the parsed events
/// for further inspection.
#[macro_export]
macro_rules! expect_event_count {
    ($logs:expr, $event_ty:ty, $n:expr) => {{
        let events = $crate::parse_all_events::<$event_ty>($logs);
        let n: usize = $n;
        assert_eq!(
            events.len(),
            n,
            "Expected exactly {} {} event(s), found {}.\nLogs:\n{}",
            n,
            std::any::type_name::<$event_ty>(),
            events.len(),
            $logs.join("\n")
        );
        events
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::*;

    #[event]
    struct DummyEvent {
        a: u64,
        b: i64,
        c: String,
    }

    #[event]
    struct OtherEvent {
        x: u32,
    }

    fn emit_dummy_log(a: u64, b: i64, c: &str) -> String {
        let mut payload = DummyEvent::DISCRIMINATOR.to_vec();
        let ev = DummyEvent {
            a,
            b,
            c: c.to_string(),
        };
        AnchorSerialize::serialize(&ev, &mut payload).unwrap();
        format!("Program data: {}", B64.encode(payload))
    }

    #[test]
    fn parses_basic_event_from_logs() {
        let logs = vec![
            "Program 11111111111111111111111111111111 invoke [1]".to_string(),
            emit_dummy_log(42, -7, "hello"),
            "Program 11111111111111111111111111111111 success".to_string(),
        ];
        let ev = parse_event::<DummyEvent>(&logs).expect("dummy event missing");
        assert_eq!(ev.a, 42);
        assert_eq!(ev.b, -7);
        assert_eq!(ev.c, "hello");
    }

    #[test]
    fn ignores_non_matching_discriminators() {
        let logs = vec![emit_dummy_log(1, 1, "x")];
        assert!(parse_event::<OtherEvent>(&logs).is_none());
    }

    #[test]
    fn ignores_non_program_data_lines() {
        let logs = vec![
            "Program log: not an event".to_string(),
            "random noise".to_string(),
        ];
        assert!(parse_event::<DummyEvent>(&logs).is_none());
    }

    #[test]
    fn parses_multiple_same_type() {
        let logs = vec![emit_dummy_log(1, 2, "a"), emit_dummy_log(3, 4, "b")];
        let evs = parse_all_events::<DummyEvent>(&logs);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].a, 1);
        assert_eq!(evs[1].c, "b");
    }

    #[test]
    fn has_event_true_and_false() {
        let logs_with = vec![emit_dummy_log(1, 2, "x")];
        let logs_without: Vec<String> = vec![];
        assert!(has_event::<DummyEvent>(&logs_with));
        assert!(!has_event::<DummyEvent>(&logs_without));
    }

    #[test]
    #[should_panic(expected = "Expected event")]
    fn expect_event_panics_when_missing() {
        let logs: Vec<String> = vec![];
        let _ = expect_event!(&logs, DummyEvent);
    }

    #[test]
    #[should_panic(expected = "Unexpected event")]
    fn assert_no_event_panics_when_present() {
        let logs = vec![emit_dummy_log(1, 2, "y")];
        assert_no_event!(&logs, DummyEvent);
    }

    #[test]
    fn assert_no_event_passes_when_absent() {
        let logs: Vec<String> = vec![];
        assert_no_event!(&logs, DummyEvent);
    }

    #[test]
    fn expect_event_count_returns_events() {
        let logs = vec![emit_dummy_log(1, 1, "a"), emit_dummy_log(2, 2, "b")];
        let events = expect_event_count!(&logs, DummyEvent, 2);
        assert_eq!(events[0].a, 1);
        assert_eq!(events[1].a, 2);
    }

    #[test]
    #[should_panic(expected = "Expected exactly 1")]
    fn expect_event_count_panics_on_mismatch() {
        let logs = vec![emit_dummy_log(1, 1, "a"), emit_dummy_log(2, 2, "b")];
        let _ = expect_event_count!(&logs, DummyEvent, 1);
    }

    #[test]
    fn ignores_truncated_blobs() {
        // 4-byte blob — too short for a discriminator
        let logs = vec![format!("Program data: {}", B64.encode([0u8; 4]))];
        assert!(parse_event::<DummyEvent>(&logs).is_none());
    }
}
