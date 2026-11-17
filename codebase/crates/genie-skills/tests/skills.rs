//! Catalogue, package and directory tests on fixture folders and generated zips.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use genie_protocol::api::mcp::{DirectoryInstallParams, DirectoryKind};
use genie_skills::catalog::FileKind;
use genie_skills::directory::{Feed, FeedEntry};
use genie_skills::scan::Severity;
use genie_skills::{
    DirectoryTransport, Invoker, JsonStateFile, MemoryState, SessionKind, SignedFeed, SkillCatalog,
    SkillDirectory, SkillError, SkillPolicy, SkillRoots, SkillSource, SkillStateStore,
};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

fn skill_md(name: &str, desc: &str, extra: &str) -> String {
    format!("---\nname: {name}\ndescription: {desc}\n{extra}---\n# {name}\nDo the {name} thing.\n")
}

fn write(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    ws: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home/.genie");
    let ws = tmp.path().join("ws");
    let us = home.join("skills");
    write(
        &us.join("pdf/SKILL.md"),
        &skill_md("pdf", "Work with PDF files.", "allowed-tools: Read\n"),
    );
    write(&us.join("pdf/references/forms.md"), "Forms guide");
    write(&us.join("pdf/scripts/fill.py"), "print(1)");
    write(&us.join("pdf/.secret"), "nope");
    write(
        &us.join("category/nested-one/SKILL.md"),
        &skill_md("nested-one", "Nested skill.", ""),
    );
    write(
        &us.join("a/b/c/too-deep/SKILL.md"),
        &skill_md("too-deep", "Too deep.", ""),
    );
    write(
        &us.join("manual/SKILL.md"),
        &skill_md("manual", "User only.", "disable-model-invocation: true\n"),
    );
    write(&us.join("broken/SKILL.md"), "---\nname: broken\n---\n");
    write(&us.join("Bad_Name/SKILL.md"), &skill_md("bad", "x", ""));
    write(
        &us.join("renamed/SKILL.md"),
        &skill_md("other-name", "Name differs.", ""),
    );
    write(
        &us.join("review-document/SKILL.md"),
        &skill_md("review-document", "My own review.", ""),
    );
    write(
        &ws.join(".genie/skills/pdf/SKILL.md"),
        &skill_md("pdf", "Project PDF skill.", ""),
    );
    write(
        &ws.join(".agents/skills/proj/SKILL.md"),
        &skill_md("proj", "Project skill.", ""),
    );
    write(
        &ws.join(".claude/skills/claude-only/SKILL.md"),
        &skill_md("claude-only", "Never scanned.", ""),
    );
    Fixture {
        _tmp: tmp,
        home,
        ws,
    }
}

fn catalog(f: &Fixture, trusted: bool) -> SkillCatalog {
    SkillCatalog::new(
        SkillRoots::standard(&f.home, Some(&f.ws), trusted),
        SkillPolicy::default(),
        Arc::new(MemoryState::default()),
    )
}

fn ids(c: &SkillCatalog) -> Vec<String> {
    c.list().skills.into_iter().map(|s| s.id).collect()
}

#[test]
fn discovery_precedence_and_trust() {
    let f = fixture();
    let c = catalog(&f, false);
    assert_eq!(
        ids(&c),
        [
            "create-skill",
            "manual",
            "nested-one",
            "pdf",
            "renamed",
            "review-document",
            "summarise-meeting",
            "write-spec"
        ]
    );
    let info = c.list().skills;
    let pdf = info.iter().find(|s| s.id == "pdf").unwrap();
    assert_eq!((pdf.source.as_str(), pdf.enabled), ("user", true));
    let review = info.iter().find(|s| s.id == "review-document").unwrap();
    assert_eq!(review.source, "user", "user overrides built-in");
    assert_eq!(
        info.iter().find(|s| s.id == "write-spec").unwrap().source,
        "builtin"
    );

    let d = c.diagnostics();
    assert!(
        d.iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("description"))
    );
    assert!(d.iter().any(|d| d.message.contains("Bad_Name")));
    assert!(
        d.iter()
            .any(|d| d.severity == Severity::Warning && d.message.contains("other-name"))
    );
    assert!(
        d.iter()
            .any(|d| d.message.contains("overrides the builtin"))
    );

    let t = catalog(&f, true);
    let info = t.list().skills;
    assert_eq!(
        info.iter().find(|s| s.id == "pdf").unwrap().source,
        "project"
    );
    assert_eq!(
        info.iter().find(|s| s.id == "pdf").unwrap().description,
        "Project PDF skill."
    );
    assert!(info.iter().any(|s| s.id == "proj"));
    assert!(
        !info
            .iter()
            .any(|s| s.id == "claude-only" || s.id == "too-deep")
    );
}

