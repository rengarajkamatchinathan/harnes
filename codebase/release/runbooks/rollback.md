# Runbook: rolling back a release

Owner: release owner. Spec 09 §6, D40.

Clients reject a feed whose `version` is lower than the one they hold, so **a rollback is a
new publish**: the previous release's manifest re-signed with a **higher feed version**.
Binaries never downgrade users silently in place; the updater sees "a newer manifest that
points at release N-1" and installs it like any update. The DB has a pre-migration
encrypted backup for this case (04 §5); a release whose migrations aren't backward
compatible must say so in its notes, and rollback then restores that backup.

## Steps

1. **Pause first** (`rollout` workflow, `action: pause`) to stop further installs.
2. Decide: pause only (bad release reached few users, fix-forward soon) vs. roll back.
3. Roll back:
   ```sh
   # previous release's per-target manifests, e.g. from the release-bundle artifact of N-1
   cargo xtask feed build --only updates --updates-dir <dir with N-1 update-*.json> \
       --previous-dir <currently published tree> --out out
   # feed versions are now live+1; verify, then upload out/updates/** over the live ones
   cargo xtask feed verify out/updates/stable/linux-x86_64.json --kind updates \
       --pubkey <updates key> --previous <live>/updates/stable/linux-x86_64.json
   ```
   Set `rollout.percent` to 100 in those manifests (`feed rollout --percent 100`) so every
   client on N moves back, then re-sign.
4. If release N is **vulnerable**, also raise `min_version` in the next good release and
   tell admins to set `updates.minVersion` (D38) past it.
5. Linux .deb/.rpm users update through apt/yum: publish N-1 again with a higher package
   version (`<N-1>+rollback1`) to the repos.
6. Write up the incident ([incident-response.md](incident-response.md)).

## Failed install on a client

Handled by the client (D40): a failed desktop install rolls back to the previous version by
itself; nothing to publish.
