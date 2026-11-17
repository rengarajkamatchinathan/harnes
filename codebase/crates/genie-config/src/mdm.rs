//! MDM policy readers (08 §5.1, D38): macOS configuration profiles / managed preferences
//! (domain `com.synergech.genie`) and Windows `HKLM\Software\Policies\Genie` (GPO/Intune
//! with the shipped ADMX template, `codebase/policy-templates/`). Linux uses the system file.
//!
//! The parsing is plain Rust and tested on every OS with fixtures; only the registry
//! access itself is Windows-only (`crate::os_windows`).
//!
//! **Registry layout** (what the ADMX template writes):
//! - values directly under `Genie` are dotted policy keys (`org.name`, `disableBypassMode`);
//! - a subkey adds a key prefix: `Genie\audit` value `includeContent` = `audit.includeContent`,
//!   and a subkey may itself be named with dots (`Genie\permissions.deny`);
//! - a subkey whose value names are all positive integers (ADMX `<list>`) is an array at
//!   that key, ordered by number;
//! - `REG_DWORD`/`REG_QWORD` are booleans for boolean keys (non-zero = true) and numbers
//!   otherwise; `REG_MULTI_SZ` is a string array; `REG_SZ` that holds JSON (`[…]`/`{…}`) is
//!   parsed; for list keys a plain `REG_SZ` is split on `;` and newlines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{Map, Value};

use crate::fsutil::read_optional;
use crate::policy::{
    MergeRule, PolicyError, PolicySource, PolicySourceKind, PolicyValues, admin_only, merge_rule,
    parse_policy,
};

/// The macOS preference domain (configuration profile payload type).
pub const MAC_DOMAIN: &str = "com.synergech.genie";
/// The Windows policy key under `HKEY_LOCAL_MACHINE`.
pub const WINDOWS_POLICY_KEY: &str = r"Software\Policies\Genie";

// ---- macOS -------------------------------------------------------------------------------

fn plist_to_json(v: &plist::Value) -> Option<Value> {
    Some(match v {
        plist::Value::Dictionary(d) => {
            let mut m = Map::new();
            for (k, v) in d {
                if let Some(j) = plist_to_json(v) {
                    m.insert(k.clone(), j);
                }
            }
            Value::Object(m)
        }
        plist::Value::Array(a) => Value::Array(a.iter().filter_map(plist_to_json).collect()),
        plist::Value::Boolean(b) => Value::Bool(*b),
        plist::Value::Integer(i) => match (i.as_signed(), i.as_unsigned()) {
            (Some(n), _) => Value::from(n),
            (None, Some(n)) => Value::from(n),
            _ => return None,
        },
        plist::Value::Real(f) => serde_json::Number::from_f64(*f).map(Value::Number)?,
        plist::Value::String(s) => Value::String(s.clone()),
        plist::Value::Date(d) => Value::String(d.to_xml_format()),
        plist::Value::Data(b) => Value::String(base64::engine::general_purpose::STANDARD.encode(b)),
        _ => return None,
    })
}

/// Drop profile bookkeeping (`PayloadUUID`, `PayloadType`, …) and flatten.
fn payload_values(m: &Map<String, Value>) -> Result<PolicyValues, String> {
    let mut clean = Map::new();
    for (k, v) in m {
        if !k.starts_with("Payload") {
            clean.insert(k.clone(), v.clone());
        }
    }
    let bytes = serde_json::to_vec(&Value::Object(clean)).map_err(|e| e.to_string())?;
    parse_policy(&bytes)
}

