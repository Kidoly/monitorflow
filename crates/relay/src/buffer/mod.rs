//! Store-and-forward buffer (ADR-0027): `SampleBatch` envelopes kept on
//! disk while an agent has no working upstream stream, replayed oldest
//! first once it has one.

pub mod store;

pub use store::{Limits, Stats, Store};
