//! Table-driven tests for 06 §5.2 (evaluation order), §5.3 (modes + policy floor),
//! §5.4 (always + re-evaluation) and §5.5 (taint, SA-29).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)] // test code

use genie_permissions::*;
use genie_protocol::ask::SaveSuggestion;
use genie_protocol::common::{ConfigScope, Mode, RuleScope, SessionKind};

const ROOT: &str = "/repo";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Want {
    Allow,
    Deny(Origin),
    Ask,
}

fn kind(v: &Verdict) -> Want {
    match v {
        Verdict::Allow { .. } => Want::Allow,
        Verdict::Deny { source, .. } => Want::Deny(*source),
        Verdict::Ask { .. } => Want::Ask,
    }
}

fn rules(list: &[(RuleClass, Origin, &str)]) -> RuleSet {
    let mut rs = RuleSet::new();
    for (c, o, r) in list {
        assert!(rs.add_list(*c, *o, &[*r]).is_empty(), "bad rule {r}");
    }
    rs
}

fn shell(cmd: &str) -> Action {
    Action::Shell {
        dialect: ShellDialect::Posix,
        command: cmd.into(),
        parts: Some(cmd.split(" && ").map(str::to_string).collect()),
        cwd: Some(ROOT.into()),
    }
}

fn edit(p: &str) -> Action {
    Action::Edit { path: p.into() }
}

fn read(p: &str) -> Action {
    Action::Read { path: p.into() }
}

fn mcp(server: &str, tool: &str, ro: bool) -> Action {
    Action::Mcp {
        server: server.into(),
        tool: tool.into(),
        read_only_hint: ro,
    }
}

fn fetch(d: &str, pasted: bool, seen: bool) -> Action {
    Action::WebFetch {
        domain: d.into(),
        pasted,
        seen,
    }
}

fn code_eval(mode: Mode, rs: RuleSet) -> Evaluator {
    let mut ctx = Context::code(ROOT, mode);
    ctx.home = Some("/home/me".into());
    ctx.additional_dirs = vec!["/shared".into()];
    ctx.case_insensitive_paths = false;
    Evaluator::new(rs, ctx)
}

use Origin::*;
use RuleClass::{Allow as A, Ask as K, Deny as D};