fn read_plist(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    let v = plist::Value::from_reader(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    match plist_to_json(&v) {
        Some(Value::Object(m)) => Ok(m),
        _ => Err("expected a dictionary".into()),
    }
}

/// A managed-preferences plist (`/Library/Managed Preferences/com.synergech.genie.plist`,
/// XML or binary): the dictionary is the policy.
pub fn parse_managed_plist(bytes: &[u8]) -> Result<PolicyValues, String> {
    payload_values(&read_plist(bytes)?)
}

/// An unsigned `.mobileconfig` (XML plist): the payloads for [`MAC_DOMAIN`], either as a
/// payload of that type or as a Custom Settings payload
/// (`com.apple.ManagedClient.preferences` → `Forced` → `mcx_preference_settings`). Later
/// payloads override earlier ones. Signed profiles (CMS) are installed by macOS into
/// managed preferences, which [`MacProfileSource`] reads.
pub fn parse_mobileconfig(bytes: &[u8]) -> Result<PolicyValues, String> {
    let top = read_plist(bytes)?;
    let mut out = PolicyValues::new();
    let mut found = false;
    for p in top
        .get("PayloadContent")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        match p.get("PayloadType").and_then(Value::as_str) {
            Some(MAC_DOMAIN) => {
                out.extend(payload_values(p)?);
                found = true;
            }
            Some("com.apple.ManagedClient.preferences") => {
                let forced = p
                    .get("PayloadContent")
                    .and_then(|c| c.get(MAC_DOMAIN))
                    .and_then(|d| d.get("Forced"))
                    .and_then(Value::as_array);
                for f in forced.into_iter().flatten() {
                    if let Some(s) = f.get("mcx_preference_settings").and_then(Value::as_object) {
                        out.extend(payload_values(s)?);
                        found = true;
                    }
                }
            }
            _ => {}
        }
    }
    if found {
        Ok(out)
    } else {
        Err(format!("no {MAC_DOMAIN} payload"))
    }
}

/// macOS managed preferences written by a configuration profile. The computer-level file
/// wins over the user-level one; both live in the root-owned `/Library/Managed Preferences`.
#[derive(Debug, Clone)]
pub struct MacProfileSource {
    /// Lowest precedence first.
    pub paths: Vec<PathBuf>,
    pub require_admin: bool,
}

impl MacProfileSource {
    pub fn system() -> MacProfileSource {
        let base = Path::new("/Library/Managed Preferences");
        let file = format!("{MAC_DOMAIN}.plist");
        let mut paths = Vec::new();
        if let Ok(user) = std::env::var("USER")
            && !user.is_empty()
            && !user.contains(['/', '\\'])
            && user != ".."
        {
            paths.push(base.join(user).join(&file));
        }
        paths.push(base.join(file));
        MacProfileSource {
            paths,
            require_admin: true,
        }
    }
}

impl PolicySource for MacProfileSource {
    fn kind(&self) -> PolicySourceKind {
        PolicySourceKind::Mdm
    }

    fn load(&self) -> Result<Option<PolicyValues>, PolicyError> {
        let mut out: Option<PolicyValues> = None;
        for path in &self.paths {
            let name = path.display().to_string();
            let Some(bytes) =
                read_optional(path).map_err(|e| PolicyError::Io(name.clone(), e.to_string()))?
            else {
                continue;
            };
            if self.require_admin
                && !admin_only(path).map_err(|e| PolicyError::Io(name.clone(), e.to_string()))?
            {
                return Err(PolicyError::UserWritable(name));
            }
            let v = parse_managed_plist(&bytes).map_err(|e| PolicyError::Parse(name, e))?;
            out.get_or_insert_with(PolicyValues::new).extend(v);
        }
        Ok(out)
    }
}

// ---- Windows registry --------------------------------------------------------------------

/// A registry value as the policy reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    Dword(u32),
    Qword(u64),
    Sz(String),
    MultiSz(Vec<String>),
}

/// One registry key: its values and the names of its subkeys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegKey {
    pub values: Vec<(String, RegValue)>,
    pub subkeys: Vec<String>,
}

/// Read access to `HKEY_LOCAL_MACHINE` (the real one on Windows, a map in tests).
pub trait RegistryReader: Send + Sync {
    /// `Ok(None)` = the key doesn't exist.
    fn read_key(&self, path: &str) -> Result<Option<RegKey>, String>;
}

