//! Protocol versioning and the version-skew decision table (01 §4.2, 02 §10).

use semver::Version;

/// Semver of this protocol crate, carried in the handshake.
pub const PROTOCOL_VERSION: &str = "1.0.0";

/// What the daemon does with an `initialize` from a client of a given version (01 §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkewDecision {
    /// Same build, or an older client with the same protocol major: proceed.
    Proceed,
    /// Client is newer and the daemon is idle: reply `restart_pending`, drain, exit (SA-04).
    RestartPending,
    /// Client is newer, the daemon is busy, protocol majors match: serve the client in
    /// degraded mode ("Engine will update when tasks finish").
    Degraded,
    /// Client is newer, daemon busy, majors differ: refuse with `busy`; the client shows
    /// "Genie is finishing N tasks before updating".
    BusyMismatch,
    /// Client is older with a different major: refuse with `protocol_mismatch`.
    ClientTooOld,
    /// A version string did not parse.
    Invalid(String),
}

/// Decide the skew path.
///
/// * `daemon_build` / `client_build`: build versions (engine_version / client_version).
/// * `daemon_protocol` / `client_protocol`: protocol semvers.
/// * `daemon_busy`: running or paused turns exist.
pub fn decide_skew(
    daemon_build: &str,
    daemon_protocol: &str,
    client_build: &str,
    client_protocol: &str,
    daemon_busy: bool,
) -> SkewDecision {
    let parse = |s: &str| Version::parse(s).map_err(|e| format!("bad version `{s}`: {e}"));
    let (db, dp, cb, cp) = match (
        parse(daemon_build),
        parse(daemon_protocol),
        parse(client_build),
        parse(client_protocol),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (.., Err(e)) => {
            return SkewDecision::Invalid(e);
        }
    };
    let same_major = dp.major == cp.major;
    let client_newer = cb > db || cp.major > dp.major;
    if client_newer {
        return match (daemon_busy, same_major) {
            (false, _) => SkewDecision::RestartPending,
            (true, true) => SkewDecision::Degraded,
            (true, false) => SkewDecision::BusyMismatch,
        };
    }
    if same_major {
        SkewDecision::Proceed
    } else {
        SkewDecision::ClientTooOld
    }
}

/// Major component of a protocol semver, if it parses.
pub fn major(v: &str) -> Option<u64> {
    Version::parse(v).ok().map(|v| v.major)
}

#[cfg(test)]
mod tests {
    use super::*;
    use SkewDecision::*;

    #[test]
    fn decision_table() {
        assert_eq!(
            decide_skew("1.0.0", "1.0.0", "1.0.0", "1.0.0", false),
            Proceed
        );
        assert_eq!(
            decide_skew("1.2.0", "1.3.0", "1.1.0", "1.1.0", true),
            Proceed
        );
        assert_eq!(
            decide_skew("1.0.0", "1.0.0", "1.0.1", "1.0.0", false),
            RestartPending
        );
        assert_eq!(
            decide_skew("1.0.0", "1.0.0", "1.0.1", "1.1.0", true),
            Degraded
        );
        assert_eq!(
            decide_skew("1.0.0", "1.0.0", "2.0.0", "2.0.0", true),
            BusyMismatch
        );
        assert_eq!(
            decide_skew("2.0.0", "2.0.0", "1.9.0", "1.9.0", false),
            ClientTooOld
        );
        assert!(matches!(
            decide_skew("x", "1.0.0", "1.0.0", "1.0.0", false),
            Invalid(_)
        ));
    }
}
