//! The live footer (07 §1.1, §1.3): streaming tail, queue, permission prompt or form,
//! slash / file menu, composer and status line. Pure: `App` → lines + cursor.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::markdown;
use super::model::{ASK_OPTIONS, App, Menu, mode_label};
use super::theme::Role;
use crate::render;

/// Most rows the streaming tail may take in the footer.
pub const MAX_LIVE_ROWS: usize = 12;
const MENU_ROWS: usize = 8;

pub struct Footer {
    pub lines: Vec<Line<'static>>,
    /// Cursor (column, row) within the footer, when the composer has focus.
    pub cursor: Option<(u16, u16)>,
}

impl Footer {
    pub fn height(&self) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }
}

pub fn footer(app: &App) -> Footer {
    let th = &app.theme;
    let w = app.width;
    let mut lines: Vec<Line<'static>> = vec![];

    // Streaming tail (only the last rows).
    let live = markdown::wrap(app.live_lines(), w);
    let skip = live.len().saturating_sub(MAX_LIVE_ROWS);
    lines.extend(live.into_iter().skip(skip));

    for item in &app.inbox {
        lines.push(Line::from(Span::styled(
            format!(
                "  ⧗ {} {}",
                match item.delivery {
                    genie_protocol::session::Delivery::Queue => "queued:",
                    genie_protocol::session::Delivery::Steer => "steer:",
                },
                render::truncate(&item.preview, usize::from(w).saturating_sub(14))
            ),
            th.style(Role::Muted),
        )));
    }

    let mut cursor = None;
    if let Some(a) = app.asks.front() {
        let label = a
            .ask
            .subagent
            .as_ref()
            .map(|s| format!(" (subagent {})", s.agent))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!("◆ Approval needed{label}"),
            th.style(Role::Warning),
        )));
        lines.push(Line::from(format!("  {}", render::ask_summary(&a.ask))));
        if let Some(genie_protocol::ask::Preview::Diff { unified, .. }) = &a.ask.preview {
            let diff = markdown::render(&format!("```diff\n{unified}\n```"), th);
            let n = diff.len();
            lines.extend(diff.into_iter().take(10));
            if n > 10 {
                lines.push(Line::from(Span::styled(
                    format!("  … {} more lines", n - 10),
                    th.style(Role::Muted),
                )));
            }
        }
        if let Some(form) = &a.form {
            for (i, ((name, sensitive), value)) in form.fields.iter().zip(&form.values).enumerate()
            {
                let shown = if *sensitive {
                    "•".repeat(value.chars().count())
                } else {
                    value.clone()
                };
                let marker = if i == form.index { "›" } else { " " };
                let text = format!("  {marker} {name}: {shown}");
                if i == form.index {
                    cursor = Some((width(&text), lines.len()));
                }
                lines.push(Line::from(text));
            }
            lines.push(Line::from(Span::styled(
                "  Enter next/submit · Esc cancel",
                th.style(Role::Muted),
            )));
        } else if let Some(reason) = &a.reason {
            let text = format!("  Deny — reason (optional): {reason}");
            cursor = Some((width(&text), lines.len()));
            lines.push(Line::from(text));
            lines.push(Line::from(Span::styled(
                "  Enter deny · Esc deny without reason",
                th.style(Role::Muted),
            )));
        } else {
            let mut spans = vec![Span::raw("  ")];
            for (i, opt) in ASK_OPTIONS.iter().enumerate() {
                let text = match i {
                    1 => match render::always_label(&a.ask) {
                        Some(l) => format!("[{}] {opt} ({l})", i + 1),
                        None => format!("[{}] {opt}", i + 1),
                    },
                    _ => format!("[{}] {opt}", i + 1),
                };
                let st = if i == a.selected {
                    th.style(Role::Selected)
                } else {
                    th.style(Role::Text)
                };
                spans.push(Span::styled(text, st));
                spans.push(Span::raw("  "));
            }
            lines.push(Line::from(spans));
        }
        if app.asks.len() > 1 {
            lines.push(Line::from(Span::styled(
                format!("  +{} more waiting", app.asks.len() - 1),
                th.style(Role::Muted),
            )));
        }
    } else {
        // Composer.
        if !app.attachments.is_empty() {
            let names: Vec<String> = app
                .attachments
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect();
            lines.push(Line::from(Span::styled(
                format!("  📎 {}", names.join(", ")),
                th.style(Role::Accent),
            )));
        }
        let prompt_style = if app.read_only {
            th.style(Role::Muted)
        } else {
            th.style(Role::Accent)
        };
        let before = &app.input[..app.cursor.min(app.input.len())];
        let cur_row = before.matches('\n').count();
        let cur_col = width(before.rsplit('\n').next().unwrap_or_default());
        let base = lines.len();
        let input_lines: Vec<&str> = if app.input.is_empty() {
            vec![""]
        } else {
            app.input.split('\n').collect()
        };
        for (i, l) in input_lines.iter().enumerate() {
            let prefix = if i == 0 { "› " } else { "  " };
            if app.input.is_empty() {
                let hint = if app.read_only {
                    "read-only · Ctrl-T to take over"
                } else {
                    "Type a message · / for commands · @ for files"
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix.to_string(), prompt_style),
                    Span::styled(hint.to_string(), th.style(Role::Muted)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(prefix.to_string(), prompt_style),
                    Span::raw((*l).to_string()),
                ]));
            }
        }
        cursor = Some((2 + cur_col, base + cur_row));

        match &app.menu {
            Some(Menu::Slash { items, selected }) => {
                for (i, it) in items.iter().enumerate().take(MENU_ROWS) {
                    let st = if i == *selected {
                        th.style(Role::Selected)
                    } else {
                        th.style(Role::Text)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  /{:<14}", it.name), st),
                        Span::styled(format!(" {}", it.hint), th.style(Role::Muted)),
                    ]));
                }
            }
            Some(Menu::Files {
                items, selected, ..
            }) => {
                for (i, it) in items.iter().enumerate().take(MENU_ROWS) {
                    let st = if i == *selected {
                        th.style(Role::Selected)
                    } else {
                        th.style(Role::Text)
                    };
                    lines.push(Line::from(Span::styled(format!("  @{it}"), st)));
                }
            }
            None => {}
        }
    }

    lines.push(status_line(app));
    let cursor = cursor.map(|(c, r)| {
        (
            u16::try_from(c).unwrap_or(0).min(w.saturating_sub(1)),
            u16::try_from(r).unwrap_or(0),
        )
    });
    Footer { lines, cursor }
}

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// model · effort · mode · context % · cost · subagents · waiting for approval.
pub fn status_line(app: &App) -> Line<'static> {
    let th = &app.theme;
    let mut parts: Vec<String> = vec![];
    parts.push(if app.model.is_empty() {
        "…".into()
    } else {
        app.model.clone()
    });
    if let Some(e) = &app.effort {
        parts.push(e.clone());
    }
    parts.push(mode_label(app.mode).to_string());
    if let Some(p) = app.context_pct {
        parts.push(format!("ctx {p}%"));
    }
    parts.push(format!("${:.2}", app.cost_usd));
    if !app.subagents.is_empty() {
        let n = app.subagents.len();
        parts.push(format!(
            "{n} subagent{} running",
            if n == 1 { "" } else { "s" }
        ));
    }
    let mut spans = vec![Span::styled(
        format!("  {}", parts.join(" · ")),
        th.style(Role::Muted),
    )];
    if !app.asks.is_empty() {
        spans.push(Span::styled(
            " · Waiting for approval".to_string(),
            th.style(Role::Warning),
        ));
    } else if app.running() {
        spans.push(Span::styled(
            " · working (Esc to interrupt)".to_string(),
            th.style(Role::Accent),
        ));
    }
    if app.reconnecting {
        spans.push(Span::styled(
            " · Reconnecting…".to_string(),
            th.style(Role::Warning),
        ));
    }
    if let Some(n) = &app.notice {
        spans.push(Span::styled(format!(" · {n}"), th.style(Role::Accent)));
    }
    Line::from(spans)
}

