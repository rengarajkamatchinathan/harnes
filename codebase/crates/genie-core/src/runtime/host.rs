//! Engine implementations of the shell and web tool ports (06 §8, §13): session start
//! directories, background jobs (store rows + the synthetic `background_done` message),
//! and the per-turn taint (SA-29).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use genie_protocol::session::Delivery;
use genie_tools::shell::{BackgroundJobs, JobFinished, JobStarted, SessionDirs};
use genie_tools::web::TaintPort;

use crate::engine::{Engine, WorkGuard};
use crate::runtime::ports::SettingsSource;
use crate::runtime::prompts::{self, fragments};

/// One turn's taint: the source label once untrusted content was seen.
pub type TaintCell = Arc<Mutex<Option<String>>>;

/// The taint cell of each running turn, keyed by session (06 §5.5, SA-29).
#[derive(Default)]
pub struct TurnTaints {
    map: Mutex<HashMap<String, (String, TaintCell)>>,
}

impl TurnTaints {
    /// A fresh cell for this turn; removed again by [`TurnTaints::leave`].
    pub fn enter(&self, session_id: &str, turn_id: &str) -> TaintCell {
        let cell = Arc::new(Mutex::new(None));
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), (turn_id.to_string(), cell.clone()));
        cell
    }

    pub fn leave(&self, session_id: &str, turn_id: &str) {
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if g.get(session_id).is_some_and(|(t, _)| t == turn_id) {
            g.remove(session_id);
        }
    }
}

impl TaintPort for TurnTaints {
    fn tainted(&self, session_id: &str, turn_id: &str, source: &str) {
        let g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, cell)) = g.get(session_id)
            && t == turn_id
        {
            let mut c = cell.lock().unwrap_or_else(|e| e.into_inner());
            if c.is_none() {
                *c = Some(source.to_string());
            }
        }
    }
}

/// A session's first shell call starts in its workspace (or worktree) root.
pub struct SettingsDirs(pub Arc<dyn SettingsSource>);

impl SessionDirs for SettingsDirs {
    fn initial_cwd(&self, session_id: &str) -> Option<PathBuf> {
        self.0.settings(session_id)?.workspace
    }
}

/// Background shell jobs (06 §13.1): `background_job` rows in the store, the engine's
/// keep-alive counter while they run, and a synthetic `background_done` inbox message when
/// they end. [`EngineJobs::bind`] gives it the engine after it is built.
pub struct EngineJobs {
    engine: OnceLock<Engine>,
    store: Option<Arc<genie_store::Store>>,
    guards: Mutex<HashMap<String, WorkGuard>>,
}

fn ms(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl EngineJobs {
    pub fn new(store: Option<Arc<genie_store::Store>>) -> Arc<EngineJobs> {
        Arc::new(EngineJobs {
            engine: OnceLock::new(),
            store,
            guards: Mutex::new(HashMap::new()),
        })
    }

    pub fn bind(&self, engine: Engine) {
        let _ = self.engine.set(engine);
    }
}

/// Lines of a finished job's output quoted in its notice; the rest is behind the ref.
const DONE_TAIL_LINES: usize = 20;
const DONE_LINE_CHARS: usize = 300;

/// The `background_done` notice (ticket 44): what ran, how it ended, the last lines of
/// its output and a ref to the full output. Never the whole log: a dev server's output
/// can run to hundreds of lines, and the notice is shown in the chat.
pub(crate) fn background_done_text(job: &JobFinished) -> String {
    let status = match (job.killed, job.exit_code) {
        (true, _) => "stopped by Genie".to_string(),
        (false, Some(c)) => format!("exit code {c}"),
        (false, None) => "terminated by a signal".to_string(),
    };
    let clip = |s: &str, n: usize| match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    };
    let command = clip(job.command.lines().next().unwrap_or(""), 200);
    let lines: Vec<&str> = job
        .output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(DONE_TAIL_LINES)..]
        .iter()
        .map(|l| clip(l, DONE_LINE_CHARS))
        .collect::<Vec<_>>()
        .join("\n");
    let mut vars: Vec<(&str, &str)> =
        vec![("id", &job.id), ("command", &command), ("status", &status)];
    let mut flags = vec![];
    if let Some(r) = job.output_ref.as_deref() {
        vars.push(("ref", r));
        flags.push("ref");
    }
    if !tail.is_empty() {
        vars.push(("tail", &tail));
        flags.push("tail");
    }
    prompts::render(fragments::BACKGROUND_DONE.trim_end(), &vars, &flags)
}

impl BackgroundJobs for EngineJobs {
    fn started(&self, job: &JobStarted) {
        if let Some(e) = self.engine.get() {
            self.guards
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(job.id.clone(), e.background_job());
        }
        if let Some(s) = &self.store {
            let r = s.write(|tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO background_job (id, session_id, kind, command, cwd, started) \
                     VALUES (?1, ?2, 'shell', ?3, ?4, ?5)",
                    (
                        &job.id,
                        &job.session_id,
                        &job.command,
                        job.cwd.display().to_string(),
                        ms(job.started),
                    ),
                )?;
                Ok(())
            });
            if let Err(e) = r {
                tracing::warn!(error = %e, job = %job.id, "background_job row not written");
            }
        }
    }

    fn finished(&self, job: JobFinished) {
        self.guards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&job.id);
        if let Some(s) = &self.store {
            let r = s.write(|tx| {
                tx.execute(
                    "UPDATE background_job SET ended = ?2, exit_code = ?3 WHERE id = ?1",
                    (&job.id, ms(job.ended), job.exit_code),
                )?;
                Ok(())
            });
            if let Err(e) = r {
                tracing::warn!(error = %e, job = %job.id, "background_job row not updated");
            }
        }
        let text = background_done_text(&job);
        let Some(engine) = self.engine.get() else {
            return;
        };
        match engine.session(&job.session_id) {
            Ok(h) => {
                if let Err(e) = h.push_synthetic("background_done", text, Delivery::Queue) {
                    tracing::warn!(error = %e, job = %job.id, "background_done not queued");
                }
            }
            Err(d) => tracing::warn!(incident_id = %d.incident_id, "background_done: no session"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(
        output: String,
        exit_code: Option<i32>,
        killed: bool,
        output_ref: Option<&str>,
    ) -> JobFinished {
        JobFinished {
            id: "bg_1".into(),
            session_id: "ses".into(),
            command: "cd app && bun run dev:app\necho second line".into(),
            exit_code,
            killed,
            output,
            output_ref: output_ref.map(str::to_string),
            ended: std::time::SystemTime::now(),
        }
    }

    #[test]
    fn background_done_quotes_only_the_last_lines() {
        let output: String = (1..=300).map(|i| format!("log line {i}\n")).collect();
        let text = background_done_text(&job(output, Some(0), false, Some("blob_x")));
        assert!(text.starts_with(
            "<system-update>Background command bg_1 (`cd app && bun run dev:app`) finished: \
             exit code 0. Full output: blob_x"
        ));
        assert!(text.contains("log line 281\n"), "{text}");
        assert!(text.contains("log line 300</system-update>"), "{text}");
        assert!(!text.contains("log line 280\n"), "{text}");
    }

    #[test]
    fn background_done_without_output_or_ref() {
        let text = background_done_text(&job(String::new(), None, true, None));
        assert_eq!(
            text,
            "<system-update>Background command bg_1 (`cd app && bun run dev:app`) finished: \
             stopped by Genie.</system-update>"
        );
    }
}
