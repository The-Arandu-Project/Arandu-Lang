//! Shared package-cache orchestration for the Arandu CLI and LSP.
//!
//! Salsa queries never call this crate. Frontends materialize and verify
//! package trees first, then pass immutable paths and identities to
//! `arandu_query`.

pub mod cache;
pub mod remote_git;
pub mod resolver;