/// Draw the footer into the inline viewport.
pub fn draw(frame: &mut Frame<'_>, f: &Footer) {
    let area: Rect = frame.area();
    frame.render_widget(Paragraph::new(f.lines.clone()), area);
    if let Some((c, r)) = f.cursor
        && r < area.height
    {
        frame.set_cursor_position(Position::new(area.x + c, area.y + r));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::tui::model::Msg;
    use crate::tui::theme::Theme;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use genie_protocol::Id;
    use genie_protocol::ask::{Ask, AskKind, Preview};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn text(f: &Footer) -> Vec<String> {
        f.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn composer_and_status() {
        let mut a = App::new(Theme::default(), "me");
        a.model = "claude-x".into();
        a.effort = Some("high".into());
        a.cost_usd = 0.125;
        a.context_pct = Some(42);
        a.subagents.insert("c1".into());
        a.subagents.insert("c2".into());
        for c in "hi".chars() {
            a.update(Msg::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        let f = footer(&a);
        let t = text(&f);
        assert_eq!(t[0], "› hi");
        assert_eq!(
            t.last().unwrap(),
            "  claude-x · high · default · ctx 42% · $0.12 · 2 subagents running"
        );
        assert_eq!(f.cursor, Some((4, 0)));
    }

    #[test]
    fn permission_prompt_in_footer() {
        let mut a = App::new(Theme::default(), "me");
        let mut ask = Ask::new("ask_1", "s", AskKind::Permission, "");
        ask.tool = Some("shell".into());
        ask.preview = Some(Preview::Command {
            command: "cargo test".into(),
            cwd: None,
        });
        ask.subagent = Some(genie_protocol::ask::SubagentRef {
            session_id: "c".into(),
            agent: "tester".into(),
        });
        a.update(Msg::AskRequest {
            id: Id::Num(1),
            ask: Box::new(ask),
        });
        let t = text(&footer(&a)).join("\n");
        assert!(t.contains("Approval needed (subagent tester)"), "{t}");
        assert!(t.contains("shell: cargo test"));
        assert!(t.contains("[1] Allow once"));
        assert!(t.contains("[3] Deny"));
        assert!(t.contains("Waiting for approval"));
    }

    #[test]
    fn draws_into_a_test_backend() {
        let mut a = App::new(Theme::default(), "me");
        a.width = 40;
        let f = footer(&a);
        let mut term = Terminal::new(TestBackend::new(40, f.height())).unwrap();
        term.draw(|fr| draw(fr, &f)).unwrap();
        let buf = term.backend().buffer().clone();
        let row: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(row.starts_with("› Type a message"));
    }
}
