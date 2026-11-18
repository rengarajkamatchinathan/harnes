//! The method table (02 §6, SA-08): every request, daemon → client request and
//! notification, with its params and result types.
//!
//! Each method is a zero-sized type implementing [`Method`], so Rust callers get typed
//! params/results (`conn.call::<SessionPrompt>(params)`), and `cargo xtask schema`
//! walks [`table`] to emit `methods.json` and the TypeScript client (02 §11).

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api::{browser, code, config, daemon, mcp, project, provider, session};
use crate::ask::AskAnswer;
use crate::common::{Empty, OkResult};
use crate::event::{CancelRequest, LeaseLost, ResyncRequired, SessionEventEnvelope, SessionSynced};
use crate::handshake::{InitializeParams, InitializeResult};
use crate::workspace;

pub use crate::api::daemon::{
    ConnectedClient, DaemonRestartParams, DaemonRestartingParams, DaemonStatus,
};

/// Who sends a method and whether it expects a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Client → daemon request.
    Request,
    /// Daemon → client request (LSP/MCP style), e.g. `permission.ask`.
    ServerRequest,
    /// Daemon → client notification.
    Notification,
    /// Client → daemon notification.
    ClientNotification,
}

pub trait Method {
    const NAME: &'static str;
    const DIRECTION: Direction;
    /// Needs the session's driver lease (02 §5). `session.update` is checked per field.
    const NEEDS_LEASE: bool;
    type Params: Serialize + DeserializeOwned + JsonSchema;
    type Result: Serialize + DeserializeOwned + JsonSchema;
}

/// One row of `methods.json`.
pub struct MethodInfo {
    pub name: &'static str,
    pub ident: &'static str,
    pub direction: Direction,
    pub needs_lease: bool,
    pub params: fn(&mut SchemaGenerator) -> Schema,
    pub result: fn(&mut SchemaGenerator) -> Schema,
}

impl MethodInfo {
    fn of<M: Method>(ident: &'static str) -> MethodInfo {
        MethodInfo {
            name: M::NAME,
            ident,
            direction: M::DIRECTION,
            needs_lease: M::NEEDS_LEASE,
            params: |g| g.subschema_for::<M::Params>(),
            result: |g| g.subschema_for::<M::Result>(),
        }
    }
}

macro_rules! methods {
    ($(
        $(#[$doc:meta])*
        $ident:ident = $name:literal, $dir:ident, $lease:literal, $params:ty => $result:ty;
    )*) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy)]
            pub struct $ident;
            impl Method for $ident {
                const NAME: &'static str = $name;
                const DIRECTION: Direction = Direction::$dir;
                const NEEDS_LEASE: bool = $lease;
                type Params = $params;
                type Result = $result;
            }
        )*

        /// Every method, in spec order.
        pub fn table() -> Vec<MethodInfo> {
            vec![$(MethodInfo::of::<$ident>(stringify!($ident))),*]
        }
    };
}

