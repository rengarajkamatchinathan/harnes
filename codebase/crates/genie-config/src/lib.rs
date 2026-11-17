//! `genie-config`: settings layers, managed policy, workspace trust (D23, D38, D43;
//! spec/08-security-and-policy.md §4–5).
//!
//! - [`settings`]: the typed schema, JSON Schema + Settings UI metadata, per-key validation.
//! - [`layers`]: [`ConfigStore`] — default/user/project/local/CLI + policy, merge, sources
//!   per key, locked keys, diagnostics, `config.set`, saving "always" rules, hot reload.
//! - [`policy`]: delivery sources (file, MDM port, signed remote URL), merge, effective view.
//! - [`trust`]: `~/.genie/trust.json` and definition-hash approvals.
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol` only. The
//! permission evaluator lives in `genie-permissions`; the engine folds
//! [`ConfigStore::permission_layers`] and [`EffectivePolicy`] into its rule set.

pub mod fsutil;
pub mod layers;
pub mod policy;
pub mod settings;
pub mod trust;

pub use layers::{ConfigError, ConfigPaths, ConfigStore, LayerKind};
pub use policy::{
    EffectivePolicy, FilePolicySource, PolicyFetcher, PolicyManager, PolicySource,
    PolicySourceKind, SignedPolicy, StaticPolicySource,
};
pub use settings::{ModeSetting, PermissionsSettings, Settings};
pub use trust::{Definition, TrustError, TrustPolicy, TrustStore, definition_hash};
