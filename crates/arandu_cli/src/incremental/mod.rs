//! Incremental compilation session state and fingerprint persistence for Arandu CLI.

pub mod fingerprint;

pub use fingerprint::{
    IncrementalCheck, IncrementalInput, SessionConfig, check_incremental, content_digest,
    record_session,
};
