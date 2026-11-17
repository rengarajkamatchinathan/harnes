-- 0001: initial schema (spec/04-storage.md §3, §6, §7).
-- Ids are ULID text with a type prefix; times are integer ms since epoch;
-- JSON columns are TEXT checked with json_valid.

-- §3.4 blobs first: everything else references them.
CREATE TABLE blob (
  id       TEXT PRIMARY KEY,                -- sha256 hex of the plaintext
  size     INTEGER NOT NULL,
  mime     TEXT,
  refcount INTEGER NOT NULL DEFAULT 0,
  created  INTEGER NOT NULL
);
CREATE INDEX blob_gc ON blob (refcount, created);

-- §3.1 projects and workspaces
CREATE TABLE project (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  instructions TEXT,
  settings     TEXT CHECK (settings IS NULL OR json_valid(settings)),
  starred      INTEGER NOT NULL DEFAULT 0,
  created      INTEGER NOT NULL,
  updated      INTEGER NOT NULL
);
CREATE TABLE project_file (
  id         TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES project (id) ON DELETE CASCADE,
  name       TEXT NOT NULL,
  mime       TEXT,
  blob_id    TEXT REFERENCES blob (id),
  tokens     INTEGER,
  sha256     TEXT,
  added      INTEGER NOT NULL
);
CREATE INDEX project_file_project ON project_file (project_id);

CREATE TABLE workspace (
  id             TEXT PRIMARY KEY,
  canonical_path TEXT NOT NULL UNIQUE,
  identity       TEXT,
  git_root       TEXT,
  created        INTEGER NOT NULL,
  last_opened    INTEGER NOT NULL
);

-- §3.2 sessions, messages, parts
CREATE TABLE session (
  id                TEXT PRIMARY KEY,
  kind              TEXT NOT NULL CHECK (kind IN ('chat', 'code')),
  project_id        TEXT REFERENCES project (id) ON DELETE SET NULL,
  workspace_id      TEXT REFERENCES workspace (id) ON DELETE SET NULL,
  worktree_id       TEXT,  -- → worktree.id; not declared because worktree.session_id points back
  parent_session_id TEXT REFERENCES session (id) ON DELETE CASCADE,
  agent             TEXT,
  title             TEXT,
  model             TEXT,
  effort            TEXT,
  mode              TEXT,
  active_leaf_id    TEXT,
  status            TEXT NOT NULL DEFAULT 'idle'
                    CHECK (status IN ('idle', 'running', 'awaiting_ask', 'interrupted')),
  driver_client     TEXT,
  head_seq          INTEGER NOT NULL DEFAULT 0,
  log_floor_seq     INTEGER NOT NULL DEFAULT 0,
  cost_usd          REAL NOT NULL DEFAULT 0,
  tokens_in         INTEGER NOT NULL DEFAULT 0,
  tokens_out        INTEGER NOT NULL DEFAULT 0,
  pinned            INTEGER NOT NULL DEFAULT 0,
  archived          INTEGER NOT NULL DEFAULT 0,
  created           INTEGER NOT NULL,
  updated           INTEGER NOT NULL
);
CREATE INDEX session_list ON session (kind, archived, updated DESC);
CREATE INDEX session_project ON session (project_id, updated DESC);
CREATE INDEX session_workspace ON session (workspace_id, updated DESC);
CREATE INDEX session_parent ON session (parent_session_id);

CREATE TABLE worktree (
  id               TEXT PRIMARY KEY,
  workspace_id     TEXT NOT NULL REFERENCES workspace (id) ON DELETE CASCADE,
  session_id       TEXT REFERENCES session (id) ON DELETE SET NULL,
  path             TEXT NOT NULL UNIQUE,
  branch           TEXT,
  base_ref         TEXT,
  created_by_genie INTEGER NOT NULL DEFAULT 0,
  setup_hash       TEXT,
  created          INTEGER NOT NULL
);

CREATE TABLE message (
  id              TEXT PRIMARY KEY,
  session_id      TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  parent_id       TEXT,
  role            TEXT NOT NULL,
  seq             INTEGER,
  created         INTEGER NOT NULL,
  model           TEXT,
  provider        TEXT,
  origin          TEXT CHECK (origin IS NULL OR json_valid(origin)),
  usage           TEXT CHECK (usage IS NULL OR json_valid(usage)),
  cost_usd        REAL,
  finish          TEXT,
  error           TEXT CHECK (error IS NULL OR json_valid(error)),
  kind            TEXT NOT NULL DEFAULT 'normal' CHECK (kind IN ('normal', 'compaction', 'synthetic')),
  snapshot_before TEXT,
  snapshot_after  TEXT
);
CREATE INDEX message_tree ON message (session_id, parent_id);
CREATE INDEX message_created ON message (session_id, created);

