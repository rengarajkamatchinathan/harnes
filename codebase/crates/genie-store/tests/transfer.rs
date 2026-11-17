//! Export all (SA-17) and session import with id remapping (04 §9).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::io::Read;

use genie_store::crypto::Key;
use genie_store::messages::{self, Message, Part};
use genie_store::sessions::{self, NewSession};
use genie_store::{Store, StoreOptions, artifacts, export, memory, projects};
use serde_json::{Value, json};

fn open(dir: &tempfile::TempDir) -> Store {
    Store::open(
        &StoreOptions::new(dir.path(), "genie.db"),
        Key::generate().unwrap(),
    )
    .unwrap()
}

/// A session with a branch (root → a, root → b), a blob part and an artifact.
fn seed(store: &Store) -> (String, String) {
    let blob = store.put_blob(b"PNGDATA", Some("image/png")).unwrap();
    let pid = store
        .write(|tx| projects::create(tx, "Proj", Some("be brief")))
        .unwrap()
        .id;
    let sid = store
        .write(|tx| {
            let s = sessions::create(
                tx,
                &NewSession {
                    title: Some("Round trip".into()),
                    project_id: Some(pid.clone()),
                    ..Default::default()
                },
            )?;
            let root = Message::new(&s.id, None, "user");
            messages::insert(tx, &root)?;
            messages::insert_part(tx, &Part::text(&root.id, 0, "hello"))?;
            let mut img = Part::new(&root.id, 1, "media", json!({"mime": "image/png"}));
            img.blob_id = Some(blob.clone());
            messages::insert_part(tx, &img)?;
            let a = Message::new(&s.id, Some(&root.id), "assistant");
            messages::insert(tx, &a)?;
            messages::insert_part(tx, &Part::text(&a.id, 0, "branch A"))?;
            let b = Message::new(&s.id, Some(&root.id), "assistant");
            messages::insert(tx, &b)?;
            messages::insert_part(tx, &Part::text(&b.id, 0, "branch B"))?;
            sessions::set_active_leaf(tx, &s.id, Some(&b.id))?;
            let art = artifacts::create(tx, &s.id, "document", Some("Notes"))?;
            artifacts::add_version(tx, &art.id, &b.id, Some("# v1"), None)?;
            memory::add(tx, memory::GLOBAL, "likes tea", Some(&s.id), None)?;
            Ok(s.id)
        })
        .unwrap();
    (sid, blob)
}

#[test]
fn export_all_writes_the_sa17_layout() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let (sid, blob) = seed(&store);
    let path = dir.path().join("all.zip");
    let counts = store
        .export_all(std::fs::File::create(&path).unwrap(), true)
        .unwrap();
    assert_eq!(
        (counts.sessions, counts.projects, counts.memories, counts.blobs, counts.artifacts),
        (1, 1, 1, 1, 1)
    );
    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let names: Vec<String> = z.file_names().map(String::from).collect();
    let mut read = |name: &str| {
        let mut s = Vec::new();
        z.by_name(name).unwrap().read_to_end(&mut s).unwrap();
        s
    };
    let manifest: Value = serde_json::from_slice(&read("manifest.json")).unwrap();
    assert_eq!(manifest["format"], "genie-export");
    assert_eq!(manifest["counts"]["sessions"], 1);
    let sess: Value = serde_json::from_slice(&read(&format!("sessions/{sid}.json"))).unwrap();
    assert_eq!(sess["info"]["title"], "Round trip");
    assert_eq!(read(&format!("blobs/{blob}")), b"PNGDATA");
    let mems: Value = serde_json::from_slice(&read("memory.json")).unwrap();
    assert_eq!(mems[0]["text"], "likes tea");
    assert!(names.iter().any(|n| n.starts_with("projects/")));
    let art = names
        .iter()
        .find(|n| n.starts_with("artifacts/") && n.ends_with("/v1.md"))
        .cloned()
        .expect("artifact version");
    assert_eq!(read(&art), b"# v1");

    // Without blobs: none in the zip.
    let p2 = dir.path().join("noblobs.zip");
    let c2 = store
        .export_all(std::fs::File::create(&p2).unwrap(), false)
        .unwrap();
    assert_eq!(c2.blobs, 0);
}

