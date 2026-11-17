# Runbook: catalog and directory curation

Owner: feeds owner (~0.1–0.2 FTE after GA, 09 §6). Formats: `feeds/README.md`.

## Change an entry

1. Edit the source (`crates/genie-catalog/catalog/bundled.json`,
   `feeds/directory/{connectors,skills}/v1.json`, `feeds/notices/index.json`).
2. `cargo xtask ci feeds` (also runs in CI): builds, signs with throwaway keys, verifies.
   For the catalog also `cargo test -p genie-catalog`.
3. Merge. Dispatch the `feeds` workflow with `publish: true`: it downloads the live tree,
   builds with `version = live + 1`, signs with the online keys, verifies (signature,
   lifetime, no rollback) and uploads.

## Keep feeds fresh

Feeds expire (21-day default lifetime). Republish at least every 14 days even without
changes; the `feeds` workflow is the way (a scheduled dispatch can be added once keys
exist). An expired catalog falls back to the bundled snapshot; an expired directory shows
as unavailable.

## Skills directory entries

Each entry pins `version`, the package `sha256` and its full `files` list (06 §9). Review the
package (scripts!) before listing it; the install flow shows users the "what will run"
manifest generated from these fields.

## Mirrors (air-gapped orgs, D27/D38)

Customers mirror the published tree as-is (`.json` + `.sig`) and point `catalog.url`,
update URLs and directory URLs at the mirror by policy. They must re-sync within the feed
lifetime or their clients fail closed.