#[test]
fn index_enable_disable_and_state() {
    let f = fixture();
    let state_path = f.home.join("skills-state.json");
    let state: Arc<dyn SkillStateStore> = Arc::new(JsonStateFile::new(&state_path));
    let mut c = SkillCatalog::new(
        SkillRoots::standard(&f.home, None, false),
        SkillPolicy::default(),
        state,
    );
    let index: Vec<String> = c.index().into_iter().map(|(id, _)| id).collect();
    assert!(index.contains(&"pdf".to_string()));
    assert!(
        !index.contains(&"manual".to_string()),
        "user-only skills stay out of the index"
    );
    assert!(c.user_invocable().iter().any(|(id, _)| id == "manual"));

    c.set_enabled("pdf", false).unwrap();
    assert!(!c.index().iter().any(|(id, _)| id == "pdf"));
    assert!(
        !c.list()
            .skills
            .iter()
            .find(|s| s.id == "pdf")
            .unwrap()
            .enabled
    );
    assert!(matches!(
        c.load("pdf", Invoker::User, SessionKind::Chat),
        Err(SkillError::Disabled(_))
    ));
    assert!(matches!(
        c.set_enabled("nope", false),
        Err(SkillError::NotFound(_))
    ));

    // Persisted: a new catalogue sees the same state.
    let c2 = SkillCatalog::new(
        SkillRoots::standard(&f.home, None, false),
        SkillPolicy::default(),
        Arc::new(JsonStateFile::new(&state_path)),
    );
    assert!(!c2.is_enabled("pdf"));
    c.set_enabled("pdf", true).unwrap();
    assert!(JsonStateFile::new(&state_path).disabled().is_empty());
}