#[test]
fn import_round_trip_keeps_ids_then_remaps_on_conflict() {
    let src_dir = tempfile::tempdir().unwrap();
    let src = open(&src_dir);
    let (sid, blob) = seed(&src);
    let json = src.read(|c| export::session_json(c, &sid, false)).unwrap();

    // Into an empty store: ids kept; the blob reference is dropped (no such blob here).
    let dst_dir = tempfile::tempdir().unwrap();
    let dst = open(&dst_dir);
    let first = dst.import_session(&json).unwrap();
    assert_eq!(first.session_id, sid);
    assert!(!first.remapped);
    assert_eq!(first.messages, 3);
    let back = dst.read(|c| export::session_json(c, &sid, false)).unwrap();
    assert_eq!(back["info"]["title"], "Round trip");
    assert_eq!(back["info"]["project_id"], Value::Null, "project not here");
    assert_eq!(back["messages"], json["messages"].clone().as_array().map(|ms| {
        Value::Array(
            ms.iter()
                .map(|m| {
                    let mut m = m.clone();
                    m["seq"] = Value::Null;
                    m
                })
                .collect(),
        )
    }).unwrap());
    assert!(back["parts"].as_array().unwrap().iter().all(|p| p["blob_id"].is_null()));
    let path = dst.read(|c| messages::active_path(c, &sid)).unwrap();
    assert_eq!(path.len(), 2, "root → branch B");
    let leaf_text = dst
        .read(|c| messages::parts(c, &path[1].id))
        .unwrap()
        .remove(0)
        .body;
    assert_eq!(leaf_text["text"], "branch B");
    assert_eq!(back["artifacts"][0]["versions"][0]["content"], "# v1");

    // Again into the same store: every id is taken → new ids, references rewritten.
    let second = dst.import_session(&json).unwrap();
    assert!(second.remapped);
    assert_ne!(second.session_id, sid);
    let again = dst
        .read(|c| export::session_json(c, &second.session_id, false))
        .unwrap();
    let msgs = again["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    let old_ids: Vec<&Value> = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["id"])
        .collect();
    assert!(msgs.iter().all(|m| !old_ids.contains(&&m["id"])));
    assert!(msgs.iter().all(|m| m["session_id"] == second.session_id.as_str()));
    let path2 = dst
        .read(|c| messages::active_path(c, &second.session_id))
        .unwrap();
    assert_eq!(path2.len(), 2);
    assert_eq!(
        again["artifacts"][0]["versions"][0]["message_id"],
        path2[1].id.as_str()
    );

    // Into the source store (blob present): the blob reference survives.
    let third = src.import_session(&json).unwrap();
    assert!(third.remapped);
    let t = src
        .read(|c| export::session_json(c, &third.session_id, false))
        .unwrap();
    assert!(t["parts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["blob_id"] == blob.as_str()));

    // A sanitized export imports without blobs or redacted artifact content.
    let clean = src.read(|c| export::session_json(c, &sid, true)).unwrap();
    let fourth = dst.import_session(&clean).unwrap();
    let f = dst
        .read(|c| export::session_json(c, &fourth.session_id, false))
        .unwrap();
    assert!(f["artifacts"][0]["versions"].as_array().unwrap().is_empty());

    // Garbage is refused without writing anything.
    let before = dst.read(|c| sessions::list(c, &Default::default())).unwrap().len();
    assert!(dst.import_session(&json!({"info": 3})).is_err());
    assert_eq!(
        dst.read(|c| sessions::list(c, &Default::default())).unwrap().len(),
        before
    );
}
