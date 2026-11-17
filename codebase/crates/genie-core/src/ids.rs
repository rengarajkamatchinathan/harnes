//! ULID ids with a type prefix (02 §6): `ses_`, `trn_`, `ask_`, `inb_`, `inc_` …

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", ulid::Ulid::new().to_string().to_lowercase())
}

pub type SessionId = String;
pub type TurnId = String;
pub type AskId = String;
pub type InboxItemId = String;