/// An in-memory registry: `path → key` (tests and fixtures).
#[derive(Debug, Clone, Default)]
pub struct MemoryRegistry(pub BTreeMap<String, RegKey>);

impl MemoryRegistry {
    /// Add a value, creating the key and linking it from its parents.
    pub fn set(&mut self, path: &str, name: &str, value: RegValue) -> &mut Self {
        self.ensure(path);
        if let Some(k) = self.0.get_mut(path) {
            k.values.push((name.to_string(), value));
        }
        self
    }

    fn ensure(&mut self, path: &str) {
        if self.0.contains_key(path) {
            return;
        }
        self.0.insert(path.to_string(), RegKey::default());
        if let Some((parent, child)) = path.rsplit_once('\\')
            && parent.len() >= WINDOWS_POLICY_KEY.len()
        {
            self.ensure(parent);
            if let Some(p) = self.0.get_mut(parent) {
                p.subkeys.push(child.to_string());
            }
        }
    }
}

impl RegistryReader for MemoryRegistry {
    fn read_key(&self, path: &str) -> Result<Option<RegKey>, String> {
        Ok(self.0.get(path).cloned())
    }
}

/// Keys that hold booleans although their merge rule doesn't say so.
const BOOL_KEYS: &[&str] = &["audit.includeContent", "updates.auto"];

fn is_bool_key(key: &str) -> bool {
    BOOL_KEYS.contains(&key)
        || matches!(merge_rule(key), MergeRule::Enable | MergeRule::Disable)
}

fn is_list_key(key: &str) -> bool {
    matches!(merge_rule(key), MergeRule::AllowList | MergeRule::DenyList)
        || key == crate::policy::KEY_POLICY_KEYS
}

fn convert(key: &str, v: &RegValue) -> Value {
    let number = |n: u64| {
        if is_bool_key(key) {
            Value::Bool(n != 0)
        } else {
            Value::from(n)
        }
    };
    match v {
        RegValue::Dword(n) => number(u64::from(*n)),
        RegValue::Qword(n) => number(*n),
        RegValue::MultiSz(list) => Value::Array(
            list.iter()
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.clone()))
                .collect(),
        ),
        RegValue::Sz(s) => {
            let t = s.trim();
            if (t.starts_with('[') || t.starts_with('{'))
                && let Ok(j) = serde_json::from_str::<Value>(t)
            {
                return j;
            }
            if is_bool_key(key) {
                match t.to_ascii_lowercase().as_str() {
                    "true" | "1" => return Value::Bool(true),
                    "false" | "0" => return Value::Bool(false),
                    _ => {}
                }
            }
            if is_list_key(key) {
                return Value::Array(
                    t.split([';', '\n'])
                        .map(str::trim)
                        .filter(|x| !x.is_empty())
                        .map(|x| Value::String(x.to_string()))
                        .collect(),
                );
            }
            if matches!(merge_rule(key), MergeRule::Min)
                && let Ok(n) = t.parse::<u64>()
            {
                return Value::from(n);
            }
            Value::String(s.clone())
        }
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

fn walk(
    reg: &dyn RegistryReader,
    path: &str,
    prefix: &str,
    depth: usize,
    out: &mut PolicyValues,
) -> Result<(), String> {
    let Some(key) = reg.read_key(path)? else {
        return Ok(());
    };
    let named: Vec<&(String, RegValue)> =
        key.values.iter().filter(|(n, _)| !n.is_empty()).collect();
    let is_list = !prefix.is_empty()
        && !named.is_empty()
        && named.iter().all(|(n, _)| n.parse::<u32>().is_ok_and(|i| i > 0));
    if is_list {
        let mut items: Vec<(u32, Value)> = named
            .iter()
            .filter_map(|(n, v)| {
                let i = n.parse::<u32>().ok()?;
                let item = match v {
                    RegValue::Sz(s) => Value::String(s.clone()),
                    other => convert("", other),
                };
                Some((i, item))
            })
            .collect();
        items.sort_by_key(|(i, _)| *i);
        out.insert(
            prefix.to_string(),
            Value::Array(items.into_iter().map(|(_, v)| v).collect()),
        );
    } else {
        for (name, v) in named {
            let k = join(prefix, name);
            let value = convert(&k, v);
            out.insert(k, value);
        }
    }
    if depth < 4 {
        for sub in &key.subkeys {
            walk(
                reg,
                &format!("{path}\\{sub}"),
                &join(prefix, sub),
                depth + 1,
                out,
            )?;
        }
    }
    Ok(())
}

