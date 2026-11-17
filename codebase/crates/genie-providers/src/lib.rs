//! `genie-providers`: Protocol adapters (Anthropic Messages, OpenAI Responses, OpenAI Chat, Gemini), Executor, SSE parser, Compat, retry.
//!
//! Placeholder created by the architecture milestone (spec/01-architecture.md §2) so the
//! crate graph and dependency rules exist from day one. Implementation: spec/03-providers-and-models.md.
//!
//! Dependency rule (01 §2.1 rule 3): this crate may depend on `genie-protocol` only
//! (plus the leaf exceptions in `xtask/src/deps.rs`). Anything it needs from another
//! subsystem is a trait defined in `genie-core` and injected by `EngineBuilder`.
