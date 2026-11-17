//! genie-store against a real SQLCipher DB in a temp dir (spec/04-storage.md).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use genie_protocol::session::{Delivery, TurnFinish};
use genie_store::crypto::Key;
use genie_store::events::{self, InboxState};
use genie_store::messages::{self, Message, Part, WireRecord};
use genie_store::search::{self, Scope};
use genie_store::sessions::{self, NewSession, SessionStatus};
use genie_store::util::{new_id, now_ms};
use genie_store::{
    Connection, KeyStore, MemoryKeyStore, Store, StoreError, StoreOptions, artifacts, export, kv,
    memory, migrate, projects, retrieval,
};
use serde_json::json;

fn open(dir: &tempfile::TempDir) -> (Store, Key) {
    let key = Key::generate().unwrap();
    let store = Store::open(&StoreOptions::new(dir.path(), "genie.db"), key.clone()).unwrap();
    (store, key)
}

fn new_session(store: &Store, title: &str) -> String {
    store
        .write(|tx| {
            sessions::create(
                tx,
                &NewSession {
                    title: Some(title.into()),
                    ..Default::default()
                },
            )
        })
        .unwrap()
        .id
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn db_is_encrypted_and_needs_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let (store, key) = open(&dir);
    assert_eq!(store.migration(), &migrate::Outcome::Bootstrapped);
    let id = new_session(&store, "secret plans");
    let path = store.db_path().to_path_buf();
    drop(store);

    let raw = std::fs::read(&path).unwrap();
    assert!(!raw.starts_with(b"SQLite format 3"), "header is encrypted");
    assert!(!raw.windows(12).any(|w| w == b"secret plans"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let opts = StoreOptions::new(dir.path(), "genie.db");
    let reopened = Store::open(&opts, key).unwrap();
    assert_eq!(reopened.migration(), &migrate::Outcome::UpToDate);
    let s = reopened.read(|c| sessions::get(c, &id)).unwrap().unwrap();
    assert_eq!(s.title.as_deref(), Some("secret plans"));
    drop(reopened);

    let err = Store::open(&opts, Key::generate().unwrap()).unwrap_err();
    assert!(matches!(err, StoreError::WrongKey(_)), "{err}");
}

#[test]
fn keystore_first_run_and_key_loss() {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions::new(dir.path(), "genie.db");
    let ks = MemoryKeyStore::new();
    let store = Store::open_with_keystore(&opts, &ks).unwrap();
    new_session(&store, "a");
    drop(store);
    // Same keychain: opens.
    drop(Store::open_with_keystore(&opts, &ks).unwrap());
    // Keychain lost: reported, nothing overwritten.
    let empty = MemoryKeyStore::new();
    let err = Store::open_with_keystore(&opts, &empty).unwrap_err();
    assert!(matches!(err, StoreError::KeyMissing { .. }));
    assert!(dir.path().join("genie.db").exists());
}

#[test]
fn newer_schema_is_refused_and_backup_is_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let (store, key) = open(&dir);
    let sid = new_session(&store, "kept");
    let bak = dir.path().join("genie.db.bak-1");
    store.backup_to(&bak).unwrap();
    store
        .write(|tx| {
            tx.execute("INSERT INTO migration VALUES (9999, 0)", [])?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let err = Store::open(&StoreOptions::new(dir.path(), "genie.db"), key.clone()).unwrap_err();
    assert!(err.to_string().contains("Update Genie"), "{err}");

    // The backup is a SQLCipher DB under the same key.
    assert!(!std::fs::read(&bak).unwrap().starts_with(b"SQLite format 3"));
    std::fs::rename(&bak, dir.path().join("restored.db")).unwrap();
    let restored = Store::open(&StoreOptions::new(dir.path(), "restored.db"), key).unwrap();
    assert!(restored.read(|c| sessions::get(c, &sid)).unwrap().is_some());
}

#[test]
fn message_tree_branches_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let sid = new_session(&store, "tree");
    let (u1, a1, a2, u2) = store
        .write(|tx| {
            let u1 = Message::new(&sid, None, "user");
            messages::insert(tx, &u1)?;
            messages::insert_part(tx, &Part::text(&u1.id, 0, "hello"))?;
            let mut a1 = Message::new(&sid, Some(&u1.id), "assistant");
            a1.created += 1;
            messages::insert(tx, &a1)?;
            // Regenerate: a sibling with the same parent.
            let mut a2 = Message::new(&sid, Some(&u1.id), "assistant");
            a2.created += 2;
            messages::insert(tx, &a2)?;
            let mut u2 = Message::new(&sid, Some(&a2.id), "user");
            u2.created += 3;
            messages::insert(tx, &u2)?;
            sessions::set_active_leaf(tx, &sid, Some(&u2.id))?;
            Ok((u1.id, a1.id, a2.id, u2.id))
        })
        .unwrap();
    store
        .read(|c| {
            let sibs: Vec<String> = messages::children(c, &sid, Some(&u1))?
                .into_iter()
                .map(|m| m.id)
                .collect();
            assert_eq!(sibs, vec![a1.clone(), a2.clone()]);
            let roots = messages::children(c, &sid, None)?;
            assert_eq!(roots.len(), 1);
            let path: Vec<String> = messages::active_path(c, &sid)?
                .into_iter()
                .map(|m| m.id)
                .collect();
            assert_eq!(path, vec![u1.clone(), a2.clone(), u2.clone()]);
            assert_eq!(messages::latest_leaf(c, &u1)?, u2);
            assert_eq!(messages::latest_leaf(c, &a1)?, a1);
            assert_eq!(messages::parts(c, &u1)?[0].body["text"], "hello");
            Ok(())
        })
        .unwrap();
    // Switch branch.
    store
        .write(|tx| sessions::set_active_leaf(tx, &sid, Some(&a1)))
        .unwrap();
    let path = store.read(|c| messages::active_path(c, &sid)).unwrap();
    assert_eq!(path.len(), 2);
    // A parent from another session is rejected.
    let other = new_session(&store, "other");
    let err = store
        .write(|tx| messages::insert(tx, &Message::new(&other, Some(&u1), "user")))
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)));
}

