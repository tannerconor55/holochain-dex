//! Parity harness: `unyt/dex_order_escrow/execution_code.rhai` run through the
//! published `rave_engine`, compared with `dex_core::execute_run`.
//!
//! The engine runs natively. The one thing that needs a conductor is the host
//! read behind `acceding_sort_allocation`; `MockHdkT` answers it with a parked
//! link record per taker spend (author, timestamp, amount), as rave_engine's
//! own tests do. Inputs are built with the engine's `RAVEInput` types, and
//! `previous_execution` is a serialized `RAVEOutput`, the shape
//! `derive_previous_execution` hands the script.

#![cfg(test)]

mod engine;
mod formatter;
mod operations;
mod scenarios;
mod differential;
