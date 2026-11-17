# feeds/

Sources for the signed static feeds served from Azure Storage + CDN (spec 09 §4, D27, D30,
D37, D40). `cargo xtask feed build` turns them into the published tree; the `feeds`
workflow dry-runs that on every change and publishes on a manual dispatch.

| Source | Published at | Signed by |
|---|---|---|
| `../crates/genie-catalog/catalog/bundled.json` | `catalog/v1/catalog.json` | catalog key |
| `directory/connectors/v1.json` | `directory/connectors/v1.json` | connectors key |
| `directory/skills/v1.json` | `directory/skills/v1.json` | skills key |
| `notices/index.json` | `notices/index.json` | notices key |
| `dist/update-<channel>-<target>.json` (from `xtask dist`) | `updates/<channel>/<target>.json` | updates key |
| `keys/keylist.json` + `.sig` (offline) | `keys/keylist.json` | **root** key |

The catalog's source stays in `genie-catalog` so the bundled snapshot and the feed are the
same document.

## Format

Every feed is a JSON object with the header `{version, issued_at, expires_at}` (UTC
`YYYY-MM-DDTHH:MM:SSZ`) and a detached `<file>.sig`: base64 of the 64-byte Ed25519
signature over the exact bytes, the format `genie-catalog::signature::verify_with` and
`genie-skills::directory::verify_feed` check. Public keys are 32-byte hex.

- `version` only goes up. `feed build` sets it to `max(source, live + 1)` given
  `--previous-dir`, so re-publishing old content (rollback) still gets a higher version.
- Lifetimes: updates ≤ 14 days (default 7), catalog/directories/notices ≤ 30 (default 21),
  key list ≤ 400 (default 365). Clients reject expired feeds, so a stalled publisher or a
  frozen mirror fails closed; air-gapped mirrors must re-sync within the lifetime.
- Update manifests: `channel`, `target`, `release {version, commit, pub_date, notes,
  notes_url}`, optional `min_version`, `rollout {percent, started_at, paused, expedited}`,
  `artifacts [{kind, format, url, sha256, size, signature}]` (artifact signature = same
  Ed25519 format over the artifact bytes). Stable uses the stages 10/50/100.
- Rollout bucketing: an install is in the rollout when
  `u64_be(sha256("genie-rollout:" + install_id + ":" + release.version)[0..8]) % 100 <
  percent`, unless `paused`. Reference: `xtask feed bucket`.

## Key hierarchy (SA-31)

Clients pin the offline root key. The root signs `keys/keylist.json`, naming one online key
per feed kind with `not_before`/`not_after`; `keys/keylist.template.json` shows the shape.
Online keys rotate yearly or on compromise by publishing a new root-signed key list
([key rotation runbook](../release/runbooks/key-rotation.md)). Until the root ceremony
happens, the clients' `PINNED_KEYS_HEX` lists are empty and `feed build` skips the key list.