#[test]
fn event_log_seq_floor_and_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let sid = new_session(&store, "events");
    let now = now_ms();
    store
        .write(|tx| {
            events::append(
                tx,
                &sid,
                1,
                "turn_started",
                r#"{"type":"turn_started"}"#,
                now,
            )?;
            events::append(
                tx,
                &sid,
                2,
                "turn_settled",
                r#"{"type":"turn_settled"}"#,
                now,
            )?;
            events::append(tx, &sid, 3, "inbox_added", r#"{"type":"inbox_added"}"#, now)?;
            Ok(())
        })
        .unwrap();
    // Gaps and repeats are refused; nothing is written.
    let err = store
        .write(|tx| events::append(tx, &sid, 5, "x", "{}", now))
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::SeqGap {
            head: 3,
            got: 5,
            ..
        }
    ));
    assert!(
        store
            .write(|tx| events::append(tx, &sid, 3, "x", "{}", now))
            .is_err()
    );
    // Invalid JSON payloads are rejected by the CHECK.
    assert!(
        store
            .write(|tx| events::append(tx, &sid, 4, "x", "{", now))
            .is_err()
    );

    let after1: Vec<u64> = store
        .read(|c| events::events_after(c, &sid, 1))
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(after1, vec![2, 3]);

    // Not idle long enough: nothing pruned.
    assert!(
        store
            .write(|tx| events::prune_idle(tx, now, events::PRUNE_IDLE_MS))
            .unwrap()
            .is_empty()
    );
    // 31 days later: pruned up to the last settled turn.
    let later = now + events::PRUNE_IDLE_MS + 1;
    let pruned = store
        .write(|tx| events::prune_idle(tx, later, events::PRUNE_IDLE_MS))
        .unwrap();
    assert_eq!(pruned, vec![(sid.clone(), 2)]);
    let (head, floor) = store.read(|c| events::head(c, &sid)).unwrap().unwrap();
    assert_eq!((head, floor), (3, 2));
    let err = store
        .read(|c| events::events_after(c, &sid, 1))
        .unwrap_err();
    assert!(matches!(err, StoreError::BelowFloor { floor: 2, .. }));
    let rest = store.read(|c| events::events_after(c, &sid, 2)).unwrap();
    assert_eq!(rest.len(), 1);
    // Appends continue after head even when pruned.
    store
        .write(|tx| events::append(tx, &sid, 4, "x", "{}", now))
        .unwrap();
    // The floor never goes down.
    assert_eq!(store.write(|tx| events::prune(tx, &sid, 1)).unwrap(), 0);
}

