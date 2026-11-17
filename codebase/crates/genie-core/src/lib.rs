//! `genie-core`: the engine inside the daemon (spec/01-architecture.md §3, §5, §6, §7).
//!
//! * [`task::TaskScope`]: every task runs on a `TaskTracker` with a child
//!   `CancellationToken` (01 §6). Raw `tokio::spawn` is banned outside this module.
//! * [`engine::EngineBuilder`]: explicit wiring of process-wide services, no DI framework.
//! * [`dir_context::DirContexts`]: lazily built, shared per-directory contexts.
//! * [`session`]: one actor per open session, with a durable inbox (steer/queue),
//!   interrupt, resume and asks.
//! * [`bus::Bus`]: per-session `seq`, journal + projectors, then fan-out.
//! * [`error`]: the layered error taxonomy (01 §7).
//! * [`locks::PathLocks`]: one async mutex per canonical path, shared by all sessions.

pub mod bus;
pub mod config;
pub mod dir_context;
pub mod engine;
pub mod error;
pub mod ids;
pub mod journal;
pub mod locks;
pub mod runtime;
pub mod session;
pub mod task;

pub use engine::{BusySummary, Engine, EngineBuilder};
pub use error::{Defect, ToolError, TurnError};
pub use session::{SessionHandle, TurnCtx, TurnRunner};
pub use task::TaskScope;