CREATE TABLE part (
  id         TEXT PRIMARY KEY,
  message_id TEXT NOT NULL REFERENCES message (id) ON DELETE CASCADE,
  ordinal    INTEGER NOT NULL,
  kind       TEXT NOT NULL,
  body       TEXT NOT NULL CHECK (json_valid(body)),
  blob_id    TEXT REFERENCES blob (id),
  call_id    TEXT,
  UNIQUE (message_id, ordinal)
);
CREATE INDEX part_call ON part (call_id);

CREATE TABLE wire_record (
  id            TEXT PRIMARY KEY,
  message_id    TEXT NOT NULL REFERENCES message (id) ON DELETE CASCADE,
  step          INTEGER NOT NULL,
  provider      TEXT NOT NULL,
  protocol      TEXT NOT NULL,
  route         TEXT,
  account_tag   BLOB,
  request_blob  TEXT REFERENCES blob (id),
  response_blob TEXT REFERENCES blob (id),
  id_map        TEXT CHECK (id_map IS NULL OR json_valid(id_map)),
  created       INTEGER NOT NULL
);
CREATE INDEX wire_record_message ON wire_record (message_id);

-- §3.3 events, inbox, claims, asks
CREATE TABLE event (
  session_id TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  seq        INTEGER NOT NULL,
  type       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),
  created    INTEGER NOT NULL,
  PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE TABLE inbox (
  id            TEXT PRIMARY KEY,
  session_id    TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  delivery      TEXT NOT NULL CHECK (delivery IN ('steer', 'queue')),
  parts         TEXT NOT NULL CHECK (json_valid(parts)),
  source        TEXT,
  client_msg_id TEXT,
  state         TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'admitted', 'removed')),
  created       INTEGER NOT NULL,
  UNIQUE (session_id, client_msg_id)
);
CREATE INDEX inbox_state ON inbox (session_id, state, created);

-- `finish` is an addition to 04 §3.3: settling a claim keeps the row with its finish, so
-- the latest turn record of a session survives a restart (genie-core `Journal::last_turn`).
CREATE TABLE execution_claim (
  session_id     TEXT PRIMARY KEY REFERENCES session (id) ON DELETE CASCADE,
  turn_id        TEXT NOT NULL,
  started        INTEGER NOT NULL,
  daemon_pid     INTEGER,
  daemon_boot_id TEXT,
  finish         TEXT
);

CREATE TABLE permission_pending (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  turn_id    TEXT,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),
  created    INTEGER NOT NULL,
  state      TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'answered', 'cancelled')),
  decision   TEXT CHECK (decision IS NULL OR json_valid(decision))
);
CREATE INDEX permission_pending_state ON permission_pending (state, created);

CREATE TABLE background_job (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  kind        TEXT NOT NULL,
  command     TEXT,
  cwd         TEXT,
  started     INTEGER NOT NULL,
  ended       INTEGER,
  exit_code   INTEGER,
  output_blob TEXT REFERENCES blob (id)
);

-- §3.4 artifacts, memory, usage, browser
CREATE TABLE artifact (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  type       TEXT NOT NULL,
  title      TEXT,
  created    INTEGER NOT NULL
);
CREATE TABLE artifact_version (
  artifact_id TEXT NOT NULL REFERENCES artifact (id) ON DELETE CASCADE,
  n           INTEGER NOT NULL,
  message_id  TEXT NOT NULL REFERENCES message (id) ON DELETE CASCADE,
  content     TEXT,
  blob_id     TEXT REFERENCES blob (id),
  created     INTEGER NOT NULL,
  PRIMARY KEY (artifact_id, n)
);
CREATE INDEX artifact_version_message ON artifact_version (message_id);

CREATE TABLE memory (
  id             TEXT PRIMARY KEY,
  scope          TEXT NOT NULL,
  text           TEXT NOT NULL,
  source_session TEXT,
  source_message TEXT,
  created        INTEGER NOT NULL,
  updated        INTEGER NOT NULL,
  last_used      INTEGER
);
CREATE INDEX memory_scope ON memory (scope, last_used DESC);

-- project_id is '' (not NULL) outside a project: NULLs never collide in a primary key,
-- which would break the per-day upsert.
CREATE TABLE usage_daily (
  day         TEXT NOT NULL,
  provider    TEXT NOT NULL,
  model       TEXT NOT NULL,
  project_id  TEXT NOT NULL DEFAULT '',
  kind        TEXT NOT NULL,
  tokens_in   INTEGER NOT NULL DEFAULT 0,
  tokens_out  INTEGER NOT NULL DEFAULT 0,
  cache_read  INTEGER NOT NULL DEFAULT 0,
  cache_write INTEGER NOT NULL DEFAULT 0,
  calls       INTEGER NOT NULL DEFAULT 0,
  cost_usd    REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (day, provider, model, project_id, kind)
);

