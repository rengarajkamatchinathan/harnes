//! `genie-providers`: the provider layer (spec/03-providers-and-models.md).
//!
//! - [`model`]: the canonical model (03 §2): messages, parts, opaque replay payloads, usage.
//! - [`event`]: the normalised stream event model every adapter emits (03 §3).
//! - [`sse`]: our own incremental SSE parser (D18).
//! - [`protocol`]: the `Protocol` trait and the four v1 adapters (03 §5).
//! - [`compile`]: canonical history → wire history (03 §4).
//! - [`error`] / [`retry`]: the error taxonomy, classification and retry policy (03 §7).
//! - [`executor`]: the single HTTP choke point (auth, timeouts, retry, cassettes; 03 §3).
//! - [`auth`], [`route`], [`compat`]: routes, presets, quirk flags and model specs.
//! - [`oauth`] (Entra PKCE / device code / refresh under a cross-process lock) and
//!   [`adc`] (Google ADC): [`TokenSource`]s for token-based routes (03 §10.2).
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol` and the
//! `genie-secrets` leaf only. Catalog data reaches it as a [`compat::ModelSpec`] built by
//! `genie-core` from a `genie-catalog` entry.

// `ProviderError` is deliberately a flat, inspectable struct (03 §7: kind, status, code,
// request ids, retry hint, message, excerpt); errors are rare next to stream events.
#![allow(clippy::result_large_err)]

pub mod adc;
pub mod auth;
pub mod cassette;
pub mod compat;
pub mod compile;
pub mod error;
pub mod event;
pub mod executor;
pub mod model;
pub mod oauth;
pub mod protocol;
pub mod retry;
pub mod route;
pub mod sse;

pub use auth::{AccessToken, Auth, AuthKind, TokenSource};
pub use compat::{Compat, ModelSpec};
pub use compile::{CompiledRequest, RouteInfo, compile};
pub use error::{ErrorKind, HttpFailure, ProviderError, RetryHint};
pub use event::StreamEvent;
pub use executor::{Executor, LlmClient, Transport};
pub use model::*;
pub use protocol::{ErasedProtocol, Framing, Protocol, RawFrame};
pub use retry::{RetryDecision, RetryPolicy};
pub use route::{Endpoint, Provider, Route};
