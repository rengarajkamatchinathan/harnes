//! `genie-sandbox`: v2: OS-native sandboxes (design accepted, D44).
//!
//! Placeholder created by the architecture milestone (spec/01-architecture.md §2) so the
//! crate graph and dependency rules exist from day one. Implementation: v2 (ticket 09).
//!
//! Dependency rule (01 §2.1 rule 3): this crate may depend on `genie-protocol` only
//! (plus the leaf exceptions in `xtask/src/deps.rs`). Anything it needs from another
//! subsystem is a trait defined in `genie-core` and injected by `EngineBuilder`.