#[test]
fn claims_and_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let a = new_session(&store, "a");
    let b = new_session(&store, "b");
    store
        .write(|tx| {
            events::claim_turn(tx, &a, "trn_1", Some(42), Some("boot"))?;
            events::claim_turn(tx, &b, "trn_2", None, None)?;
            events::settle_turn(tx, &b, "trn_2", TurnFinish::Completed)?;
            Ok(())
        })
        .unwrap();
    let open_claims = store.read(events::unsettled_claims).unwrap();
    assert_eq!(open_claims.len(), 1);
    assert_eq!(open_claims[0].turn_id, "trn_1");
    assert_eq!(open_claims[0].daemon_pid, Some(42));
    let lb = store.read(|c| events::last_turn(c, &b)).unwrap().unwrap();
    assert_eq!(lb.finish, Some(TurnFinish::Completed));
    // A new claim reopens.
    store
        .write(|tx| events::claim_turn(tx, &b, "trn_3", None, None))
        .unwrap();
    assert_eq!(store.read(events::unsettled_claims).unwrap().len(), 2);

    let parts = json!([{"type": "text", "text": "hi"}]);
    let (i1, new1) = store
        .write(|tx| events::inbox_add(tx, &a, Delivery::Queue, &parts, Some("cli"), Some("c1")))
        .unwrap();
    let (i1b, new1b) = store
        .write(|tx| events::inbox_add(tx, &a, Delivery::Queue, &parts, Some("cli"), Some("c1")))
        .unwrap();
    assert!(new1 && !new1b);
    assert_eq!(i1.id, i1b.id);
    let (i2, _) = store
        .write(|tx| events::inbox_add(tx, &a, Delivery::Steer, &parts, None, None))
        .unwrap();
    store
        .write(|tx| events::inbox_set_state(tx, &i1.id, InboxState::Admitted))
        .unwrap();
    let pending = store.read(|c| events::inbox_pending(c, &a)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, i2.id);
    assert_eq!(pending[0].delivery, Delivery::Steer);
}

