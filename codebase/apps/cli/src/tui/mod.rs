//! Inline TUI (07 §1): [`model`] (state + update), [`view`] (footer rendering),
//! [`markdown`] (streaming blocks), [`theme`], [`commands`] (one registry for slash
//! commands and keys) and [`run`] (the terminal driver).

pub mod commands;
pub mod markdown;
pub mod model;
pub mod run;
pub mod theme;
pub mod view;
