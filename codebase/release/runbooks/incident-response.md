# Runbook: incident response

Spec 09 §6, 08 §9. Roles: incident lead (first responder from the owner list in
`release/README.md`), release owner (updates), feeds owner (catalog/directories),
security (keys, disclosure).

## Triage (first 30 minutes)

1. Open an incident channel/ticket; note start time, reporter, affected versions/channels.
2. Classify:
   - **Bad release** (crashes, data loss, regression) → [rollback.md](rollback.md).
   - **Vulnerable release** → expedited fix + `min_version`; see below.
   - **Bad feed content** (broken catalog entry, malicious/broken connector or skill) →
     kill switch below.
   - **Key compromise** (any feed key, OS signing identity) → [key-rotation.md](key-rotation.md)
     *emergency* section.
   - **Feed outage** (CDN/storage down) → clients keep their last good cached copy
     (catalog, directories) and skip the update check; restore hosting, then republish so
     nothing expires (lifetimes in feeds/README.md).
3. **Pause the stable rollout** if a release is involved. It is always safe.

## Feed kill switch

Publish a feed that removes or disables the broken entry. The new feed must have a higher
`version`, which `feed build --previous-dir` guarantees:

```sh
# edit the source: remove the model from crates/genie-catalog/catalog/bundled.json,
# or the entry from feeds/directory/{connectors,skills}/v1.json
cargo xtask feed build --only catalog --out out --previous-dir <live tree>   # (or connectors/skills)
cargo xtask feed verify out/catalog/v1/catalog.json --kind catalog --pubkey <catalog key> \
    --previous <live>/catalog/v1/catalog.json
# upload out/ (feeds workflow with publish: true does build + verify + upload)
```

Clients refresh the catalog on start and every ~6 h, directories daily and on open. For
installed skills/connectors, the removal stops new installs; uninstall guidance goes in
`notices/index.json`.

## Vulnerable build

1. Fix on main, cut an expedited release ([release.md](release.md), `expedited`).
2. In the fixed release's manifests set `min_version` to the fixed version: older clients
   get a forced upgrade prompt.
3. Ask managed-deployment admins to set `updates.minVersion` (D38) and, for MSI
   deployments with the in-app updater off, to push the fixed MSI.
4. Coordinate disclosure with security once ≥ 90% of active installs are on the fix.

## Close-out

Post-incident review within 5 working days: timeline, impact, what detected it, what slowed
the response, follow-up tickets. Update this runbook if a step was missing or wrong.