methods! {
    // ---- handshake (§2)
    /// Must be the first request on a connection.
    Initialize = "initialize", Request, false, InitializeParams => InitializeResult;
    /// Liveness probe.
    Ping = "$/ping", Request, false, Empty => Empty;

    // ---- daemon and secrets (§6.1)
    DaemonStatusMethod = "daemon.status", Request, false, Empty => DaemonStatus;
    DaemonRestart = "daemon.restart", Request, false, DaemonRestartParams => Empty;
    SecretsUnlock = "secrets.unlock", Request, false, daemon::UnlockParams => OkResult;
    DoctorRun = "doctor.run", Request, false, Empty => daemon::DoctorResult;
    DiagnosticsCollect = "diagnostics.collect", Request, false, daemon::DiagnosticsParams => daemon::DiagnosticsResult;

    // ---- subscriptions (§3)
    SessionSubscribe = "session.subscribe", Request, false, session::SubscribeParams => session::SubscribeResult;
    SessionUnsubscribe = "session.unsubscribe", Request, false, session::SessionIdParams => Empty;
    WorkspaceSubscribe = "workspace.subscribe", Request, false, Empty => Empty;

    // ---- sessions (§6.2)
    SessionList = "session.list", Request, false, session::ListParams => session::ListResult;
    SessionGet = "session.get", Request, false, session::GetParams => session::GetResult;
    SessionCreate = "session.create", Request, false, session::CreateParams => session::CreateResult;
    /// Lease needed only for model/effort/mode/active_leaf_id.
    SessionUpdate = "session.update", Request, false, session::UpdateParams => Empty;
    SessionDelete = "session.delete", Request, true, session::SessionIdParams => Empty;
    SessionPrompt = "session.prompt", Request, true, session::PromptParams => session::PromptResult;
    SessionRegenerate = "session.regenerate", Request, true, session::RegenerateParams => Empty;
    SessionInterrupt = "session.interrupt", Request, true, session::InterruptParams => session::InterruptResult;
    /// Move a running tool call (a `shell` command) to the background (ticket 43).
    SessionBackgroundTool = "session.background_tool", Request, true, session::BackgroundToolParams => session::BackgroundToolResult;
    SessionResume = "session.resume", Request, true, session::SessionIdParams => Empty;
    SessionTakeOver = "session.take_over", Request, false, session::SessionIdParams => Empty;
    SessionRelease = "session.release", Request, false, session::SessionIdParams => Empty;
    SessionFork = "session.fork", Request, false, session::ForkParams => session::CreateResult;
    SessionCompact = "session.compact", Request, true, session::CompactParams => Empty;
    SessionInboxRemove = "session.inbox.remove", Request, true, session::InboxRemoveParams => Empty;
    SessionExport = "session.export", Request, false, session::ExportParams => session::ExportResult;
    SessionSearch = "session.search", Request, false, session::SearchParams => session::SearchResult;
    /// The tools a session's model sees (CLI `system/init`).
    ToolsList = "tools.list", Request, false, session::SessionIdParams => session::ToolsListResult;
    /// Import one session JSON (04 §9); ids that already exist are replaced.
    SessionImport = "session.import", Request, false, session::SessionImportParams => session::SessionImportResult;
    /// Selectable agent definitions for a folder (06 §11): built-in, user, trusted project.
    AgentsList = "agents.list", Request, false, config::CwdParams => session::AgentListResult;

    // ---- asks (§6.3, §7)
    PermissionPending = "permission.pending", Request, false, session::PendingParams => session::PendingResult;
    /// Not lease-gated: any authorised client may answer (02 §5).
    PermissionRespond = "permission.respond", Request, false, session::RespondParams => Empty;
    PermissionAsk = "permission.ask", ServerRequest, false, session::AskParams => AskAnswer;

    // ---- config, policy, trust (§6.4)
    ConfigGet = "config.get", Request, false, config::ConfigGetParams => config::ConfigGetResult;
    ConfigSet = "config.set", Request, false, config::ConfigSetParams => Empty;
    ConfigSchema = "config.schema", Request, false, Empty => config::ConfigSchemaResult;
    ConfigDiagnostics = "config.diagnostics", Request, false, config::CwdParams => config::ConfigDiagnosticsResult;
    PolicyEffective = "policy.effective", Request, false, Empty => config::PolicyEffectiveResult;
    TrustGet = "trust.get", Request, false, config::PathParams => config::TrustGetResult;
    TrustSet = "trust.set", Request, false, config::TrustSetParams => Empty;
    TrustPendingDefinitions = "trust.pending_definitions", Request, false, config::PathParams => config::PendingDefinitionsResult;
    TrustApproveDefinition = "trust.approve_definition", Request, false, config::ApproveDefinitionParams => Empty;
    /// Stored folder decisions (Settings → Privacy); revoke with `trust.set {decision: revoke}`.
    TrustList = "trust.list", Request, false, Empty => config::TrustListResult;
    /// Saved permission rules per scope (Settings → Permissions, 06 §5.4).
    PermissionRulesList = "permissions.rules.list", Request, false, config::PermissionRulesListParams => config::PermissionRulesListResult;
    PermissionRulesRemove = "permissions.rules.remove", Request, false, config::PermissionRuleRemoveParams => Empty;
    /// "Delete all data" step 1 (04 §8): what will go, and a short-lived confirmation token.
    DataDeleteAllBegin = "data.delete_all.begin", Request, false, Empty => config::DeleteAllBeginResult;
    /// Step 2: wipe sessions, projects, blobs, memory, artifacts, snapshots, caches (+ local audit).
    DataDeleteAllConfirm = "data.delete_all.confirm", Request, false, config::DeleteAllParams => config::DeleteAllResult;

    // ---- providers, catalog, usage (§6.5)
    ProviderList = "provider.list", Request, false, Empty => provider::ProviderListResult;
    ProviderCredentialAdd = "provider.credential.add", Request, false, provider::CredentialAddParams => provider::CredentialAddResult;
    ProviderCredentialRemove = "provider.credential.remove", Request, false, provider::CredentialIdParams => Empty;
    ProviderCredentialActivate = "provider.credential.activate", Request, false, provider::CredentialIdParams => Empty;
    ProviderLoginStart = "provider.login.start", Request, false, provider::LoginStartParams => provider::LoginStartResult;
    ProviderLoginStatus = "provider.login.status", Request, false, provider::LoginStatusParams => provider::LoginStatusResult;
    ProviderTest = "provider.test", Request, false, provider::ProviderParams => provider::ProviderTestResult;
    ModelList = "model.list", Request, false, provider::ModelListParams => provider::ModelListResult;
    CatalogStatusMethod = "catalog.status", Request, false, Empty => provider::CatalogStatus;
    CatalogRefresh = "catalog.refresh", Request, false, Empty => provider::CatalogStatus;
    UsageQuery = "usage.query", Request, false, provider::UsageQueryParams => provider::UsageQueryResult;
    UsageBudgetSet = "usage.budget.set", Request, false, provider::BudgetSetParams => Empty;

    // ---- browser (§6.6)
    BrowserStatusMethod = "browser.status", Request, false, Empty => browser::BrowserStatus;
    BrowserScreencastStart = "browser.screencast.start", Request, false, browser::ScreencastStartParams => browser::StreamIdResult;
    BrowserScreencastAck = "browser.screencast.ack", Request, false, browser::ScreencastAckParams => Empty;
    BrowserScreencastStop = "browser.screencast.stop", Request, false, browser::StreamIdParams => Empty;
    BrowserTakeover = "browser.takeover", Request, true, session::SessionIdParams => Empty;
    BrowserHandback = "browser.handback", Request, true, session::SessionIdParams => Empty;
    BrowserStop = "browser.stop", Request, true, session::SessionIdParams => Empty;
    BrowserInput = "browser.input", Request, true, browser::BrowserInputParams => Empty;
    BrowserTabs = "browser.tabs", Request, false, session::SessionIdParams => browser::TabsResult;
    BrowserTabActivate = "browser.tab.activate", Request, true, browser::TabActivateParams => Empty;
    BrowserPopout = "browser.popout", Request, false, session::SessionIdParams => Empty;
    BrowserSitesList = "browser.sites.list", Request, false, Empty => browser::SitesListResult;
    BrowserSitesSet = "browser.sites.set", Request, false, browser::SitePermission => Empty;
    BrowserDataClear = "browser.data.clear", Request, false, browser::DataClearParams => Empty;

    // ---- terminal (§6.7)
    TerminalOpen = "terminal.open", Request, false, browser::TerminalOpenParams => browser::TerminalIdParams;
    TerminalAttach = "terminal.attach", Request, false, browser::TerminalAttachParams => browser::TerminalAttachResult;
    TerminalInput = "terminal.input", Request, false, browser::TerminalInputParams => Empty;
    TerminalResize = "terminal.resize", Request, false, browser::TerminalResizeParams => Empty;
    TerminalClose = "terminal.close", Request, false, browser::TerminalIdParams => Empty;

    // ---- MCP, skills, directories (§6.8)
    McpList = "mcp.list", Request, false, config::CwdParams => mcp::McpListResult;
    McpAdd = "mcp.add", Request, false, mcp::McpAddParams => Empty;
    McpRemove = "mcp.remove", Request, false, mcp::McpRemoveParams => Empty;
    McpSetEnabled = "mcp.set_enabled", Request, false, mcp::McpSetEnabledParams => Empty;
    McpAuthStart = "mcp.auth.start", Request, false, mcp::NameParams => mcp::AuthUrlResult;
    McpLogs = "mcp.logs", Request, false, mcp::McpLogsParams => mcp::LinesResult;
    McpTools = "mcp.tools", Request, false, mcp::NameParams => mcp::McpItemsResult;
    McpResources = "mcp.resources", Request, false, mcp::NameParams => mcp::McpItemsResult;
    McpPrompts = "mcp.prompts", Request, false, mcp::NameParams => mcp::McpItemsResult;
    McpResourceRead = "mcp.resource.read", Request, false, mcp::McpResourceReadParams => mcp::BlobMimeResult;
    SkillList = "skill.list", Request, false, Empty => mcp::SkillListResult;
    SkillSetEnabled = "skill.set_enabled", Request, false, mcp::SkillSetEnabledParams => Empty;
    SkillUpload = "skill.upload", Request, false, mcp::BlobIdParams => mcp::IdResult;
    DirectoryList = "directory.list", Request, false, mcp::DirectoryListParams => mcp::DirectoryListResult;
    DirectoryInstall = "directory.install", Request, false, mcp::DirectoryInstallParams => mcp::DirectoryInstallResult;

    // ---- projects, artifacts, memory, blobs (§6.9)
    ProjectList = "project.list", Request, false, Empty => project::ProjectListResult;
    ProjectCreate = "project.create", Request, false, project::ProjectCreateParams => project::ProjectIdResult;
    ProjectUpdate = "project.update", Request, false, project::ProjectUpdateParams => Empty;
    ProjectDelete = "project.delete", Request, false, project::ProjectIdParams => Empty;
    ProjectFileAdd = "project.file.add", Request, false, project::ProjectFileAddParams => project::FileIdResult;
    ProjectFileRemove = "project.file.remove", Request, false, project::ProjectFileRemoveParams => Empty;
    ProjectFileList = "project.file.list", Request, false, project::ProjectIdParams => project::ProjectFileListResult;
    ProjectIndexStatus = "project.index.status", Request, false, project::ProjectIdParams => project::IndexStatus;
    ArtifactList = "artifact.list", Request, false, project::ArtifactListParams => project::ArtifactListResult;
    ArtifactGet = "artifact.get", Request, false, project::ArtifactGetParams => project::ArtifactGetResult;
    MemoryList = "memory.list", Request, false, project::MemoryScopeParams => project::MemoryListResult;
    MemoryUpdate = "memory.update", Request, false, project::MemoryUpdateParams => Empty;
    MemoryDelete = "memory.delete", Request, false, project::MemoryIdParams => Empty;
    MemorySetState = "memory.set_state", Request, false, project::MemorySetStateParams => Empty;
    MemoryReset = "memory.reset", Request, false, project::MemoryScopeParams => Empty;
    BlobPutBegin = "blob.put.begin", Request, false, project::BlobPutBeginParams => project::UploadIdResult;
    BlobPutChunk = "blob.put.chunk", Request, false, project::BlobPutChunkParams => Empty;
    BlobPutEnd = "blob.put.end", Request, false, project::UploadIdParams => project::BlobPutResult;
    BlobPutPath = "blob.put.path", Request, false, project::BlobPutPathParams => project::BlobPutResult;
    BlobGet = "blob.get", Request, false, project::BlobGetParams => project::BlobGetResult;

    // ---- Code mode (§6.10)
    CheckpointList = "checkpoint.list", Request, false, session::SessionIdParams => code::CheckpointListResult;
    CheckpointPreviewMethod = "checkpoint.preview", Request, false, code::CheckpointParams => code::CheckpointPreview;
    CheckpointRewind = "checkpoint.rewind", Request, true, code::CheckpointParams => Empty;
    CheckpointUndo = "checkpoint.undo", Request, true, session::SessionIdParams => Empty;
    GitStatus = "git.status", Request, false, code::SessionPathParams => code::GitStatusResult;
    GitDiff = "git.diff", Request, false, code::SessionPathParams => code::TextResult;
    GitCommit = "git.commit", Request, true, code::GitCommitParams => code::GitCommitResult;
    WorktreeFinish = "worktree.finish", Request, true, code::WorktreeFinishParams => code::WorktreeFinishResult;
    FilesTree = "files.tree", Request, false, code::SessionPathParams => code::FilesTreeResult;
    FilesRead = "files.read", Request, false, code::SessionPathParams => code::FilesReadResult;
    ImportScan = "import.scan", Request, false, code::ImportParams => code::ImportResult;
    ImportPreview = "import.preview", Request, false, code::ImportParams => code::ImportResult;
    ImportApply = "import.apply", Request, false, code::ImportParams => code::ImportResult;
    ExportAll = "export.all", Request, false, code::ExportAllParams => code::ExportAllResult;

    // ---- daemon → client notifications
    /// A session event (02 §4).
    Event = "event", Notification, false, SessionEventEnvelope => ();
    SessionSyncedNote = "session.synced", Notification, false, SessionSynced => ();
    SessionResyncRequired = "session.resync_required", Notification, false, ResyncRequired => ();
    LeaseLostNote = "lease.lost", Notification, false, LeaseLost => ();
    CancelRequestNote = "$/cancelRequest", Notification, false, CancelRequest => ();
    DaemonRestarting = "daemon.restarting", Notification, false, DaemonRestartingParams => ();
    WorkspaceSnapshot = "workspace.snapshot", Notification, false, workspace::WorkspaceSnapshot => ();
    SessionListed = "session.listed", Notification, false, workspace::SessionListed => ();
    ConfigChanged = "config.changed", Notification, false, workspace::ConfigChanged => ();
    PolicyChanged = "policy.changed", Notification, false, workspace::PolicyChanged => ();
    McpStatusNote = "mcp.status", Notification, false, workspace::McpStatusChanged => ();
    CatalogUpdated = "catalog.updated", Notification, false, workspace::CatalogUpdated => ();
    ProviderStatusNote = "provider.status", Notification, false, workspace::ProviderStatusChanged => ();
    UserNotification = "notification", Notification, false, workspace::UserNotification => ();
    TerminalExited = "terminal.exited", Notification, false, workspace::TerminalExited => ();
    /// Memory changed outside a turn (`memory.update/delete/reset`, Settings → Memory).
    MemoryChangedNote = "memory.changed", Notification, false, crate::event::MemoryChanged => ();
}

// Names the architecture layer (spec 01) already used.
pub const INITIALIZE: &str = Initialize::NAME;
pub const DAEMON_STATUS: &str = DaemonStatusMethod::NAME;
pub const DAEMON_RESTART: &str = DaemonRestart::NAME;
pub const PING: &str = Ping::NAME;
pub const DAEMON_RESTARTING: &str = DaemonRestarting::NAME;
pub const EVENT: &str = Event::NAME;

/// Look up a method by wire name.
pub fn find(name: &str) -> Option<MethodInfo> {
    table().into_iter().find(|m| m.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique_and_well_formed() {
        let t = table();
        let mut seen = HashSet::new();
        for m in &t {
            assert!(seen.insert(m.name), "duplicate method {}", m.name);
            assert!(
                m.name.starts_with("$/")
                    || m.name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_'),
                "bad name {}",
                m.name
            );
        }
        assert!(t.len() > 100);
        assert!(find("session.prompt").is_some_and(|m| m.needs_lease));
        assert!(find("permission.respond").is_some_and(|m| !m.needs_lease));
    }
}
