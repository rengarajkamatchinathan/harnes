//! The running turn of each session, for subsystems that must reach the turn from outside
//! a tool call: MCP elicitation and sampling requests become asks of the turn whose tool
//! call is running on that server (06 §7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::session::TurnCtx;

#[derive(Default)]
pub struct ActiveTurns {
    map: Mutex<HashMap<String, Arc<TurnCtx>>>,
}

/// Removes the entry when the turn ends.
pub struct ActiveGuard<'a> {
    turns: &'a ActiveTurns,
    session_id: String,
    turn_id: String,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        let mut m = self.turns.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(&self.session_id)
            .is_some_and(|t| t.turn_id == self.turn_id)
        {
            m.remove(&self.session_id);
        }
    }
}

impl ActiveTurns {
    pub fn new() -> ActiveTurns {
        ActiveTurns::default()
    }

    pub fn enter(&self, turn: &Arc<TurnCtx>) -> ActiveGuard<'_> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(turn.session_id.clone(), turn.clone());
        ActiveGuard {
            turns: self,
            session_id: turn.session_id.clone(),
            turn_id: turn.turn_id.clone(),
        }
    }

    /// The running turn of `session_id`.
    pub fn get(&self, session_id: &str) -> Option<Arc<TurnCtx>> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }
}
