/// Secrets are always carried as [`SecretString`]; its `Debug` prints `[REDACTED]`
/// (01 §7 rule 2). The keychain and passphrase-file backends land with spec 08.
pub use secrecy::{ExposeSecret, SecretString};

pub mod credentials;
