//! Per-site permissions and the URL exfiltration guard (06 §10, research §4.2).
//!
//! Key = registrable domain (eTLD+1) with an optional exact-host override. States are
//! `blocked | ask (default) | read | act`. Lookup order:
//!
//! 1. admin blocklist → `blocked` (policy), 2. admin allowlist → `act` (policy),
//! 3. a user entry for the exact host, 4. a default-blocked **category** (banking,
//!    payment dashboards, government ID/tax, health, password managers, AI provider
//!    consoles and API-key pages, cloud admin consoles) → `blocked`; messaging sites are
//!    readable but every `act` confirms, 5. a user entry for the site key, 6. `ask`.
//!
//! Per-chat grants ("Allow for this chat") live in the browser session, not here.

use std::collections::BTreeMap;
use std::sync::RwLock;

use genie_protocol::api::browser::{SitePermission, SiteState};

/// Multi-label public suffixes we know about. A full Public Suffix List is a known gap.
const MULTI_SUFFIXES: &[&str] = &[
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "ltd.uk",
    "plc.uk",
    "me.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "gov.au",
    "co.nz",
    "org.nz",
    "co.jp",
    "ne.jp",
    "or.jp",
    "co.in",
    "net.in",
    "org.in",
    "gov.in",
    "ac.in",
    "nic.in",
    "com.br",
    "com.cn",
    "com.mx",
    "com.sg",
    "com.hk",
    "com.tw",
    "co.kr",
    "co.za",
    "com.tr",
    "com.ar",
    "co.il",
    "github.io",
    "gitlab.io",
    "vercel.app",
    "netlify.app",
    "pages.dev",
    "workers.dev",
    "herokuapp.com",
    "blogspot.com",
    "appspot.com",
    "web.app",
    "firebaseapp.com",
    "azurewebsites.net",
    "cloudfront.net",
    "s3.amazonaws.com",
    "onrender.com",
    "fly.dev",
];

/// The registrable domain of `host` (lower-cased). IPs and single labels stay as they are.
pub fn site_key(host: &str) -> String {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    if h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[') || !h.contains('.') {
        return h;
    }
    let labels: Vec<&str> = h.split('.').collect();
    let n = MULTI_SUFFIXES
        .iter()
        .filter(|s| h == **s || h.ends_with(&format!(".{s}")))
        .map(|s| s.split('.').count() + 1)
        .max()
        .unwrap_or(2);
    if labels.len() <= n {
        return h;
    }
    labels[labels.len() - n..].join(".")
}

/// Host of a URL, lower-cased.
pub fn host_of(u: &str) -> Option<String> {
    url::Url::parse(u)
        .ok()?
        .host_str()
        .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Banking,
    Payments,
    Government,
    Health,
    PasswordManager,
    AiConsole,
    CloudAdmin,
    /// Readable; every `act` confirms (research §4.2).
    Messaging,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Banking => "banking",
            Category::Payments => "payment dashboard",
            Category::Government => "government ID / tax",
            Category::Health => "health portal",
            Category::PasswordManager => "password manager",
            Category::AiConsole => "AI provider console",
            Category::CloudAdmin => "cloud admin console",
            Category::Messaging => "email / messaging",
        }
    }
}