#[test]
fn evaluation_order() {
    // (rules, action, mode, expected)
    let cases: Vec<(&str, Vec<(RuleClass, Origin, &str)>, Action, Mode, Want)> = vec![
        (
            "policy deny beats user allow",
            vec![(D, Policy, "Bash(rm:*)"), (A, User, "Bash(rm:*)")],
            shell("rm -rf x"),
            Mode::Default,
            Want::Deny(Policy),
        ),
        (
            "policy deny reported before a layer deny",
            vec![(D, Local, "Bash(rm:*)"), (D, Policy, "Bash")],
            shell("rm x"),
            Mode::Default,
            Want::Deny(Policy),
        ),
        (
            "layer deny beats allow (by class, not order)",
            vec![
                (A, Local, "Bash(git push:*)"),
                (D, User, "Bash(git push:*)"),
            ],
            shell("git push"),
            Mode::Default,
            Want::Deny(User),
        ),
        (
            "deny holds in bypass",
            vec![(D, Project, "Edit(/secrets/**)")],
            edit("/repo/secrets/a"),
            Mode::Bypass,
            Want::Deny(Project),
        ),
        (
            "deny beats read-only default",
            vec![(D, User, "Read(./.env)")],
            read("/repo/.env"),
            Mode::Default,
            Want::Deny(User),
        ),
        (
            "bare name deny matches at any depth",
            vec![(D, User, "Read(*.pem)")],
            read("/repo/a/b/k.pem"),
            Mode::Default,
            Want::Deny(User),
        ),
        (
            "ask beats allow",
            vec![(A, User, "Bash(git:*)"), (K, User, "Bash(git push:*)")],
            shell("git push"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "ask holds in accept-edits",
            vec![(K, Local, "Edit(/docs/**)")],
            edit("/repo/docs/a.md"),
            Mode::AcceptEdits,
            Want::Ask,
        ),
        (
            "ask is allowed in bypass",
            vec![(K, Local, "Bash(npm:*)")],
            shell("npm i"),
            Mode::Bypass,
            Want::Allow,
        ),
        (
            "policy ask holds in bypass",
            vec![(K, Policy, "Bash(npm:*)")],
            shell("npm i"),
            Mode::Bypass,
            Want::Ask,
        ),
        (
            "allow rule",
            vec![(A, Local, "Bash(npm run test:*)")],
            shell("npm run test -- -w"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "allow is word-bounded",
            vec![(A, Local, "Bash(git commit:*)")],
            shell("git commits"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "compound: every part must pass",
            vec![(A, Local, "Bash(npm:*)")],
            shell("npm i && curl x"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "compound: read-only parts pass",
            vec![(A, Local, "Bash(npm:*)")],
            shell("npm i && ls"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "compound: one denied part denies",
            vec![(D, User, "Bash(curl:*)")],
            shell("ls && curl x"),
            Mode::Default,
            Want::Deny(User),
        ),
        (
            "read-only shell default",
            vec![],
            shell("git status"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "mutating shell asks",
            vec![],
            shell("touch a"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "read inside workspace",
            vec![],
            read("/repo/src/a.rs"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "read in additional dir",
            vec![],
            read("/shared/x"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "read outside asks",
            vec![],
            read("/etc/passwd"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "read rule outside",
            vec![(A, User, "Read(//etc/**)")],
            read("/etc/passwd"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "home rule",
            vec![(A, User, "Read(~/notes/**)")],
            read("/home/me/notes/a"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "edit asks in default",
            vec![],
            edit("/repo/a"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "edit rule root-relative",
            vec![(A, Local, "Edit(/docs/**)")],
            edit("/repo/docs/x.md"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "Write rule covers edits",
            vec![(A, Local, "Write(/docs/**)")],
            edit("/repo/docs/x.md"),
            Mode::Default,
            Want::Allow,
        ),
        (
            "edit rule does not cover reads",
            vec![(D, Local, "Edit(//etc/**)")],
            read("/etc/hosts"),
            Mode::Default,
            Want::Ask,
        ),
        (
            "mcp asks on first use",
            vec![],
            mcp("github", "list", true),
            Mode::Default,
            Want::Ask,
        ),
        (
            "mcp server rule",
            vec![(A, User, "mcp__github")],
            mcp("github", "create_issue", false),
            Mode::Default,
            Want::Allow,
        ),
        (
            "mcp tool rule",
            vec![(A, User, "mcp__github__list")],
            mcp("github", "create_issue", false),
            Mode::Default,
            Want::Ask,
        ),
        (
            "webfetch pasted",
            vec![],
            fetch("example.com", true, false),
            Mode::Default,
            Want::Allow,
        ),
        (
            "webfetch other asks",
            vec![],
            fetch("example.com", false, false),
            Mode::Default,
            Want::Ask,
        ),
        (
            "webfetch domain rule",
            vec![(A, User, "WebFetch(domain:*.example.com)")],
            fetch("api.example.com", false, false),
            Mode::Default,
            Want::Allow,
        ),
        (
            "webfetch policy deny wins over pasted",
            vec![(D, Policy, "WebFetch(domain:evil.com)")],
            fetch("evil.com", true, false),
            Mode::Default,
            Want::Deny(Policy),
        ),
        (
            "websearch default allowed",
            vec![],
            Action::WebSearch,
            Mode::Default,
            Want::Allow,
        ),
        (
            "websearch policy deny",
            vec![(D, Policy, "WebSearch")],
            Action::WebSearch,
            Mode::Default,
            Want::Deny(Policy),
        ),
        (
            "browser first visit asks",
            vec![],
            Action::Browser {
                site: "example.com".into(),
                action: BrowserAction::Navigate,
            },
            Mode::Default,
            Want::Ask,
        ),
        (
            "browser site rule",
            vec![(A, User, "Browser(site:*.example.com)")],
            Action::Browser {
                site: "shop.example.com".into(),
                action: BrowserAction::Navigate,
            },
            Mode::Default,
            Want::Allow,
        ),
        (
            "browser action ask beats site allow",
            vec![
                (A, User, "Browser(site:example.com)"),
                (K, User, "Browser(action:submit)"),
            ],
            Action::Browser {
                site: "example.com".into(),
                action: BrowserAction::Act("submit".into()),
            },
            Mode::Default,
            Want::Ask,
        ),
        (
            "artifact network deny",
            vec![(D, Policy, "Artifact(network)")],
            Action::Other {
                tool: "Artifact".into(),
                specifier: Some("network".into()),
                read_only: true,
            },
            Mode::Default,
            Want::Deny(Policy),
        ),
        (
            "read-only other tool",
            vec![],
            Action::Other {
                tool: "memory_search".into(),
                specifier: None,
                read_only: true,
            },
            Mode::Default,
            Want::Allow,
        ),
    ];
    for (name, rs, action, mode, want) in cases {
        let ev = code_eval(mode, rules(&rs));
        let v = ev.evaluate(&Request::new(action));
        assert_eq!(kind(&v), want, "{name}: {v:?}");
    }
}

#[test]
fn shell_special_cases() {
    let ev = code_eval(
        Mode::Default,
        rules(&[(D, User, "Bash(rm:*)"), (A, User, "Bash")]),
    );
    // Unparseable: deny rules still see the raw text, otherwise ask (06 §13.3).
    let raw = |c: &str| {
        Request::new(Action::Shell {
            dialect: ShellDialect::Posix,
            command: c.into(),
            parts: None,
            cwd: Some(ROOT.into()),
        })
    };
    assert_eq!(kind(&ev.evaluate(&raw("rm -rf $(weird"))), Want::Deny(User));
    assert!(matches!(
        ev.evaluate(&raw("echo $(weird")),
        Verdict::Ask {
            reason: AskReason::Unparseable,
            ..
        }
    ));
    // cwd outside the workspace asks even with an allow rule.
    let outside = Request::new(Action::Shell {
        dialect: ShellDialect::Posix,
        command: "ls".into(),
        parts: Some(vec!["ls".into()]),
        cwd: Some("/tmp".into()),
    });
    assert!(matches!(
        ev.evaluate(&outside),
        Verdict::Ask {
            reason: AskReason::CwdOutside,
            ..
        }
    ));
    // PowerShell rules don't match bash and are case-insensitive.
    let ev = code_eval(
        Mode::Default,
        rules(&[(A, User, "PowerShell(Remove-Item:*)")]),
    );
    let ps = |c: &str, d| {
        Request::new(Action::Shell {
            dialect: d,
            command: c.into(),
            parts: Some(vec![c.into()]),
            cwd: None,
        })
    };
    assert_eq!(
        kind(&ev.evaluate(&ps("remove-item x", ShellDialect::PowerShell))),
        Want::Allow
    );
    assert_eq!(
        kind(&ev.evaluate(&ps("Remove-Item x", ShellDialect::Posix))),
        Want::Ask
    );
}

#[test]
fn modes() {
    let none: Vec<(RuleClass, Origin, &str)> = vec![];
    let mcp_allow = vec![(A, User, "mcp__db")];
    let cases: Vec<(&str, Mode, Vec<(RuleClass, Origin, &str)>, Action, Want)> = vec![
        (
            "default: edit asks",
            Mode::Default,
            none.clone(),
            edit("/repo/a"),
            Want::Ask,
        ),
        (
            "accept-edits: edit inside allowed",
            Mode::AcceptEdits,
            none.clone(),
            edit("/repo/a"),
            Want::Allow,
        ),
        (
            "accept-edits: edit outside asks",
            Mode::AcceptEdits,
            none.clone(),
            edit("/etc/a"),
            Want::Ask,
        ),
        (
            "accept-edits: shell follows rules",
            Mode::AcceptEdits,
            none.clone(),
            shell("touch a"),
            Want::Ask,
        ),
        (
            "accept-edits: mcp follows rules",
            Mode::AcceptEdits,
            none.clone(),
            mcp("db", "q", false),
            Want::Ask,
        ),
        (
            "plan: no shell even read-only",
            Mode::Plan,
            none.clone(),
            shell("ls"),
            Want::Deny(Engine),
        ),
        (
            "plan: no shell even with allow",
            Mode::Plan,
            vec![(A, User, "Bash")],
            shell("ls"),
            Want::Deny(Engine),
        ),
        (
            "plan: no edits",
            Mode::Plan,
            vec![(A, User, "Edit")],
            edit("/repo/a"),
            Want::Deny(Engine),
        ),
        (
            "plan: reads ok",
            Mode::Plan,
            none.clone(),
            read("/repo/a"),
            Want::Allow,
        ),
        (
            "plan: mcp without allow rule blocked",
            Mode::Plan,
            none.clone(),
            mcp("db", "q", true),
            Want::Deny(Engine),
        ),
        (
            "plan: mcp with allow rule ok",
            Mode::Plan,
            mcp_allow,
            mcp("db", "q", false),
            Want::Allow,
        ),
        (
            "plan: browser act blocked",
            Mode::Plan,
            vec![(A, User, "Browser")],
            Action::Browser {
                site: "a.com".into(),
                action: BrowserAction::Act("click".into()),
            },
            Want::Deny(Engine),
        ),
        (
            "plan: browser read follows rules",
            Mode::Plan,
            vec![(A, User, "Browser")],
            Action::Browser {
                site: "a.com".into(),
                action: BrowserAction::Read,
            },
            Want::Allow,
        ),
        (
            "plan: deny reported over plan block",
            Mode::Plan,
            vec![(D, User, "Bash")],
            shell("ls"),
            Want::Deny(User),
        ),
        (
            "bypass: everything else allowed",
            Mode::Bypass,
            none.clone(),
            edit("/etc/x"),
            Want::Allow,
        ),
        (
            "bypass: mcp allowed",
            Mode::Bypass,
            none,
            mcp("db", "q", false),
            Want::Allow,
        ),
    ];
    for (name, mode, rs, action, want) in cases {
        let ev = code_eval(mode, rules(&rs));
        assert_eq!(kind(&ev.evaluate(&Request::new(action))), want, "{name}");
    }

    // Chat sessions are always `default`, whatever the request says.
    let mut ctx = Context::chat();
    ctx.mode = Mode::Bypass;
    let ev = Evaluator::new(RuleSet::new(), ctx);
    assert_eq!(ev.mode(), Mode::Default);
    assert_eq!(
        kind(&ev.evaluate(&Request::new(mcp("a", "b", false)))),
        Want::Ask
    );
}

#[test]
fn policy_mode_floor() {
    let p = |disable_bypass, floor| ModePolicy {
        disable_bypass,
        floor,
    };
    let cases = [
        (Mode::Bypass, p(false, None), Mode::Bypass),
        (Mode::Bypass, p(true, None), Mode::Default),
        (Mode::AcceptEdits, p(true, None), Mode::AcceptEdits),
        (
            Mode::Bypass,
            p(false, Some(Mode::AcceptEdits)),
            Mode::AcceptEdits,
        ),
        (
            Mode::AcceptEdits,
            p(false, Some(Mode::Default)),
            Mode::Default,
        ),
        (Mode::Plan, p(true, Some(Mode::Default)), Mode::Plan),
        (Mode::Bypass, p(true, Some(Mode::Bypass)), Mode::Default),
        (Mode::Unknown, p(false, None), Mode::Default),
    ];
    for (req, pol, want) in cases {
        assert_eq!(
            effective_mode(SessionKind::Code, req, pol),
            want,
            "{req:?} {pol:?}"
        );
    }
    // A disabled bypass means edits ask again.
    let ev = code_eval(Mode::Bypass, RuleSet::new()).with_policy(p(true, None));
    assert_eq!(
        kind(&ev.evaluate(&Request::new(edit("/repo/a")))),
        Want::Ask
    );
}

#[test]
fn taint_downgrades_auto_allows() {
    let taint = Taint {
        source: "example.com".into(),
        read_origins: vec!["example.com".into()],
    };
    let rs = vec![
        (A, Local, "Bash(npm:*)"),
        (A, User, "Edit(//etc/**)"),
        (A, User, "mcp__db"),
        (A, User, "WebFetch(domain:*)"),
        (A, User, "Browser"),
    ];
    // (name, mode, action, want untainted, want tainted)
    let cases: Vec<(&str, Mode, Action, Want, Want)> = vec![
        (
            "shell by rule",
            Mode::Default,
            shell("npm i"),
            Want::Allow,
            Want::Ask,
        ),
        (
            "read-only shell stays",
            Mode::Default,
            shell("ls"),
            Want::Allow,
            Want::Allow,
        ),
        (
            "edit outside by rule",
            Mode::Default,
            edit("/etc/hosts"),
            Want::Allow,
            Want::Ask,
        ),
        (
            "edit inside by accept-edits stays",
            Mode::AcceptEdits,
            edit("/repo/a"),
            Want::Allow,
            Want::Allow,
        ),
        (
            "mcp not read-only",
            Mode::Default,
            mcp("db", "write", false),
            Want::Allow,
            Want::Ask,
        ),
        (
            "mcp annotated read-only stays",
            Mode::Default,
            mcp("db", "read", true),
            Want::Allow,
            Want::Allow,
        ),
        (
            "fetch new domain",
            Mode::Default,
            fetch("other.com", false, false),
            Want::Allow,
            Want::Ask,
        ),
        (
            "fetch seen domain stays",
            Mode::Default,
            fetch("other.com", false, true),
            Want::Allow,
            Want::Allow,
        ),
        (
            "fetch pasted stays",
            Mode::Default,
            fetch("other.com", true, false),
            Want::Allow,
            Want::Allow,
        ),
        (
            "browser act other origin",
            Mode::Default,
            Action::Browser {
                site: "bank.com".into(),
                action: BrowserAction::Act("click".into()),
            },
            Want::Allow,
            Want::Ask,
        ),
        (
            "browser act same origin stays",
            Mode::Default,
            Action::Browser {
                site: "example.com".into(),
                action: BrowserAction::Act("click".into()),
            },
            Want::Allow,
            Want::Allow,
        ),
        (
            "read stays",
            Mode::Default,
            read("/repo/a"),
            Want::Allow,
            Want::Allow,
        ),
        (
            "bypass adds no asks",
            Mode::Bypass,
            shell("rm x"),
            Want::Allow,
            Want::Allow,
        ),
    ];
    for (name, mode, action, clean, dirty) in cases {
        let ev = code_eval(mode, rules(&rs));
        let req = Request::new(action);
        assert_eq!(kind(&ev.evaluate(&req)), clean, "{name} (clean)");
        let v = ev.evaluate(&req.clone().tainted(taint.clone()));
        assert_eq!(kind(&v), dirty, "{name} (tainted)");
        if dirty == Want::Ask {
            assert!(
                matches!(v, Verdict::Ask { reason: AskReason::Tainted { ref source }, .. } if source == "example.com")
            );
        }
    }
}

#[test]
fn save_suggestions() {
    let ev = code_eval(Mode::Default, RuleSet::new());
    let sugg = |a: Action| match ev.evaluate(&Request::new(a)) {
        Verdict::Ask {
            save_suggestions, ..
        } => save_suggestions
            .into_iter()
            .map(|s| (s.rule, s.scope))
            .collect::<Vec<_>>(),
        v => panic!("expected ask, got {v:?}"),
    };
    let l = RuleScope::Local;
    assert_eq!(
        sugg(shell("git commit -m x")),
        vec![("Bash(git commit:*)".to_string(), l)]
    );
    assert_eq!(
        sugg(shell("npm run test && git push origin main && ls")),
        vec![
            ("Bash(npm run test:*)".to_string(), l),
            ("Bash(git push:*)".to_string(), l)
        ]
    );
    assert_eq!(
        sugg(edit("/repo/src/a.rs")),
        vec![("Edit(/src/**)".to_string(), l)]
    );
    assert_eq!(sugg(edit("/repo/a.rs")), vec![("Edit(/**)".to_string(), l)]);
    assert_eq!(
        sugg(read("/etc/ssl/x.pem")),
        vec![("Read(//etc/ssl/**)".to_string(), l)]
    );
    assert_eq!(
        sugg(fetch("Docs.rs", false, false)),
        vec![("WebFetch(domain:docs.rs)".to_string(), l)]
    );
    assert_eq!(
        sugg(mcp("gh", "create_issue", false)),
        vec![("mcp__gh__create_issue".to_string(), l)]
    );

    // Chat sessions save to the user scope.
    let chat = Evaluator::new(RuleSet::new(), Context::chat());
    match chat.evaluate(&Request::new(mcp("gh", "x", false))) {
        Verdict::Ask {
            save_suggestions, ..
        } => assert_eq!(save_suggestions[0].scope, RuleScope::User),
        v => panic!("{v:?}"),
    }
}

#[test]
fn always_saves_and_releases_pending() {
    let mut ev = code_eval(Mode::Default, RuleSet::new());
    let pending = [
        ("a1", Request::new(shell("git commit -m one"))),
        ("a2", Request::new(shell("git commit -m two"))),
        ("a3", Request::new(shell("git push"))),
        ("a4", Request::new(mcp("gh", "x", false))),
    ];
    let first = match ev.evaluate(&pending[0].1) {
        Verdict::Ask {
            save_suggestions, ..
        } => save_suggestions[0].clone(),
        v => panic!("{v:?}"),
    };
    assert_eq!(
        ev.remember(&first),
        Ok(SaveTarget::Settings(ConfigScope::Local))
    );
    let released = ev.released(pending.iter().map(|(k, r)| (k, r)));
    assert_eq!(released, vec!["a1", "a2"]);

    // Session scope stays in memory.
    let s = SaveSuggestion {
        rule: "mcp__gh__x".into(),
        scope: RuleScope::Session,
    };
    assert_eq!(ev.remember(&s), Ok(SaveTarget::Session));
    assert!(ev.evaluate(&pending[3].1).is_allow());

    // Local scope without a project falls back to the user settings file.
    let mut chat = Evaluator::new(RuleSet::new(), Context::chat());
    let s = SaveSuggestion {
        rule: "WebSearch".into(),
        scope: RuleScope::Local,
    };
    assert_eq!(
        chat.remember(&s),
        Ok(SaveTarget::Settings(ConfigScope::User))
    );

    // A saved allow never overrides a deny.
    let mut ev = code_eval(Mode::Default, rules(&[(D, Policy, "Bash(git push:*)")]));
    ev.remember(&SaveSuggestion {
        rule: "Bash(git push:*)".into(),
        scope: RuleScope::Local,
    })
    .unwrap();
    assert_eq!(kind(&ev.evaluate(&pending[2].1)), Want::Deny(Policy));
    assert_eq!(
        ev.evaluate(&pending[2].1)
            .blocked_message(Some("Synergech"))
            .as_deref(),
        Some("Blocked by Synergech policy (Bash(git push:*))")
    );
}

#[test]
fn windows_paths_are_case_insensitive() {
    let mut ctx = Context::code(r"C:\Work\Repo", Mode::Default);
    ctx.case_insensitive_paths = true;
    let rs = rules(&[
        (D, User, r"Edit(/Secrets/**)"),
        (A, User, r"Read(//C:\Tools\**)"),
    ]);
    let ev = Evaluator::new(rs, ctx);
    assert_eq!(
        kind(&ev.evaluate(&Request::new(edit(r"c:\work\repo\SECRETS\a.txt")))),
        Want::Deny(User)
    );
    assert_eq!(
        kind(&ev.evaluate(&Request::new(read(r"C:/tools/x/y.exe")))),
        Want::Allow
    );
    assert_eq!(
        kind(&ev.evaluate(&Request::new(read(r"C:\WORK\REPO\src\a.rs")))),
        Want::Allow
    );
}

#[test]
fn rule_set_management() {
    let mut rs = RuleSet::new();
    let errs = rs.add_list(A, User, &["Bash(ls:*)", "Bash(", "mcp__x", "Bash(ls:*)"]);
    assert_eq!(errs.len(), 1);
    assert_eq!(rs.len(), 2, "duplicates are folded");
    assert!(rs.remove(A, User, "mcp__x"));
    assert!(!rs.remove(A, Local, "Bash(ls:*)"));
    assert_eq!(rs.len(), 1);
}
