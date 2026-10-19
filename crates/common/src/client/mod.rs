//! Client side shared by the agent and the relay: enrollment, the identity
//! on disk, certificate renewal, reconnect backoff and failure classes,
//! and the shutdown signal.

pub mod backoff;
pub mod enroll;
pub mod failure;
pub mod identity;
pub mod renew;
pub mod shutdown;