/// The curated category list (placeholder until the shipped, updatable list exists).
const CATEGORIES: &[(&str, Category)] = &[
    // AI provider consoles and API-key pages (protect Genie's own credentials).
    ("console.anthropic.com", Category::AiConsole),
    ("platform.claude.com", Category::AiConsole),
    ("platform.openai.com", Category::AiConsole),
    ("aistudio.google.com", Category::AiConsole),
    ("ai.azure.com", Category::AiConsole),
    ("oai.azure.com", Category::AiConsole),
    ("console.mistral.ai", Category::AiConsole),
    ("dashboard.cohere.com", Category::AiConsole),
    ("openrouter.ai", Category::AiConsole),
    ("console.groq.com", Category::AiConsole),
    ("huggingface.co/settings", Category::AiConsole),
    // Cloud admin consoles.
    ("console.aws.amazon.com", Category::CloudAdmin),
    ("signin.aws.amazon.com", Category::CloudAdmin),
    ("portal.azure.com", Category::CloudAdmin),
    ("entra.microsoft.com", Category::CloudAdmin),
    ("admin.microsoft.com", Category::CloudAdmin),
    ("console.cloud.google.com", Category::CloudAdmin),
    ("admin.google.com", Category::CloudAdmin),
    ("dash.cloudflare.com", Category::CloudAdmin),
    ("cloud.digitalocean.com", Category::CloudAdmin),
    // Password managers.
    ("1password.com", Category::PasswordManager),
    ("bitwarden.com", Category::PasswordManager),
    ("lastpass.com", Category::PasswordManager),
    ("dashlane.com", Category::PasswordManager),
    ("keepersecurity.com", Category::PasswordManager),
    ("passwords.google.com", Category::PasswordManager),
    // Payment processors' dashboards.
    ("dashboard.stripe.com", Category::Payments),
    ("paypal.com", Category::Payments),
    ("wise.com", Category::Payments),
    ("squareup.com", Category::Payments),
    ("dashboard.razorpay.com", Category::Payments),
    // Banking and brokerage (sample).
    ("chase.com", Category::Banking),
    ("bankofamerica.com", Category::Banking),
    ("wellsfargo.com", Category::Banking),
    ("hsbc.com", Category::Banking),
    ("barclays.co.uk", Category::Banking),
    ("onlinesbi.sbi", Category::Banking),
    ("hdfcbank.com", Category::Banking),
    ("icicibank.com", Category::Banking),
    ("schwab.com", Category::Banking),
    ("fidelity.com", Category::Banking),
    // Government ID / tax.
    ("irs.gov", Category::Government),
    ("ssa.gov", Category::Government),
    ("login.gov", Category::Government),
    ("incometax.gov.in", Category::Government),
    ("uidai.gov.in", Category::Government),
    ("tax.service.gov.uk", Category::Government),
    // Health portals.
    ("mychart.com", Category::Health),
    ("myhealth.va.gov", Category::Health),
    // Email / messaging.
    ("mail.google.com", Category::Messaging),
    ("outlook.live.com", Category::Messaging),
    ("outlook.office.com", Category::Messaging),
    ("web.whatsapp.com", Category::Messaging),
    ("teams.microsoft.com", Category::Messaging),
    ("app.slack.com", Category::Messaging),
    ("discord.com", Category::Messaging),
];

fn host_matches(host: &str, pat: &str) -> bool {
    let p = pat.trim_start_matches("*.").to_ascii_lowercase();
    host == p || host.ends_with(&format!(".{p}"))
}