/// Read `HKLM\Software\Policies\Genie` into flat policy values. `Ok(None)` = no key.
pub fn registry_to_policy(reg: &dyn RegistryReader) -> Result<Option<PolicyValues>, String> {
    if reg.read_key(WINDOWS_POLICY_KEY)?.is_none() {
        return Ok(None);
    }
    let mut out = PolicyValues::new();
    walk(reg, WINDOWS_POLICY_KEY, "", 0, &mut out)?;
    // Nested JSON objects from `REG_SZ` values flatten like the file does.
    let bytes = serde_json::to_vec(&out).map_err(|e| e.to_string())?;
    let mut flat = parse_policy(&bytes)?;
    // `parse_policy` treats dotted keys as leaves already; nothing else to do.
    flat.retain(|_, v| !v.is_null());
    Ok(Some(flat))
}

/// Windows Group Policy / Intune (`HKLM\Software\Policies\Genie`). HKLM is admin-writable
/// only, so no extra owner check is needed.
pub struct RegistryPolicySource {
    reader: Box<dyn RegistryReader>,
}

impl RegistryPolicySource {
    pub fn new(reader: Box<dyn RegistryReader>) -> RegistryPolicySource {
        RegistryPolicySource { reader }
    }

    /// The real registry.
    #[cfg(windows)]
    pub fn system() -> RegistryPolicySource {
        RegistryPolicySource::new(Box::new(crate::os_windows::WinRegistry))
    }
}

impl PolicySource for RegistryPolicySource {
    fn kind(&self) -> PolicySourceKind {
        PolicySourceKind::Mdm
    }

    fn load(&self) -> Result<Option<PolicyValues>, PolicyError> {
        registry_to_policy(self.reader.as_ref())
            .map_err(|e| PolicyError::Io(format!("HKLM\\{WINDOWS_POLICY_KEY}"), e))
    }
}

// ---- Windows ACL decision ----------------------------------------------------------------

/// One access-control entry of a file's DACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AceInfo {
    /// `ACCESS_ALLOWED_ACE_TYPE` (deny entries only restrict and are ignored).
    pub allow: bool,
    /// `INHERIT_ONLY_ACE`: applies to children, not this file.
    pub inherit_only: bool,
    pub mask: u32,
    /// String SID (`S-1-5-32-544`).
    pub sid: String,
}

/// SYSTEM, Administrators, TrustedInstaller.
pub const ADMIN_SIDS: &[&str] = &[
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];
/// CREATOR OWNER only matters for inheritance; on a file it grants nothing.
const CREATOR_OWNER: &str = "S-1-3-0";

/// Rights that let a principal change the file or its ACL.
pub const WRITE_RIGHTS: u32 = 0x0000_0002 // FILE_WRITE_DATA
    | 0x0000_0004 // FILE_APPEND_DATA
    | 0x0001_0000 // DELETE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000 // GENERIC_WRITE
    | 0x0200_0000; // MAXIMUM_ALLOWED

