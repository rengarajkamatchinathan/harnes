# Runbook: staged rollout (stable)

Owner: release owner. Spec 09 §4.1, SA-33, D39, D40.

Stages **10% → 50% → 100%**. Each stage soaks **≥ 48 h** (24 h with `expedited` for a
critical security fix) and advances only if:

- crash-free session rate of the new release **≥ 99.5%**, and
- it is **not more than 0.2 points below** the previous stable's rate.

A failed rate gate **pauses** the rollout (clients stop picking it up; installs that
already updated stay). Not enough soak time only **holds**.

## Advance

1. Read the crash-free rates from the D39 crash dashboard for the release and the previous
   stable, and the hours since `rollout.started_at`.
2. Dispatch the `rollout` workflow: `action: advance` + the three numbers (+ `expedited`).
   It runs `cargo xtask feed gate`; on pass it runs `feed rollout --next` on every
   `updates/stable/*.json`, re-signs, verifies against the previous manifests (no rollback)
   and uploads. On a failed gate it pauses instead and the run is red.
3. Locally the same, for a dry run:
   ```sh
   cargo xtask feed gate --candidate 99.71 --baseline 99.80 --soak-hours 52
   cargo xtask feed rollout updates/stable/linux-x86_64.json --next
   cargo xtask feed sign updates/stable/linux-x86_64.json --kind updates --key-env GENIE_FEED_KEY_UPDATES
   ```

## Pause / resume

`rollout` workflow with `action: pause` (or `resume`). Pausing is cheap and safe: do it
first when in doubt, then investigate.

## How clients pick

An install is in the rollout when `bucket(install_id, version) < percent` and the manifest
is not paused (feeds/README.md). The bucket is stable per install and release, so a user
doesn't flip in and out between checks. `updates.minVersion` (admin policy) and the
manifest's `min_version` bypass the percentage: they force the upgrade prompt.