CREATE TABLE browser_action (
  id             TEXT PRIMARY KEY,
  session_id     TEXT NOT NULL REFERENCES session (id) ON DELETE CASCADE,
  ts             INTEGER NOT NULL,
  tab_id         TEXT,
  tool           TEXT NOT NULL,
  origin         TEXT,
  args_redacted  TEXT CHECK (args_redacted IS NULL OR json_valid(args_redacted)),
  decision       TEXT CHECK (decision IS NULL OR json_valid(decision)),
  status         TEXT,
  thumbnail_blob TEXT REFERENCES blob (id)
);

CREATE TABLE kv (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL CHECK (json_valid(value))
);

-- §7 retrieval index
CREATE TABLE kchunk (
  id           TEXT PRIMARY KEY,
  project_id   TEXT NOT NULL REFERENCES project (id) ON DELETE CASCADE,
  file_id      TEXT NOT NULL REFERENCES project_file (id) ON DELETE CASCADE,
  ordinal      INTEGER NOT NULL,
  start_byte   INTEGER NOT NULL,
  end_byte     INTEGER NOT NULL,
  tokens       INTEGER NOT NULL,
  text         TEXT NOT NULL,
  content_hash BLOB NOT NULL
);
CREATE INDEX kchunk_project ON kchunk (project_id);
CREATE INDEX kchunk_file ON kchunk (file_id, ordinal);

CREATE TABLE kembedding (
  content_hash BLOB NOT NULL,
  model        TEXT NOT NULL,
  dim          INTEGER NOT NULL,
  vector       BLOB NOT NULL,
  created      INTEGER NOT NULL,
  PRIMARY KEY (content_hash, model)
);

