//! nns-vesl — .nock name registrar hull (NNS: the .nock Name Service).
//!
//! The Hoon kernel in `hoon/app/app.hoon` is the authoritative store:
//! `names`, `tx-hashes`, and `primaries` are all maintained there,
//! and every uniqueness / ownership rule is enforced by the kernel
//! (the Nockchain equivalent of a Solidity revert). The Rust hull
//! hosts the kernel, exposes an HTTP API, and keeps a denormalized
//! mirror for fast reads.
//!
//! Claim flow (`POST /claim`):
//!   1. hull validates input (address shape, name shape, matching
//!      pending reservation)
//!   2. in non-local modes, hull requires `txHash`; then calls
//!      `payment::verify`
//!   3. hull enqueues claim-note replay; follower pokes the kernel
//!      with `%claim name owner fee tx-hash`
//!   4. on `%claimed` success, hull mirrors the new row; on an
//!      accompanying `%primary-set` (first claim for this owner)
//!      the hull also updates its reverse-lookup index
//!   5. on `%claim-error`, the hull returns `400` without mutating
//!
//! Primary flow (`POST /primary`) is the same shape: one poke
//! (`%set-primary`), `%primary-set` / `%primary-error` effects.
//!
//! Settlement pokes (`%vesl-register` / `%vesl-settle`) are wired
//! on the Hoon side but not driven from the hot path yet — see the
//! graduation notes in `README.md`.

pub mod api;
pub mod chain;
pub mod chain_follower;
pub mod claim_note;
pub mod freshness;
pub mod kernel;
pub mod payment;
pub mod state;
pub mod types;
pub mod wallet_y4;

pub use state::{AppState, SharedState};
