//! `genie-skills`: skills scanner/index, slash commands, skill packages and the signed
//! skills directory (spec/06-agent-runtime.md §9, D37, D44).
//!
//! - [`scan`]: discovery in built-in / `~/.genie/skills` / trusted project roots, fingerprint
//!   for hot reload;
//! - [`manifest`]: `SKILL.md` frontmatter schema and validation;
//! - [`catalog`]: [`SkillCatalog`] — policy, enable/disable state, `skill.list`, the
//!   `<available_skills>` index, `load` (body on use) and `read_file`;
//! - [`package`]: `.zip`/`.skill` import with traversal, size and zip-bomb guards;
//! - [`directory`]: signed skills directory (`directory.list/install`);
//! - [`command`]: Markdown slash commands (`$ARGUMENTS`, `$1..$N`, `@file`, `` !`cmd` ``);
//! - [`tools`]: data and bodies for the engine's `skill` / `skill_read` tools.
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol` only. The
//! `Tool` impls, file watching and permission checks are wired by `genie-core`.

pub mod catalog;
pub mod command;
pub mod directory;
pub mod error;
pub mod frontmatter;
pub mod manifest;
pub mod package;
pub mod scan;
pub mod tools;

pub use catalog::{
    Invoker, JsonStateFile, LoadedSkill, MAX_READ, MemoryState, SessionKind, SkillCatalog,
    SkillFile, SkillPolicy, SkillStateStore,
};
pub use command::{CommandFile, CommandScope, Expanded, load_commands};
pub use directory::{DirectoryTransport, SignedFeed, SkillDirectory};
pub use error::SkillError;
pub use manifest::SkillManifest;
pub use package::{ImportLimits, ImportOptions, InstalledSkill};
pub use scan::{Skill, SkillRoot, SkillRoots, SkillSource};