/// The `policy.json` ACL rule (08 §5.1): owned by an admin principal and nobody else may
/// write it. `dacl = None` is a NULL DACL (everyone has full access).
pub fn acl_admin_only(owner_sid: &str, dacl: Option<&[AceInfo]>) -> bool {
    if !ADMIN_SIDS.contains(&owner_sid) {
        return false;
    }
    let Some(aces) = dacl else {
        return false;
    };
    aces.iter().all(|a| {
        !a.allow
            || a.inherit_only
            || a.mask & WRITE_RIGHTS == 0
            || ADMIN_SIDS.contains(&a.sid.as_str())
            || a.sid == CREATOR_OWNER
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MANAGED_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>org.name</key><string>Synergech</string>
  <key>disableBypassMode</key><true/>
  <key>permissions</key>
  <dict>
    <key>deny</key><array><string>Bash(rm:*)</string></array>
  </dict>
  <key>retention.maxDays</key><integer>30</integer>
  <key>PayloadUUID</key><string>ignored</string>
</dict>
</plist>"#;

    #[test]
    fn managed_preferences_xml_and_binary() {
        let v = parse_managed_plist(MANAGED_XML.as_bytes()).unwrap();
        assert_eq!(v["org.name"], json!("Synergech"));
        assert_eq!(v["disableBypassMode"], json!(true));
        assert_eq!(v["permissions.deny"], json!(["Bash(rm:*)"]));
        assert_eq!(v["retention.maxDays"], json!(30));
        assert!(!v.contains_key("PayloadUUID"));

        // macOS writes managed preferences as binary plists.
        let parsed = plist::Value::from_reader_xml(MANAGED_XML.as_bytes()).unwrap();
        let mut bin = Vec::new();
        parsed.to_writer_binary(&mut bin).unwrap();
        assert_eq!(parse_managed_plist(&bin).unwrap(), v);
        assert!(parse_managed_plist(b"not a plist").is_err());
    }

    #[test]
    fn mobileconfig_payloads() {
        let fixture = include_str!("../../../policy-templates/macos/com.synergech.genie.mobileconfig");
        let v = parse_mobileconfig(fixture.as_bytes()).unwrap();
        assert_eq!(v["org.name"], json!("Example Corp"));
        assert_eq!(v["telemetry.level"], json!("crash-only"));
        assert_eq!(v["providers.allowed"], json!(["azure-foundry"]));
        assert!(v.keys().all(|k| !k.starts_with("Payload")));

        let custom = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>PayloadContent</key><array><dict>
  <key>PayloadType</key><string>com.apple.ManagedClient.preferences</string>
  <key>PayloadContent</key><dict><key>com.synergech.genie</key><dict><key>Forced</key><array><dict>
    <key>mcx_preference_settings</key><dict><key>mcp.disableLocal</key><true/></dict>
  </dict></array></dict></dict>
</dict></array></dict></plist>"#;
        let v = parse_mobileconfig(custom.as_bytes()).unwrap();
        assert_eq!(v["mcp.disableLocal"], json!(true));
        let other = r#"<?xml version="1.0"?><plist version="1.0"><dict><key>PayloadContent</key><array/></dict></plist>"#;
        assert!(parse_mobileconfig(other.as_bytes()).is_err());
    }

    #[test]
    fn mac_source_reads_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.plist");
        let device = dir.path().join("device.plist");
        std::fs::write(&user, MANAGED_XML.replace("Synergech", "UserLevel")).unwrap();
        let src = MacProfileSource {
            paths: vec![user.clone(), device.clone()],
            require_admin: false,
        };
        assert_eq!(src.load().unwrap().unwrap()["org.name"], json!("UserLevel"));
        std::fs::write(&device, MANAGED_XML).unwrap();
        assert_eq!(src.load().unwrap().unwrap()["org.name"], json!("Synergech"));
        let none = MacProfileSource {
            paths: vec![dir.path().join("missing.plist")],
            require_admin: true,
        };
        assert_eq!(none.load().unwrap(), None);
        #[cfg(unix)]
        {
            let strict = MacProfileSource {
                paths: vec![device],
                require_admin: true,
            };
            assert!(matches!(strict.load(), Err(PolicyError::UserWritable(_))));
        }
    }

    fn fake_registry() -> MemoryRegistry {
        let k = WINDOWS_POLICY_KEY;
        let mut r = MemoryRegistry::default();
        r.set(k, "org.name", RegValue::Sz("Contoso".into()))
            .set(k, "disableBypassMode", RegValue::Dword(1))
            .set(k, "retention.maxDays", RegValue::Dword(90))
            .set(k, "telemetry.level", RegValue::Sz("off".into()))
            .set(k, "", RegValue::Sz("default value ignored".into()))
            .set(
                k,
                "models.blocked",
                RegValue::MultiSz(vec!["gpt-4o*".into(), "".into()]),
            )
            .set(
                k,
                "webFetch.domains",
                RegValue::Sz("example.com; docs.rs".into()),
            )
            .set(k, "mcp.allowed", RegValue::Sz(r#"[{"name":"github"}]"#.into()))
            .set(
                &format!(r"{k}\permissions.deny"),
                "2",
                RegValue::Sz("WebSearch".into()),
            )
            .set(
                &format!(r"{k}\permissions.deny"),
                "1",
                RegValue::Sz("Bash(curl:*)".into()),
            )
            .set(&format!(r"{k}\audit"), "includeContent", RegValue::Dword(0))
            .set(
                &format!(r"{k}\audit\otlp"),
                "endpoint",
                RegValue::Sz("https://otel.contoso.com".into()),
            )
            .set(&format!(r"{k}\memory"), "enabled", RegValue::Qword(0));
        r
    }

    #[test]
    fn registry_values_map_to_policy_keys() {
        let v = registry_to_policy(&fake_registry()).unwrap().unwrap();
        assert_eq!(v["org.name"], json!("Contoso"));
        assert_eq!(v["disableBypassMode"], json!(true));
        assert_eq!(v["retention.maxDays"], json!(90));
        assert_eq!(v["telemetry.level"], json!("off"));
        assert_eq!(v["models.blocked"], json!(["gpt-4o*"]));
        assert_eq!(v["webFetch.domains"], json!(["example.com", "docs.rs"]));
        assert_eq!(v["mcp.allowed"], json!([{"name": "github"}]));
        assert_eq!(v["permissions.deny"], json!(["Bash(curl:*)", "WebSearch"]));
        assert_eq!(v["audit.includeContent"], json!(false));
        assert_eq!(v["audit.otlp.endpoint"], json!("https://otel.contoso.com"));
        assert_eq!(v["memory.enabled"], json!(false));
        assert!(!v.contains_key(""));

        let src = RegistryPolicySource::new(Box::new(MemoryRegistry::default()));
        assert_eq!(src.load().unwrap(), None);
        assert_eq!(src.kind(), PolicySourceKind::Mdm);
    }

    #[test]
    fn acl_rule() {
        let ace = |sid: &str, mask: u32| AceInfo {
            allow: true,
            inherit_only: false,
            mask,
            sid: sid.into(),
        };
        let admins = "S-1-5-32-544";
        let users = "S-1-5-32-545";
        let read_exec = 0x1200a9;
        let full = 0x1f01ff;
        let good = [
            ace("S-1-5-18", full),
            ace(admins, full),
            ace(users, read_exec),
            ace(CREATOR_OWNER, full),
        ];
        assert!(acl_admin_only(admins, Some(&good)));
        assert!(!acl_admin_only("S-1-5-21-1-2-3-1001", Some(&good)), "user-owned");
        assert!(!acl_admin_only(admins, None), "NULL DACL");
        let mut bad = good.to_vec();
        bad.push(ace(users, 0x2)); // Users: write data
        assert!(!acl_admin_only(admins, Some(&bad)));
        let mut inherit = good.to_vec();
        inherit.push(AceInfo {
            inherit_only: true,
            ..ace(users, full)
        });
        assert!(acl_admin_only(admins, Some(&inherit)));
        let mut deny = good.to_vec();
        deny.push(AceInfo {
            allow: false,
            ..ace("S-1-1-0", full)
        });
        assert!(acl_admin_only(admins, Some(&deny)));
    }
}
