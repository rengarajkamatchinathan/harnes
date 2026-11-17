//! Session-level wire enums used by the session actor (01 §5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `session.status` as stored in the DB (01 §5.1, t12). `Paused` is `AwaitingAsk` with
/// no driver attached; `Interrupting` and `Compacting` are only ever ephemeral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    AwaitingAsk,
    Interrupted,
}

/// Ephemeral status reported live; a superset of [`SessionStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatus {
    Idle,
    Running,
    AwaitingAsk,
    /// `AwaitingAsk` with no client attached; an OS notification was raised.
    Paused,
    Compacting,
    Interrupting,
    Interrupted,
}

impl LiveStatus {
    /// The durable status this live status is stored as.
    pub fn durable(self) -> SessionStatus {
        match self {
            LiveStatus::Idle | LiveStatus::Interrupting => SessionStatus::Idle,
            LiveStatus::Running | LiveStatus::Compacting => SessionStatus::Running,
            LiveStatus::AwaitingAsk | LiveStatus::Paused => SessionStatus::AwaitingAsk,
            LiveStatus::Interrupted => SessionStatus::Interrupted,
        }
    }
}

/// How a prompt is delivered to a session (01 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Injected at the next step boundary of a running turn, or starts a turn if idle.
    #[default]
    Steer,
    /// A new turn after the current turn settles.
    Queue,
}

/// How a turn settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnFinish {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