#[test]
fn load_and_read_files() {
    let f = fixture();
    let c = catalog(&f, false);
    let l = c.load("pdf", Invoker::Model, SessionKind::Chat).unwrap();
    assert_eq!(l.body.trim(), "# pdf\nDo the pdf thing.");
    assert_eq!(l.allowed_tools, vec!["Read"]);
    let files: Vec<_> = l.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
    assert_eq!(
        files,
        [
            ("references/forms.md", FileKind::Reference),
            ("scripts/fill.py", FileKind::Script)
        ]
    );
    let r = l.render();
    assert!(r.starts_with("<skill name=\"pdf\" base_path=\""));
    assert!(r.contains("scripts/fill.py (8 bytes) [script: runs in Code mode]"));
    assert!(!r.contains(".secret"));
    let code = c
        .load("pdf", Invoker::Model, SessionKind::Code)
        .unwrap()
        .render();
    assert!(code.contains("[script: run with the shell tool]"));

    // Body is read on use: edits after the scan show up.
    write(
        &f.home.join("skills/pdf/SKILL.md"),
        &skill_md("pdf", "Work with PDF files.", "").replace("Do the", "Now do the"),
    );
    assert!(
        c.load("pdf", Invoker::Model, SessionKind::Chat)
            .unwrap()
            .body
            .contains("Now do the")
    );

    assert!(matches!(
        c.load("manual", Invoker::Model, SessionKind::Chat),
        Err(SkillError::UserOnly(_))
    ));
    assert!(c.load("manual", Invoker::User, SessionKind::Chat).is_ok());
    assert!(matches!(
        c.load("ghost", Invoker::User, SessionKind::Chat),
        Err(SkillError::NotFound(_))
    ));

    let b = c
        .load("write-spec", Invoker::Model, SessionKind::Chat)
        .unwrap();
    assert_eq!(b.base_path, "builtin:write-spec");
    assert!(b.files.is_empty());
    assert!(
        c.read_file("write-spec", "SKILL.md", Invoker::Model)
            .is_ok()
    );

    assert_eq!(
        c.read_file("pdf", "references/forms.md", Invoker::Model)
            .unwrap()
            .content,
        "Forms guide"
    );
    for bad in [
        "../manual/SKILL.md",
        "/etc/passwd",
        ".secret",
        "references/../../manual/SKILL.md",
    ] {
        assert!(
            matches!(
                c.read_file("pdf", bad, Invoker::Model),
                Err(SkillError::PathEscape(_))
            ),
            "{bad}"
        );
    }
    assert!(c.read_file("pdf", "references", Invoker::Model).is_err());
    assert!(c.read_file("pdf", "missing.md", Invoker::Model).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_out_of_skill_is_refused() {
    let f = fixture();
    let outside = f.home.join("outside.txt");
    write(&outside, "secret");
    std::os::unix::fs::symlink(&outside, f.home.join("skills/pdf/references/link.md")).unwrap();
    let c = catalog(&f, false);
    assert!(matches!(
        c.read_file("pdf", "references/link.md", Invoker::Model),
        Err(SkillError::PathEscape(_))
    ));
}

#[test]
fn large_and_binary_files() {
    let f = fixture();
    let big = "é".repeat(genie_skills::MAX_READ as usize); // 2 bytes each → cut mid-char
    write(&f.home.join("skills/pdf/references/big.md"), &big);
    fs::create_dir_all(f.home.join("skills/pdf/assets")).unwrap();
    fs::write(
        f.home.join("skills/pdf/assets/logo.bin"),
        [0xff, 0xfe, 0x00],
    )
    .unwrap();
    let c = catalog(&f, false);
    let r = c
        .read_file("pdf", "references/big.md", Invoker::Model)
        .unwrap();
    assert!(r.truncated);
    assert_eq!(r.content.len(), genie_skills::MAX_READ as usize);
    assert!(
        c.read_file("pdf", "assets/logo.bin", Invoker::Model)
            .is_err()
    );
}

#[test]
fn policy() {
    let f = fixture();
    let mut c = catalog(&f, false);
    c.set_policy(SkillPolicy {
        allowed: Some(vec!["pdf".into(), "write-spec".into()]),
        ..Default::default()
    });
    assert_eq!(ids(&c), ["pdf", "write-spec"]);
    assert!(matches!(
        c.load("manual", Invoker::User, SessionKind::Chat),
        Err(SkillError::Blocked(_))
    ));
    c.set_policy(SkillPolicy {
        directory_only: true,
        ..Default::default()
    });
    // Built-ins only; the user's `review-document` override is not a directory skill.
    assert_eq!(ids(&c), ["create-skill", "summarise-meeting", "write-spec"]);
    assert!(matches!(
        c.upload(
            &zip_of(&[("SKILL.md", skill_md("x", "y", "").as_bytes())]),
            false
        ),
        Err(SkillError::Policy(_))
    ));
}

#[test]
fn hot_reload_fingerprint() {
    let f = fixture();
    let mut c = catalog(&f, false);
    assert!(!c.refresh());
    write(
        &f.home.join("skills/fresh/SKILL.md"),
        &skill_md("fresh", "New.", ""),
    );
    assert!(c.refresh());
    assert!(c.get("fresh").is_some());
    assert!(!c.refresh());
    // Editing a description is picked up (size changes).
    write(
        &f.home.join("skills/fresh/SKILL.md"),
        &skill_md("fresh", "New and improved.", ""),
    );
    assert!(c.refresh());
    assert_eq!(
        c.get("fresh").unwrap().manifest.description,
        "New and improved."
    );
    // Nested category folders are watched too.
    write(
        &f.home.join("skills/category/two/SKILL.md"),
        &skill_md("two", "Two.", ""),
    );
    assert!(c.refresh());
    assert!(c.get("two").is_some());
}

// ---- packages ----

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn empty_catalog() -> (tempfile::TempDir, SkillCatalog) {
    let tmp = tempfile::tempdir().unwrap();
    let c = SkillCatalog::new(
        SkillRoots::standard(tmp.path(), None, false),
        SkillPolicy::default(),
        Arc::new(MemoryState::default()),
    );
    (tmp, c)
}

#[test]
fn upload_root_and_folder_layouts() {
    let (tmp, mut c) = empty_catalog();
    let md = skill_md("zipped", "From a zip.", "");
    let z = zip_of(&[
        ("SKILL.md", md.as_bytes()),
        ("references/a.md", b"A"),
        (".DS_Store", b"junk"),
        ("__MACOSX/._SKILL.md", b"junk"),
    ]);
    let inst = c.upload(&z, false).unwrap();
    assert_eq!(inst.id, "zipped");
    assert_eq!(inst.files, ["SKILL.md", "references/a.md"]);
    assert_eq!(inst.sha256, hex::encode(Sha256::digest(&z)));
    assert!(tmp.path().join("skills/zipped/references/a.md").is_file());
    assert!(!tmp.path().join("skills/zipped/.DS_Store").exists());
    let info = c
        .list()
        .skills
        .into_iter()
        .find(|s| s.id == "zipped")
        .unwrap();
    assert_eq!(info.source, "upload");
    assert_eq!(c.get("zipped").unwrap().source, SkillSource::Upload);
    assert!(
        c.load("zipped", Invoker::Model, SessionKind::Chat)
            .unwrap()
            .files
            .iter()
            .all(|f| !f.path.starts_with('.'))
    );

    assert!(matches!(
        c.upload(&z, false),
        Err(SkillError::AlreadyExists(_))
    ));
    let md2 = skill_md("zipped", "Updated.", "");
    let z2 = zip_of(&[
        ("zipped/SKILL.md", md2.as_bytes()),
        ("zipped/scripts/x.sh", b"echo"),
    ]);
    let inst = c.upload(&z2, true).unwrap();
    assert!(inst.replaced);
    assert_eq!(c.get("zipped").unwrap().manifest.description, "Updated.");
    assert!(
        !tmp.path().join("skills/zipped/references").exists(),
        "replace is a full swap"
    );
    // No leftovers from the temp/swap folders.
    let names: Vec<_> = fs::read_dir(tmp.path().join("skills"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, ["zipped"]);

    c.uninstall("zipped").unwrap();
    assert!(c.get("zipped").is_none());
    assert!(matches!(
        c.uninstall("write-spec"),
        Err(SkillError::Policy(_))
    ));
}

fn upload_err(files: &[(&str, &[u8])]) -> SkillError {
    let (_tmp, mut c) = empty_catalog();
    c.upload(&zip_of(files), false).unwrap_err()
}

#[test]
fn upload_rejections() {
    let md = skill_md("ok", "Fine.", "");
    let md = md.as_bytes();
    assert!(
        matches!(upload_err(&[("README.md", b"x")]), SkillError::Package(m) if m.contains("SKILL.md"))
    );
    assert!(matches!(
        upload_err(&[("a/SKILL.md", md), ("b/x.md", b"x")]),
        SkillError::Package(_)
    ));
    assert!(
        matches!(upload_err(&[("SKILL.md", md), ("../evil.md", b"x")]), SkillError::Package(m) if m.contains("escapes"))
    );
    assert!(matches!(
        upload_err(&[("SKILL.md", md), ("a/../../evil.md", b"x")]),
        SkillError::Package(_)
    ));
    assert!(matches!(
        upload_err(&[("SKILL.md", md), ("/abs.md", b"x")]),
        SkillError::Package(_)
    ));
    assert!(matches!(
        upload_err(&[("SKILL.md", md), ("a\\..\\b.md", b"x")]),
        SkillError::Package(_)
    ));
    assert!(matches!(
        upload_err(&[("SKILL.md", md), ("C:/x.md", b"x")]),
        SkillError::Package(_)
    ));
    assert!(matches!(
        upload_err(&[("SKILL.md", b"---\nname: Bad\n---\n")]),
        SkillError::Invalid(_)
    ));
    assert!(matches!(
        upload_err(&[("SKILL.md", b"no frontmatter")]),
        SkillError::Invalid(_)
    ));
    let (_t, mut c) = empty_catalog();
    assert!(matches!(
        c.upload(b"not a zip", false),
        Err(SkillError::Package(_))
    ));

    // Symlink entry.
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    w.start_file("SKILL.md", SimpleFileOptions::default())
        .unwrap();
    w.write_all(md).unwrap();
    w.add_symlink("link", "/etc/passwd", SimpleFileOptions::default())
        .unwrap();
    let z = w.finish().unwrap().into_inner();
    assert!(matches!(c.upload(&z, false), Err(SkillError::Package(m)) if m.contains("symbolic")));
}

#[test]
fn zip_bomb_and_limits() {
    let md = skill_md("bomb", "Boom.", "");
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let z = zip_of(&[("SKILL.md", md.as_bytes()), ("assets/zeros.bin", &zeros)]);
    assert!(z.len() < 100 * 1024, "compresses well");
    let (_t, mut c) = empty_catalog();
    assert!(matches!(c.upload(&z, false), Err(SkillError::Package(m)) if m.contains("ratio")));

    let root = tempfile::tempdir().unwrap();
    let small = genie_skills::ImportOptions {
        limits: genie_skills::ImportLimits {
            max_file: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    let z = zip_of(&[("SKILL.md", md.as_bytes())]);
    assert!(matches!(
        genie_skills::package::import(&z, root.path(), &small, &|_| true),
        Err(SkillError::TooLarge { .. })
    ));
    let few = genie_skills::ImportOptions {
        limits: genie_skills::ImportLimits {
            max_entries: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let z = zip_of(&[("SKILL.md", md.as_bytes()), ("a", b"1"), ("b", b"2")]);
    assert!(genie_skills::package::import(&z, root.path(), &few, &|_| true).is_err());
    let tiny = genie_skills::ImportOptions {
        limits: genie_skills::ImportLimits {
            max_archive: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(matches!(
        genie_skills::package::import(&z, root.path(), &tiny, &|_| true),
        Err(SkillError::TooLarge { .. })
    ));
    assert!(
        fs::read_dir(root.path()).unwrap().next().is_none(),
        "nothing written on failure"
    );
}

#[test]
fn upload_respects_allowlist() {
    let (_t, mut c) = empty_catalog();
    c.set_policy(SkillPolicy {
        allowed: Some(vec!["other".into()]),
        ..Default::default()
    });
    let md = skill_md("nope", "x.", "");
    assert!(matches!(
        c.upload(&zip_of(&[("SKILL.md", md.as_bytes())]), false),
        Err(SkillError::Blocked(_))
    ));
}

// ---- directory ----

struct FakeTransport {
    feed: SignedFeed,
    package: Vec<u8>,
}

#[async_trait]
impl DirectoryTransport for FakeTransport {
    async fn fetch_feed(&self) -> Result<SignedFeed, SkillError> {
        Ok(self.feed.clone())
    }
    async fn fetch_package(&self, url: &str) -> Result<Vec<u8>, SkillError> {
        assert_eq!(url, "https://dir.example/dirskill-1.0.0.zip");
        Ok(self.package.clone())
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

fn signed(feed: &Feed, k: &SigningKey) -> SignedFeed {
    let body = serde_json::to_vec(feed).unwrap();
    SignedFeed {
        signature_b64: base64::engine::general_purpose::STANDARD.encode(k.sign(&body).to_bytes()),
        body,
    }
}

fn dir_setup(
    tamper: impl FnOnce(&mut FeedEntry, &mut Vec<u8>),
) -> (SkillDirectory<FakeTransport>, Vec<u8>) {
    let md = skill_md("dirskill", "From the directory.", "");
    let mut package = zip_of(&[("SKILL.md", md.as_bytes()), ("scripts/run.py", b"print(1)")]);
    let mut entry = FeedEntry {
        id: "dirskill".into(),
        version: "1.0.0".into(),
        name: "Dir skill".into(),
        description: "From the directory.".into(),
        url: "https://dir.example/dirskill-1.0.0.zip".into(),
        sha256: hex::encode(Sha256::digest(&package)),
        size: package.len() as u64,
        files: vec!["SKILL.md".into(), "scripts/run.py".into()],
        publisher: Some("Genie".into()),
        allowed_tools: vec![],
    };
    tamper(&mut entry, &mut package);
    let feed = Feed {
        version: 1,
        entries: vec![entry],
    };
    let t = FakeTransport {
        feed: signed(&feed, &key()),
        package: package.clone(),
    };
    (
        SkillDirectory::with_keys(t, vec![key().verifying_key()]),
        package,
    )
}

fn params(confirm: bool) -> DirectoryInstallParams {
    DirectoryInstallParams {
        kind: DirectoryKind::Skills,
        id: "dirskill".into(),
        version: "1.0.0".into(),
        confirm,
    }
}

#[tokio::test]
async fn directory_list_and_install() {
    let (dir, _) = dir_setup(|_, _| {});
    let l = dir.list(Some("DIRECTORY")).await.unwrap();
    assert_eq!(l.entries.len(), 1);
    assert_eq!(
        l.entries[0].extra["scripts"],
        serde_json::json!(["scripts/run.py"])
    );
    assert!(dir.list(Some("nothing")).await.unwrap().entries.is_empty());

    let (_t, mut c) = empty_catalog();
    let first = dir.install(&mut c, &params(false)).await.unwrap();
    assert!(!first.installed);
    assert_eq!(
        first.manifest["files"],
        serde_json::json!(["SKILL.md", "scripts/run.py"])
    );
    assert!(
        c.get("dirskill").is_none(),
        "nothing installed before confirm"
    );

    let second = dir.install(&mut c, &params(true)).await.unwrap();
    assert!(second.installed);
    assert_eq!(c.get("dirskill").unwrap().source, SkillSource::Directory);
    assert_eq!(
        c.get("dirskill")
            .unwrap()
            .install
            .as_ref()
            .unwrap()
            .version
            .as_deref(),
        Some("1.0.0")
    );

    // Directory installs still work under directory-only policy; uploads don't.
    c.set_policy(SkillPolicy {
        directory_only: true,
        ..Default::default()
    });
    assert!(c.get("dirskill").is_some());
    assert!(
        dir.install(&mut c, &params(true)).await.unwrap().installed,
        "reinstall replaces"
    );

    let mut wrong = params(true);
    wrong.version = "2.0.0".into();
    assert!(matches!(
        dir.install(&mut c, &wrong).await,
        Err(SkillError::Directory(_))
    ));
}

#[tokio::test]
async fn directory_rejects_tampering() {
    // Wrong key.
    let other = SkillDirectory::with_keys(
        FakeTransport {
            feed: signed(
                &Feed {
                    version: 1,
                    entries: vec![],
                },
                &SigningKey::from_bytes(&[1u8; 32]),
            ),
            package: vec![],
        },
        vec![key().verifying_key()],
    );
    assert!(
        matches!(other.list(None).await, Err(SkillError::Directory(m)) if m.contains("signature"))
    );
    let nokeys = SkillDirectory::new(FakeTransport {
        feed: SignedFeed {
            body: b"{}".to_vec(),
            signature_b64: String::new(),
        },
        package: vec![],
    });
    assert!(
        nokeys.list(None).await.is_err(),
        "no pinned keys yet → refuse"
    );

    // Package swapped after signing.
    let (dir, _) = dir_setup(|_, pkg| {
        *pkg = zip_of(&[("SKILL.md", skill_md("dirskill", "Evil.", "").as_bytes())]);
    });
    let (_t, mut c) = empty_catalog();
    assert!(matches!(
        dir.install(&mut c, &params(true)).await,
        Err(SkillError::Directory(_))
    ));

    // Checksum matches but the file list differs from the manifest.
    let (dir, _) = dir_setup(|e, _| e.files = vec!["SKILL.md".into()]);
    assert!(
        matches!(dir.install(&mut c, &params(true)).await, Err(SkillError::Package(m)) if m.contains("manifest"))
    );

    // Package's skill name differs from the entry id.
    let (dir, _) = dir_setup(|e, pkg| {
        *pkg = zip_of(&[("SKILL.md", skill_md("sneaky", "x.", "").as_bytes())]);
        e.sha256 = hex::encode(Sha256::digest(&*pkg));
        e.size = pkg.len() as u64;
        e.files = vec!["SKILL.md".into()];
    });
    assert!(
        matches!(dir.install(&mut c, &params(true)).await, Err(SkillError::Directory(m)) if m.contains("sneaky"))
    );
    assert!(c.get("dirskill").is_none() && c.get("sneaky").is_none());

    // Allowlist blocks before anything is fetched.
    c.set_policy(SkillPolicy {
        allowed: Some(vec![]),
        ..Default::default()
    });
    let (dir, _) = dir_setup(|_, _| {});
    assert!(matches!(
        dir.install(&mut c, &params(false)).await,
        Err(SkillError::Blocked(_))
    ));
}