#[test]
fn session_delete_cascades_and_releases_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let sid = new_session(&store, "doomed searchable");
    let child = store
        .write(|tx| {
            sessions::create(
                tx,
                &NewSession {
                    parent_session_id: Some(sid.clone()),
                    title: Some("subagent".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap()
        .id;
    let shared = store.put_blob(b"shared bytes", Some("text/plain")).unwrap();
    let req = store.put_blob(b"request", None).unwrap();
    let keep_sid = new_session(&store, "survivor");

    store
        .write(|tx| {
            let m = Message::new(&sid, None, "user");
            messages::insert(tx, &m)?;
            messages::insert_part(tx, &Part::text(&m.id, 0, "zebra crossing"))?;
            let mut att = Part::new(&m.id, 1, "attachment", json!({"name": "a.txt"}));
            att.blob_id = Some(shared.clone());
            messages::insert_part(tx, &att)?;
            messages::insert_wire_record(
                tx,
                &WireRecord {
                    id: new_id("wir"),
                    message_id: m.id.clone(),
                    step: 0,
                    provider: "anthropic".into(),
                    protocol: "messages".into(),
                    route: None,
                    account_tag: Some(vec![1, 2]),
                    request_blob: Some(req.clone()),
                    response_blob: Some(shared.clone()),
                    id_map: Some(json!({})),
                    created: now_ms(),
                },
            )?;
            events::append(tx, &sid, 1, "x", "{}", now_ms())?;
            events::inbox_add(tx, &sid, Delivery::Steer, &json!([]), None, None)?;
            events::claim_turn(tx, &sid, "trn", None, None)?;
            tx.execute(
                "INSERT INTO permission_pending (id, session_id, kind, payload, created)
                 VALUES ('ask_1', ?1, 'tool', '{}', 0)",
                [&sid],
            )?;
            tx.execute(
                "INSERT INTO background_job (id, session_id, kind, started, output_blob)
                 VALUES ('job_1', ?1, 'shell', 0, ?2)",
                [&sid, &shared],
            )?;
            tx.execute(
                "INSERT INTO browser_action (id, session_id, ts, tool) VALUES ('ba_1', ?1, 0, 'click')",
                [&sid],
            )?;
            let a = artifacts::create(tx, &sid, "code", Some("snippet"))?;
            artifacts::add_version(tx, &a.id, &m.id, Some("fn main() {}"), None)?;
            // A child session message and the survivor's part also use the shared blob.
            let cm = Message::new(&child, None, "user");
            messages::insert(tx, &cm)?;
            let km = Message::new(&keep_sid, None, "user");
            messages::insert(tx, &km)?;
            let mut kp = Part::new(&km.id, 0, "media", json!({}));
            kp.blob_id = Some(shared.clone());
            messages::insert_part(tx, &kp)?;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.blob_row(&shared).unwrap().unwrap().refcount, 4);
    assert_eq!(store.blob_row(&req).unwrap().unwrap().refcount, 1);
    assert_eq!(
        store
            .read(|c| search::chat_search(c, "zebra", &Scope::All, 10))
            .unwrap()
            .len(),
        1
    );

    assert!(store.write(|tx| sessions::delete(tx, &sid)).unwrap());

    store
        .read(|c| {
            for t in [
                "event",
                "inbox",
                "execution_claim",
                "permission_pending",
                "background_job",
                "browser_action",
                "artifact",
                "artifact_version",
                "wire_record",
            ] {
                assert_eq!(count(c, t), 0, "{t}");
            }
            assert_eq!(count(c, "session"), 1, "child session cascaded too");
            assert_eq!(count(c, "message"), 1);
            assert_eq!(count(c, "part"), 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(store.blob_row(&shared).unwrap().unwrap().refcount, 1);
    assert_eq!(store.blob_row(&req).unwrap().unwrap().refcount, 0);
    assert!(
        store
            .read(|c| search::chat_search(c, "zebra", &Scope::All, 10))
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .read(|c| search::title_search(c, "doomed", 10))
            .unwrap()
            .is_empty()
    );

    // GC respects the grace period, then removes unreferenced blobs only.
    assert_eq!(store.gc_blobs(now_ms()).unwrap(), 0);
    let later = now_ms() + genie_store::BLOB_GRACE_MS + 1;
    assert_eq!(store.gc_blobs(later).unwrap(), 1);
    assert!(store.blob_row(&req).unwrap().is_none());
    assert!(!store.blob_files().exists(&req));
    assert_eq!(store.read_blob(&shared).unwrap(), b"shared bytes");
}

#[test]
fn search_scopes_titles_memory_and_vacuum() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let project = store
        .write(|tx| projects::create(tx, "Rocket", None))
        .unwrap();
    let in_proj = store
        .write(|tx| {
            sessions::create(
                tx,
                &NewSession {
                    project_id: Some(project.id.clone()),
                    title: Some("Launch checklist".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap()
        .id;
    let loose = new_session(&store, "Café ideas");
    store
        .write(|tx| {
            for (s, text) in [
                (&in_proj, "fuel the booster"),
                (&loose, "booster seats for kids"),
            ] {
                let m = Message::new(s, None, "user");
                messages::insert(tx, &m)?;
                messages::insert_part(tx, &Part::text(&m.id, 0, text))?;
            }
            Ok(())
        })
        .unwrap();

    let hits = |scope: Scope, q: &str| {
        store
            .read(|c| search::chat_search(c, q, &scope, 10))
            .unwrap()
            .into_iter()
            .map(|h| h.session_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(hits(Scope::All, "booster").len(), 2);
    assert_eq!(
        hits(Scope::Project(project.id.clone()), "booster"),
        vec![in_proj.clone()]
    );
    assert_eq!(hits(Scope::NonProject, "booster"), vec![loose.clone()]);
    // Prefix on the last word, diacritics folded, title hits.
    assert_eq!(hits(Scope::All, "boost").len(), 2);
    assert_eq!(hits(Scope::All, "cafe"), vec![loose.clone()]);
    // FTS syntax in user input is inert.
    assert!(hits(Scope::All, "booster OR \"(").is_empty());

    assert_eq!(
        store
            .read(|c| search::title_search(c, "checkl", 10))
            .unwrap(),
        vec![in_proj.clone()]
    );
    assert_eq!(
        store.read(|c| search::title_search(c, "ca", 10)).unwrap(),
        vec![loose.clone()]
    );
    store
        .write(|tx| sessions::set_title(tx, &loose, Some("Renamed")))
        .unwrap();
    assert!(
        store
            .read(|c| search::title_search(c, "Café", 10))
            .unwrap()
            .is_empty()
    );
    assert_eq!(hits(Scope::All, "renamed"), vec![loose.clone()]);

    let mem = store
        .write(|tx| memory::add(tx, memory::GLOBAL, "User prefers metric units", None, None))
        .unwrap();
    store
        .write(|tx| memory::add(tx, &project.id, "Rocket uses metric too", None, None))
        .unwrap();
    let found = store
        .read(|c| memory::search(c, &[memory::GLOBAL], "metric", 10))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, mem.id);

    // VACUUM may renumber rowids; indexes are rebuilt and deletes still clean up.
    assert!(store.vacuum_if_needed(now_ms(), true).unwrap());
    assert_eq!(hits(Scope::All, "booster").len(), 2);
    store.write(|tx| sessions::delete(tx, &in_proj)).unwrap();
    assert_eq!(hits(Scope::All, "booster"), vec![loose.clone()]);
    store.write(|tx| memory::delete(tx, &mem.id)).unwrap();
    assert!(
        store
            .read(|c| memory::search(c, &[memory::GLOBAL], "metric", 10))
            .unwrap()
            .is_empty()
    );
    // Not forced, nothing to reclaim: skipped.
    assert!(!store.vacuum_if_needed(now_ms(), false).unwrap());
}

#[test]
fn retrieval_index_and_project_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let p = store
        .write(|tx| projects::create(tx, "Docs", None))
        .unwrap();
    let blob = store.put_blob(b"file body", Some("text/markdown")).unwrap();
    let f = store
        .write(|tx| {
            projects::add_file(tx, &p.id, "guide.md", Some("text/markdown"), &blob, Some(3))
        })
        .unwrap();
    let ids = store
        .write(|tx| {
            retrieval::replace_file_chunks(
                tx,
                &p.id,
                &f.id,
                &[
                    retrieval::NewChunk {
                        ordinal: 0,
                        start_byte: 0,
                        end_byte: 10,
                        tokens: 3,
                        text: "install the toolchain",
                    },
                    retrieval::NewChunk {
                        ordinal: 1,
                        start_byte: 10,
                        end_byte: 20,
                        tokens: 3,
                        text: "configure the proxy settings",
                    },
                    retrieval::NewChunk {
                        ordinal: 2,
                        start_byte: 20,
                        end_byte: 30,
                        tokens: 3,
                        text: "troubleshooting network errors",
                    },
                ],
            )
        })
        .unwrap();
    assert_eq!(
        store
            .read(|c| retrieval::bm25(c, &p.id, "proxy", 5))
            .unwrap(),
        vec![ids[1].clone()]
    );
    let missing = store
        .read(|c| retrieval::missing_embeddings(c, &p.id, "emb-1"))
        .unwrap();
    assert_eq!(missing.len(), 3);
    store
        .write(|tx| {
            for (i, (hash, _)) in missing.iter().enumerate() {
                let mut v = vec![0.0f32; 3];
                v[i] = 1.0;
                retrieval::put_embedding(tx, hash, "emb-1", &v)?;
            }
            Ok(())
        })
        .unwrap();
    // Which chunk got which axis depends on DISTINCT order; look it up.
    let c2 = store
        .read(|c| retrieval::chunk(c, &ids[2]))
        .unwrap()
        .unwrap();
    let v2 = store
        .read(|c| retrieval::get_embedding(c, &c2.content_hash, "emb-1"))
        .unwrap()
        .unwrap();
    let top = store
        .read(|c| retrieval::vector_search(c, &p.id, "emb-1", &v2, 1))
        .unwrap();
    assert_eq!(top, vec![ids[2].clone()]);
    let fused = store
        .read(|c| retrieval::hybrid_search(c, &p.id, "proxy", Some(("emb-1", &v2)), 2))
        .unwrap();
    assert_eq!(fused.len(), 2);
    assert!(fused.contains(&ids[1]) && fused.contains(&ids[2]));
    assert_eq!(store.blob_row(&blob).unwrap().unwrap().refcount, 1);

    store.write(|tx| projects::delete(tx, &p.id)).unwrap();
    store
        .read(|c| {
            assert_eq!(count(c, "kchunk"), 0);
            assert_eq!(count(c, "project_file"), 0);
            // Embeddings are a content-hash cache and outlive the project.
            assert_eq!(count(c, "kembedding"), 3);
            Ok(())
        })
        .unwrap();
    assert_eq!(store.blob_row(&blob).unwrap().unwrap().refcount, 0);
    assert!(
        store
            .read(|c| retrieval::bm25(c, &p.id, "proxy", 5))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn artifacts_follow_branches_and_spill_to_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let sid = new_session(&store, "art");
    let (root, b1, b2, art) = store
        .write(|tx| {
            let root = Message::new(&sid, None, "user");
            messages::insert(tx, &root)?;
            let b1 = Message::new(&sid, Some(&root.id), "assistant");
            messages::insert(tx, &b1)?;
            let b2 = Message::new(&sid, Some(&root.id), "assistant");
            messages::insert(tx, &b2)?;
            let art = artifacts::create(tx, &sid, "document", Some("Doc"))?;
            Ok((root.id, b1.id, b2.id, art.id))
        })
        .unwrap();
    let _ = root;
    let v1 = store.add_artifact_version(&art, &b1, "small").unwrap();
    let big = "x".repeat(artifacts::INLINE_MAX + 1);
    let v2 = store.add_artifact_version(&art, &b2, &big).unwrap();
    assert_eq!((v1.n, v2.n), (1, 2));
    assert!(v1.content.is_some() && v1.blob_id.is_none());
    let blob = v2.blob_id.clone().unwrap();
    assert_eq!(store.read_blob(&blob).unwrap(), big.as_bytes());
    assert_eq!(store.read_blob_range(&blob, 10, 5).unwrap(), b"xxxxx");

    store
        .write(|tx| sessions::set_active_leaf(tx, &sid, Some(&b1)))
        .unwrap();
    let on_path = store
        .read(|c| artifacts::versions_on_active_path(c, &art))
        .unwrap();
    assert_eq!(on_path.iter().map(|v| v.n).collect::<Vec<_>>(), vec![1]);
    store
        .write(|tx| sessions::set_active_leaf(tx, &sid, Some(&b2)))
        .unwrap();
    let on_path = store
        .read(|c| artifacts::versions_on_active_path(c, &art))
        .unwrap();
    assert_eq!(on_path.iter().map(|v| v.n).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn usage_kv_export_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let u = kv::Usage {
        day: "2026-11-17".into(),
        provider: "anthropic".into(),
        model: "m".into(),
        kind: "chat".into(),
        tokens_in: 10,
        calls: 1,
        cost_usd: 0.5,
        ..Default::default()
    };
    store.write(|tx| kv::add_usage(tx, &u)).unwrap();
    store.write(|tx| kv::add_usage(tx, &u)).unwrap();
    let rows = store
        .read(|c| kv::usage_between(c, "2026-11-01", "2026-11-30"))
        .unwrap();
    assert_eq!(rows.len(), 1, "no-project rows upsert into one");
    assert_eq!((rows[0].tokens_in, rows[0].calls), (20, 2));

    store
        .write(|tx| kv::set(tx, "flag", &json!({"on": true})))
        .unwrap();
    assert_eq!(
        store.read(|c| kv::get(c, "flag")).unwrap(),
        Some(json!({"on": true}))
    );

    // A failing closure rolls the whole transaction back.
    let sid = new_session(&store, "export me");
    let res: Result<(), StoreError> = store.write(|tx| {
        sessions::set_title(tx, &sid, Some("changed"))?;
        Err(StoreError::Invalid("boom".into()))
    });
    assert!(res.is_err());
    let s = store.read(|c| sessions::get(c, &sid)).unwrap().unwrap();
    assert_eq!(s.title.as_deref(), Some("export me"));

    store
        .write(|tx| {
            let m = Message::new(&sid, None, "user");
            messages::insert(tx, &m)?;
            messages::insert_part(tx, &Part::text(&m.id, 0, "question"))?;
            messages::insert_part(
                tx,
                &Part::new(
                    &m.id,
                    1,
                    "tool_call",
                    json!({"name": "read", "input": {"path": "/etc/secret"}}),
                ),
            )?;
            messages::insert_part(
                tx,
                &Part::new(&m.id, 2, "tool_result", json!({"output": "TOPSECRET"})),
            )?;
            sessions::set_active_leaf(tx, &sid, Some(&m.id))?;
            Ok(())
        })
        .unwrap();
    let full = store
        .read(|c| export::session_json(c, &sid, false))
        .unwrap();
    assert_eq!(full["parts"].as_array().unwrap().len(), 3);
    assert!(full.to_string().contains("TOPSECRET"));
    let clean = store.read(|c| export::session_json(c, &sid, true)).unwrap();
    let s = clean.to_string();
    assert!(!s.contains("TOPSECRET") && !s.contains("/etc/secret"));
    assert!(s.contains("question"));
    let md = store.read(|c| export::session_markdown(c, &sid)).unwrap();
    assert!(md.starts_with("# export me") && md.contains("question") && md.contains("`read`"));
}

#[test]
fn delete_all_data_removes_everything_it_owns() {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions::new(dir.path(), "genie.db");
    let ks = MemoryKeyStore::new();
    let store = Store::open_with_keystore(&opts, &ks).unwrap();
    store.put_blob(b"bye", None).unwrap();
    std::fs::write(dir.path().join("settings.json"), "{}").unwrap();
    store.delete_all_data(&ks).unwrap();
    assert!(!dir.path().join("genie.db").exists());
    assert!(!dir.path().join("genie.db-wal").exists());
    assert!(!dir.path().join("blobs").exists());
    assert!(dir.path().join("settings.json").exists());
    assert!(
        ks.get(genie_store::keys::DATA_KEY_ACCOUNT)
            .unwrap()
            .is_none()
    );
    // A fresh store starts cleanly.
    let fresh = Store::open_with_keystore(&opts, &ks).unwrap();
    assert_eq!(fresh.migration(), &migrate::Outcome::Bootstrapped);
}

#[test]
fn session_listing_and_status() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = open(&dir);
    let a = new_session(&store, "a");
    let b = new_session(&store, "b");
    store
        .write(|tx| {
            sessions::set_pinned(tx, &a, true)?;
            sessions::set_status(tx, &b, SessionStatus::AwaitingAsk)?;
            sessions::add_usage(tx, &b, 5, 7, 0.25)
        })
        .unwrap();
    let list = store
        .read(|c| sessions::list(c, &sessions::ListFilter::default()))
        .unwrap();
    assert_eq!(
        list.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![a.clone(), b.clone()]
    );
    let sb = store.read(|c| sessions::get(c, &b)).unwrap().unwrap();
    assert_eq!(sb.status, SessionStatus::AwaitingAsk);
    assert_eq!((sb.tokens_in, sb.tokens_out), (5, 7));
    store
        .write(|tx| sessions::set_archived(tx, &a, true))
        .unwrap();
    let list = store
        .read(|c| sessions::list(c, &sessions::ListFilter::default()))
        .unwrap();
    assert_eq!(list.len(), 1);
    // Retention leaves sessions awaiting an answer alone.
    let deleted = store
        .apply_retention(now_ms() + 10 * 86_400_000, 1)
        .unwrap();
    assert_eq!(deleted, 1);
    assert!(store.read(|c| sessions::get(c, &b)).unwrap().is_some());
}
