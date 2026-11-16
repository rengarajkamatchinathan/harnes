# 41 — Business model, licensing and source model (deferred)

type: wayfinder:grilling · status: deferred · claimed-by: — · blocked-by: — (deferred by the user, 2026-11-16)

## Question
How is Genie sold, how are licences enforced, and is the source closed or open?

## Options already outlined (from ticket 35)
- **Business model:** per-seat subscription with BYO provider keys · free app + paid Enterprise tier (policy, audit, MDM, SSO) · bundled model credits through a Genie gateway (needs billing + reseller agreements).
- **Licence enforcement:** online activation + 30-day offline grace + signed offline licence file for air-gapped orgs · signed licence file only · honour system.
- **Source:** closed · source-available (BSL/FSL) · open-core (open engine/CLI, closed enterprise features).

## Design hooks already in place
- A `licence` module boundary in the daemon; Settings → Account & licence (D34); policy can deliver a licence file (D38).

## Also deferred
- **Repo host + CI platform** (GitHub + Actions / Azure DevOps / GitHub + Azure Pipelines), deferred during 34/38. Affects signing integration (D40), secrets for live provider tests (34), and the CI drift checks (D20/D22/38).

## When to revisit
Before the first external (non-Synergech) release, or when pricing/packaging work starts.

## Resolution
_deferred_
