# Runbook: feed key custody and rotation

Spec 09 §4 (SA-31), t35 action item 5. Owners: release owner + security.

## Hierarchy

- **Root key** (Ed25519, offline): generated and stored on hardware (HSM or two hardware
  tokens), **two custodians**, never on a networked machine or in CI. Its public key is
  pinned in the clients. It signs only the key list.
- **Online keys**, one per feed kind (updates, catalog, connectors, skills, notices): held
  as CI secrets `GENIE_FEED_KEY_<KIND>` in the protected `release` environment. Listed in
  `feeds/keys/keylist.json` with `not_before`/`not_after`.
- **OS code-signing identities** (Developer ID, Trusted Signing profile, GPG) are separate
  and rotate on their vendors' schedules.
- Customer policy-signing keys are customer-owned (08 §5) and not part of this.

## Root key ceremony (first time; needs humans)

1. Two custodians, an air-gapped machine, the hardware tokens, a witness log.
2. Generate the root key on the token (or `cargo xtask feed keygen --out root.key` on the
   air-gapped machine, then import to the token and destroy the file).
3. Record `cargo xtask feed pubkey --key-file root.key` (hex) in the witness log; it goes
   into the clients' pinned root list and `vars.GENIE_FEED_ROOT_PUBKEY`.
4. Generate the five online keys (`feed keygen`), store each as its CI secret, record the
   public keys.
5. Fill `feeds/keys/keylist.json` from `keylist.template.json`, stamp it
   (`cargo xtask feed stamp feeds/keys/keylist.json --kind keylist --ttl-days 365`),
   sign it with the root key (`feed sign … --kind keylist --key-file <root>`), and commit
   both files.

## Yearly rotation (online keys)

Start ≥ 30 days before the oldest `not_after`.

1. Generate the new online key(s); store as the CI secret under a staging name.
2. New key list: add the new key alongside the old one (overlap ≥ 14 days, longer than the
   longest feed lifetime), bump `version`, stamp, root-sign, publish (`feeds` workflow).
3. After clients have the new list (one refresh cycle), swap the CI secret to the new key
   and republish all feeds of that kind.
4. After the overlap, publish a key list without the old key.

## Emergency rotation (online key compromised)

1. Root custodians produce a key list **without** the compromised key and with a fresh one
   (higher `version`). Publish immediately.
2. Swap the CI secret, republish every feed of that kind (higher versions).
3. Treat feeds signed in the exposure window as suspect: kill-switch anything unexpected
   ([incident-response.md](incident-response.md)).

## Root key compromise or loss

The root is pinned in shipped binaries, so this needs an app update: ship a release whose
pinned root list contains a new root (the old root may stay for one release to sign the
transition key list, unless it is the compromised one). Clients that never update keep
trusting the old root; that is why the root never touches a network.
