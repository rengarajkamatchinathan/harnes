//! Tiny argument parser for xtask subcommands: positionals, `--flag`, `--key value`
//! (repeatable). No clap: xtask stays light.

use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct Opts {
    pub positional: Vec<String>,
    flags: BTreeMap<String, Vec<String>>,
}

/// Flags that never take a value.
const SWITCHES: &[&str] = &[
    "--check",
    "--release",
    "--bump",
    "--pause",
    "--resume",
    "--expedited",
    "--strict",
    "--desktop",
    "--no-build",
    "--force",
    "--list",
    "--next",
    "--pem",
    "--ephemeral",
];

impl Opts {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Opts, String> {
        let mut o = Opts::default();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    o.flags.entry(format!("--{k}")).or_default().push(v.into());
                } else if SWITCHES.contains(&a.as_str()) {
                    o.flags.entry(a).or_default().push(String::new());
                } else {
                    let v = it.next().ok_or_else(|| format!("`{a}` needs a value"))?;
                    o.flags.entry(a).or_default().push(v);
                }
            } else {
                o.positional.push(a);
            }
        }
        Ok(o)
    }

    pub fn has(&self, flag: &str) -> bool {
        self.flags.contains_key(flag)
    }

    pub fn get(&self, flag: &str) -> Option<&str> {
        self.flags
            .get(flag)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    pub fn all(&self, flag: &str) -> Vec<&str> {
        self.flags
            .get(flag)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    pub fn require(&self, flag: &str) -> Result<&str, String> {
        self.get(flag)
            .ok_or_else(|| format!("missing `{flag} <value>`"))
    }

    pub fn pos(&self, i: usize, what: &str) -> Result<&str, String> {
        self.positional
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("missing <{what}>"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let o = Opts::parse(
            [
                "a.json",
                "--pubkey",
                "k1",
                "--pubkey=k2",
                "--bump",
                "--now",
                "x",
            ]
            .map(String::from),
        )
        .unwrap();
        assert_eq!(o.positional, vec!["a.json"]);
        assert_eq!(o.all("--pubkey"), vec!["k1", "k2"]);
        assert!(o.has("--bump"));
        assert_eq!(o.get("--now"), Some("x"));
        assert!(Opts::parse(["--now".to_string()]).is_err());
    }
}