-- §6 full-text search. Row ids of tables without an INTEGER PRIMARY KEY may change on
-- VACUUM, so `Store::vacuum` rebuilds every FTS index afterwards.
--
-- message_fts: text parts (rowid = part.rowid) and session titles (rowid = -session.rowid).
-- It keeps its own copy of the text because it indexes two source tables.
CREATE VIRTUAL TABLE message_fts USING fts5 (
  text, title, session_id UNINDEXED, message_id UNINDEXED,
  tokenize = 'unicode61 remove_diacritics 2'
);
-- Substring search over titles (sidebar); rowid = session.rowid.
CREATE VIRTUAL TABLE session_title_fts USING fts5 (title, tokenize = 'trigram');
CREATE VIRTUAL TABLE memory_fts USING fts5 (
  text, content = 'memory', content_rowid = 'rowid',
  tokenize = 'unicode61 remove_diacritics 2'
);
CREATE VIRTUAL TABLE kchunk_fts USING fts5 (
  text, content = 'kchunk', content_rowid = 'rowid',
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER part_fts_ai AFTER INSERT ON part WHEN new.kind = 'text' BEGIN
  INSERT INTO message_fts (rowid, text, title, session_id, message_id)
  VALUES (new.rowid, json_extract(new.body, '$.text'), NULL,
          (SELECT session_id FROM message WHERE id = new.message_id), new.message_id);
END;
CREATE TRIGGER part_fts_ad AFTER DELETE ON part WHEN old.kind = 'text' BEGIN
  DELETE FROM message_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER part_fts_au AFTER UPDATE OF body, kind ON part BEGIN
  DELETE FROM message_fts WHERE rowid = old.rowid;
  INSERT INTO message_fts (rowid, text, title, session_id, message_id)
  SELECT new.rowid, json_extract(new.body, '$.text'), NULL,
         (SELECT session_id FROM message WHERE id = new.message_id), new.message_id
  WHERE new.kind = 'text';
END;

CREATE TRIGGER session_fts_ai AFTER INSERT ON session WHEN new.title IS NOT NULL BEGIN
  INSERT INTO message_fts (rowid, text, title, session_id, message_id)
  VALUES (-new.rowid, NULL, new.title, new.id, NULL);
  INSERT INTO session_title_fts (rowid, title) VALUES (new.rowid, new.title);
END;
CREATE TRIGGER session_fts_ad AFTER DELETE ON session BEGIN
  DELETE FROM message_fts WHERE rowid = -old.rowid;
  DELETE FROM session_title_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER session_fts_au AFTER UPDATE OF title ON session BEGIN
  DELETE FROM message_fts WHERE rowid = -old.rowid;
  DELETE FROM session_title_fts WHERE rowid = old.rowid;
  INSERT INTO message_fts (rowid, text, title, session_id, message_id)
  SELECT -new.rowid, NULL, new.title, new.id, NULL WHERE new.title IS NOT NULL;
  INSERT INTO session_title_fts (rowid, title)
  SELECT new.rowid, new.title WHERE new.title IS NOT NULL;
END;

CREATE TRIGGER memory_fts_ai AFTER INSERT ON memory BEGIN
  INSERT INTO memory_fts (rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER memory_fts_ad AFTER DELETE ON memory BEGIN
  INSERT INTO memory_fts (memory_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TRIGGER memory_fts_au AFTER UPDATE OF text ON memory BEGIN
  INSERT INTO memory_fts (memory_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
  INSERT INTO memory_fts (rowid, text) VALUES (new.rowid, new.text);
END;

CREATE TRIGGER kchunk_fts_ai AFTER INSERT ON kchunk BEGIN
  INSERT INTO kchunk_fts (rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER kchunk_fts_ad AFTER DELETE ON kchunk BEGIN
  INSERT INTO kchunk_fts (kchunk_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TRIGGER kchunk_fts_au AFTER UPDATE OF text ON kchunk BEGIN
  INSERT INTO kchunk_fts (kchunk_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
  INSERT INTO kchunk_fts (rowid, text) VALUES (new.rowid, new.text);
END;

-- §4 blob refcounts, kept in the same transaction as the referencing row. Cascading
-- deletes fire these too, so deleting a session releases its blobs.
CREATE TRIGGER project_file_blob_id_ai AFTER INSERT ON project_file WHEN new.blob_id IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.blob_id;
END;
CREATE TRIGGER project_file_blob_id_ad AFTER DELETE ON project_file WHEN old.blob_id IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.blob_id;
END;
CREATE TRIGGER project_file_blob_id_au AFTER UPDATE OF blob_id ON project_file WHEN old.blob_id IS NOT new.blob_id BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.blob_id;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.blob_id;
END;

CREATE TRIGGER part_blob_id_ai AFTER INSERT ON part WHEN new.blob_id IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.blob_id;
END;
CREATE TRIGGER part_blob_id_ad AFTER DELETE ON part WHEN old.blob_id IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.blob_id;
END;
CREATE TRIGGER part_blob_id_au AFTER UPDATE OF blob_id ON part WHEN old.blob_id IS NOT new.blob_id BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.blob_id;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.blob_id;
END;

CREATE TRIGGER wire_record_request_blob_ai AFTER INSERT ON wire_record WHEN new.request_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.request_blob;
END;
CREATE TRIGGER wire_record_request_blob_ad AFTER DELETE ON wire_record WHEN old.request_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.request_blob;
END;
CREATE TRIGGER wire_record_request_blob_au AFTER UPDATE OF request_blob ON wire_record WHEN old.request_blob IS NOT new.request_blob BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.request_blob;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.request_blob;
END;

CREATE TRIGGER wire_record_response_blob_ai AFTER INSERT ON wire_record WHEN new.response_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.response_blob;
END;
CREATE TRIGGER wire_record_response_blob_ad AFTER DELETE ON wire_record WHEN old.response_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.response_blob;
END;
CREATE TRIGGER wire_record_response_blob_au AFTER UPDATE OF response_blob ON wire_record WHEN old.response_blob IS NOT new.response_blob BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.response_blob;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.response_blob;
END;

CREATE TRIGGER artifact_version_blob_id_ai AFTER INSERT ON artifact_version WHEN new.blob_id IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.blob_id;
END;
CREATE TRIGGER artifact_version_blob_id_ad AFTER DELETE ON artifact_version WHEN old.blob_id IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.blob_id;
END;
CREATE TRIGGER artifact_version_blob_id_au AFTER UPDATE OF blob_id ON artifact_version WHEN old.blob_id IS NOT new.blob_id BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.blob_id;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.blob_id;
END;

CREATE TRIGGER background_job_output_blob_ai AFTER INSERT ON background_job WHEN new.output_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.output_blob;
END;
CREATE TRIGGER background_job_output_blob_ad AFTER DELETE ON background_job WHEN old.output_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.output_blob;
END;
CREATE TRIGGER background_job_output_blob_au AFTER UPDATE OF output_blob ON background_job WHEN old.output_blob IS NOT new.output_blob BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.output_blob;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.output_blob;
END;

CREATE TRIGGER browser_action_thumbnail_blob_ai AFTER INSERT ON browser_action WHEN new.thumbnail_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.thumbnail_blob;
END;
CREATE TRIGGER browser_action_thumbnail_blob_ad AFTER DELETE ON browser_action WHEN old.thumbnail_blob IS NOT NULL BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.thumbnail_blob;
END;
CREATE TRIGGER browser_action_thumbnail_blob_au AFTER UPDATE OF thumbnail_blob ON browser_action WHEN old.thumbnail_blob IS NOT new.thumbnail_blob BEGIN
  UPDATE blob SET refcount = refcount - 1 WHERE id = old.thumbnail_blob;
  UPDATE blob SET refcount = refcount + 1 WHERE id = new.thumbnail_blob;
END;