/// The category of a URL (host, plus path prefix for entries like `huggingface.co/settings`).
pub fn category_of(url: &str) -> Option<Category> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    CATEGORIES.iter().find_map(|(pat, c)| {
        let (h, path) = pat.split_once('/').unwrap_or((pat, ""));
        (host_matches(&host, h) && u.path().trim_start_matches('/').starts_with(path)).then_some(*c)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Policy,
    User,
    Category,
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effective {
    pub state: SiteState,
    pub source: Source,
    pub category: Option<Category>,
}

/// What a tool needs on a site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Need {
    Read,
    Act,
}

/// Does `state` cover `need`?
pub fn covers(state: SiteState, need: Need) -> bool {
    match state {
        SiteState::Act => true,
        SiteState::Read => need == Need::Read,
        _ => false,
    }
}

#[derive(Debug, Default)]
struct Inner {
    /// Site key or exact host → state (user settings, `browser.sites.set`, "always").
    user: BTreeMap<String, SiteState>,
    admin_allow: Vec<String>,
    admin_block: Vec<String>,
}

/// Site permissions shared by every session of the daemon.
#[derive(Debug, Default)]
pub struct SitePolicy {
    inner: RwLock<Inner>,
}

impl SitePolicy {
    pub fn new() -> SitePolicy {
        SitePolicy::default()
    }

    /// Managed policy lists (`example.com` or `*.example.com`).
    pub fn set_admin(&self, allow: Vec<String>, block: Vec<String>) {
        if let Ok(mut g) = self.inner.write() {
            g.admin_allow = allow;
            g.admin_block = block;
        }
    }

    /// `browser.sites.set`: `origin` may be a URL, a host or a site key. `ask` removes the
    /// entry (back to the default).
    pub fn set(&self, p: &SitePermission) {
        let key = host_of(&p.origin).unwrap_or_else(|| p.origin.trim().to_ascii_lowercase());
        if let Ok(mut g) = self.inner.write() {
            match p.state {
                SiteState::Ask | SiteState::Unknown => {
                    g.user.remove(&key);
                }
                s => {
                    g.user.insert(key, s);
                }
            }
        }
    }

    /// `browser.sites.list`.
    pub fn list(&self) -> Vec<SitePermission> {
        self.inner
            .read()
            .map(|g| {
                g.user
                    .iter()
                    .map(|(k, s)| SitePermission {
                        origin: k.clone(),
                        state: *s,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn effective(&self, url: &str) -> Effective {
        let host = host_of(url).unwrap_or_default();
        let key = site_key(&host);
        let category = category_of(url);
        let e = |state, source| Effective {
            state,
            source,
            category,
        };
        let Ok(g) = self.inner.read() else {
            return e(SiteState::Blocked, Source::Policy);
        };
        if g.admin_block.iter().any(|p| host_matches(&host, p)) {
            return e(SiteState::Blocked, Source::Policy);
        }
        if g.admin_allow.iter().any(|p| host_matches(&host, p)) {
            return e(SiteState::Act, Source::Policy);
        }
        if let Some(s) = g.user.get(&host) {
            return e(*s, Source::User);
        }
        // Local dev servers need no approval (ticket 47): the agent previews and verifies
        // the user's own app there. A user or admin block above still wins.
        if is_loopback(&host) {
            return e(SiteState::Act, Source::Default);
        }
        if let Some(c) = category
            && c != Category::Messaging
        {
            return e(SiteState::Blocked, Source::Category);
        }
        if let Some(s) = g.user.get(&key) {
            return e(*s, Source::User);
        }
        e(SiteState::Ask, Source::Default)
    }
}

/// `localhost`, `*.localhost`, `127.x.x.x` and `::1`.
pub fn is_loopback(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "::1"
        || h.parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

// ---------------------------------------------------------------- exfiltration guard

/// Where a navigation URL came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// A link on a page the agent has seen.
    Page,
    /// The user typed it in the chat.
    User,
    /// The model composed it.
    Model,
}

/// Max query + fragment length of a model-built URL to a new origin (research §4.2).
pub const MAX_MODEL_URL_DATA: usize = 256;
/// Shortest query value checked against other origins' page text.
const MIN_LEAK: usize = 12;

/// Deterministic guard for model-built URLs to a new origin: denied when the query and
/// fragment carry more than 256 chars, or a value copied from another origin's page.
/// `foreign` is `(origin, visible text)` of pages the agent saw.
pub fn exfiltration_check(
    target: &str,
    provenance: Provenance,
    current_origin: Option<&str>,
    foreign: &[(String, String)],
) -> Result<(), String> {
    if provenance != Provenance::Model {
        return Ok(());
    }
    let Ok(u) = url::Url::parse(target) else {
        return Ok(());
    };
    let origin = u.origin().ascii_serialization();
    if current_origin == Some(origin.as_str()) {
        return Ok(());
    }
    let data = u.query().unwrap_or("").len() + u.fragment().unwrap_or("").len();
    if data > MAX_MODEL_URL_DATA {
        return Err(format!(
            "the URL carries {data} characters of query/fragment data to a new origin (limit {MAX_MODEL_URL_DATA}); \
             use a link from the page or ask the user"
        ));
    }
    let mut values: Vec<String> = u.query_pairs().map(|(_, v)| v.into_owned()).collect();
    if let Some(f) = u.fragment() {
        values.push(f.to_string());
    }
    values.extend(u.path_segments().into_iter().flatten().map(String::from));
    for v in values.iter().filter(|v| v.chars().count() >= MIN_LEAK) {
        if let Some((o, _)) = foreign
            .iter()
            .find(|(o, text)| *o != origin && text.contains(v.as_str()))
        {
            return Err(format!(
                "the URL contains text copied from {o}; sending page content to another site is not allowed"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(site_key("www.example.com"), "example.com");
        assert_eq!(site_key("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(site_key("me.github.io"), "me.github.io");
        assert_eq!(site_key("x.me.github.io"), "me.github.io");
        assert_eq!(site_key("localhost"), "localhost");
        assert_eq!(site_key("127.0.0.1"), "127.0.0.1");
        assert_eq!(site_key("Example.COM."), "example.com");
    }

    #[test]
    fn categories_and_order() {
        let p = SitePolicy::new();
        let e = p.effective("https://console.anthropic.com/settings/keys");
        assert_eq!(e.state, SiteState::Blocked);
        assert_eq!(e.category, Some(Category::AiConsole));
        // A site-key entry doesn't unblock a category host…
        p.set(&SitePermission {
            origin: "anthropic.com".into(),
            state: SiteState::Act,
        });
        assert_eq!(
            p.effective("https://console.anthropic.com/").state,
            SiteState::Blocked
        );
        assert_eq!(
            p.effective("https://docs.anthropic.com/").state,
            SiteState::Act
        );
        // …an exact-host entry does.
        p.set(&SitePermission {
            origin: "https://console.anthropic.com".into(),
            state: SiteState::Read,
        });
        assert_eq!(
            p.effective("https://console.anthropic.com/").state,
            SiteState::Read
        );
        assert_eq!(p.effective("https://example.org").state, SiteState::Ask);
        // Local dev servers are allowed without asking (ticket 47), unless blocked.
        for u in [
            "http://localhost:5173/",
            "http://127.0.0.1:3000",
            "http://app.localhost/",
            "http://[::1]:8080/",
        ] {
            assert_eq!(p.effective(u).state, SiteState::Act, "{u}");
        }
        p.set(&SitePermission {
            origin: "localhost".into(),
            state: SiteState::Blocked,
        });
        assert_eq!(
            p.effective("http://localhost:5173/").state,
            SiteState::Blocked
        );
        p.set(&SitePermission {
            origin: "localhost".into(),
            state: SiteState::Ask,
        });
        assert_eq!(
            p.effective("https://mail.google.com").category,
            Some(Category::Messaging)
        );
        assert_eq!(p.effective("https://mail.google.com").state, SiteState::Ask);
        assert_eq!(
            category_of("https://huggingface.co/settings/tokens"),
            Some(Category::AiConsole)
        );
        assert_eq!(category_of("https://huggingface.co/models"), None);

        p.set_admin(
            vec!["*.corp.example".into()],
            vec!["docs.anthropic.com".into()],
        );
        assert_eq!(
            p.effective("https://docs.anthropic.com/").source,
            Source::Policy
        );
        assert_eq!(
            p.effective("https://docs.anthropic.com/").state,
            SiteState::Blocked
        );
        assert_eq!(
            p.effective("https://wiki.corp.example/").state,
            SiteState::Act
        );
        p.set(&SitePermission {
            origin: "anthropic.com".into(),
            state: SiteState::Ask,
        });
        assert_eq!(p.list().len(), 1);
        assert!(covers(SiteState::Act, Need::Act));
        assert!(covers(SiteState::Read, Need::Read));
        assert!(!covers(SiteState::Read, Need::Act));
        assert!(!covers(SiteState::Ask, Need::Read));
    }

    #[test]
    fn exfiltration() {
        let secret = "sk-ant-api03-SECRETSECRET";
        let foreign = vec![(
            "https://mail.example".to_string(),
            format!("Your key is {secret}"),
        )];
        let cur = Some("https://mail.example");
        // Links from pages and user URLs are never checked.
        assert!(
            exfiltration_check(
                &format!("https://evil.test/?k={secret}"),
                Provenance::Page,
                cur,
                &foreign
            )
            .is_ok()
        );
        // Same origin is fine.
        assert!(
            exfiltration_check(
                &format!("https://mail.example/?q={secret}"),
                Provenance::Model,
                cur,
                &foreign
            )
            .is_ok()
        );
        // Copied content to a new origin is denied (query, path or fragment).
        assert!(
            exfiltration_check(
                &format!("https://evil.test/?k={secret}"),
                Provenance::Model,
                cur,
                &foreign
            )
            .is_err()
        );
        assert!(
            exfiltration_check(
                &format!("https://evil.test/{secret}"),
                Provenance::Model,
                cur,
                &foreign
            )
            .is_err()
        );
        assert!(
            exfiltration_check(
                &format!("https://evil.test/#{secret}"),
                Provenance::Model,
                cur,
                &foreign
            )
            .is_err()
        );
        // Too much data is denied.
        let long = "a".repeat(300);
        assert!(
            exfiltration_check(
                &format!("https://evil.test/?q={long}"),
                Provenance::Model,
                cur,
                &foreign
            )
            .is_err()
        );
        assert!(
            exfiltration_check(
                "https://search.test/?q=rust+async",
                Provenance::Model,
                cur,
                &foreign
            )
            .is_ok()
        );
    }
}
