//! `genie-permissions`: permission rules, evaluation order, modes and taint
//! (spec/06-agent-runtime.md §5, spec/08-security-and-policy.md §3).
//!
//! - [`rule`]: the Claude-compatible grammar (`Tool`, `Tool(specifier)`, `mcp__server__tool`)
//!   plus Genie's `Browser(...)`, `Artifact(...)`, `WebSearch`.
//! - [`eval`]: [`Evaluator::evaluate`] — policy deny → deny → ask → allow → mode default,
//!   modes with the policy floor, the taint downgrade (SA-29), "always" saving and the
//!   re-evaluation of pending asks.
//! - [`shell`]: the rule side of the command scanner (read-only list, arity table).
//!
//! The crate is pure: rules come in as a [`RuleSet`] folded from the config layers by the
//! engine, and decisions go out as a [`Verdict`]. The engine adapts it to its own
//! `ToolAuthorizer` port.
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol` only.

pub mod eval;
pub mod path;
pub mod rule;
pub mod shell;

pub use eval::{
    Action, AllowedBy, AskReason, BrowserAction, Context, Evaluator, ModePolicy, Origin, Request,
    RuleClass, RuleSet, SaveTarget, Taint, Verdict, default_save_scope, effective_mode,
};
pub use rule::{Rule, RuleError};
pub use shell::{BuiltinReadOnly, ReadOnlyClassifier, ShellDialect, arity_prefix};
