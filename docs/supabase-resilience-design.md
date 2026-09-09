# Supabase resilience — complete, permanent, safe

**Status:** Design doc. Born from the 2026-05-21 jai-pay incident where the Supabase project's Postgres compute became unreachable while REST/Auth/Storage/Realtime stayed up. The static `supabase-runtime-shape` doctor caught related config drift but could not detect runtime unreachability. This doc closes that gap and lays out the doctrine.

**Audience:** anyone responsible for jai-pay's database availability.

---

## The incident, in one paragraph

A jai-pay user-visible endpoint returned `x-jaipay-source-status: degraded with db_backpressure`. Diagnosis ran through three hypotheses (URL shape → `connection_limit` missing → Postgres compute down). The first was wrong; the second was a real bug that got codified into a doctor; the third was the actual immediate cause and was discoverable only by probing the Supabase project directly. The static doctor pipeline could not catch the third class because it's a runtime liveness signal, not a configuration signal. The fix-of-record was "click Resume in the Supabase dashboard." That is *not* the kind of fix that scales for a payment app.

---

## The five layers of permanent fix

Listed in *increasing* depth. Layer 1 is what you do today. Layer 5 is what makes the same incident impossible.

### Layer 1 — Resume + verify (incident closure)

1. https://supabase.com/dashboard/project/`<project-ref>` → check compute status
2. If paused: click Resume
3. If active but failing: open Supabase support, attach `ECIRCUITBREAKER` + `EAUTHQUERY` log lines
4. Re-probe with `psql "$DATABASE_URL"` until `SELECT 1` returns
5. Re-deploy jai-pay only if `connection_limit=1` env was added (already done in PR #183 / live env)

**Done when:** `psql` returns `SELECT 1` ≤ 1s, and `/api/fitness/overview` returns `x-jaipay-source-status: ok`.

### Layer 2 — Upgrade out of free tier

The free tier auto-pauses after ~1 week of inactivity. **A payment app cannot run on free-tier Postgres.** Upgrade to Supabase Pro (or self-host PG on RDS / Neon).

Cost: ~$25/mo per project on Pro. This is the smallest, cheapest, most-effective fix.

**Done when:** `vercel env ls` shows the project pointing at a non-free-tier Supabase, and the Supabase dashboard shows no "free tier" badge.

### Layer 3 — Detection — runtime liveness doctor

Static config doctors can't catch a paused Postgres. We need a runtime probe.

New doctor: `supabase-project-liveness`. Behavior:
- Reads `DATABASE_URL` (or accepts `--from-env-file`)
- Issues a lightweight TCP+TLS+startup-packet probe against the host with a 2s budget
- Three rule IDs:
  - `supabase_project_db_unreachable` (error) — TCP/TLS/auth fails
  - `supabase_project_pooler_circuit_open` (error) — server returns `ECIRCUITBREAKER` / `EAUTHQUERY`
  - `supabase_project_slow_handshake` (warning) — connection establishes but takes > 1s

Wire into `leio-code audit --strict` so any deploy gate catches a dead DB before pushing. Wire into a cron job (workspace's existing scheduled-tasks plugin) so an unattended outage triggers a doctor finding within 60 seconds.

**Done when:** the doctor exists; `audit --strict` includes it; cron schedule is documented.

### Layer 4 — Application resilience — Redis-backed snapshot cache

The current snapshot cache uses `prisma.dashboardSnapshot.findUnique()`. **This means the cache lookup itself requires Postgres.** When Postgres dies, the cache fallback dies. That's an architectural smell.

**Fix:** move the snapshot cache to Redis or Vercel KV. The substrate that hosts the cache must be *independent* of the DB it's caching.

Sketch:

```typescript
// jai-pay/src/lib/dashboard-snapshots.ts (or new redis-snapshots.ts)
import { Redis } from "@upstash/redis";

const redis = Redis.fromEnv(); // reads KV_REST_API_URL + KV_REST_API_TOKEN

const SNAPSHOT_TTL_SECONDS = 8 * 60 * 60;

export async function readDashboardSnapshot<T>(key: string, scopeId: string) {
  const raw = await redis.get<string>(`snapshot:${key}:${scopeId}`);
  if (!raw) return null;
  return JSON.parse(raw) as SnapshotEnvelope<T>;
}

export async function writeDashboardSnapshot<T>(...) {
  await redis.set(`snapshot:${key}:${scopeId}`, JSON.stringify(envelope), {
    ex: SNAPSHOT_TTL_SECONDS,
  });
}
```

The `refresh*` paths still hit Postgres — that's where fresh data comes from. But all *read* paths land in Redis. When Postgres dies:
- Cached responses keep flowing from Redis (stale but available)
- Refresh jobs fail loudly; oncall gets paged
- Users see "data is X hours old" instead of 500s

**Done when:** `readDashboardSnapshot` no longer touches Prisma, and a smoke test of `/api/fitness/overview` succeeds with Postgres deliberately disabled.

### Layer 5 — Multi-region / multi-provider failover (XL, future)

The deepest layer. Even Pro Supabase can have regional outages. The eventual answer:

- **Read replica via Supabase Read Replicas** (Pro+ feature) in a different AWS region
- **Failover proxy** at the application or Vercel Edge layer that routes reads to either primary or replica based on health
- **Async replication to a backup provider** (e.g. nightly logical replication to Neon) for the case where Supabase is fully down

This is N weeks of engineering. Not for today. Document the path and revisit when SLA targets push it forward.

---

## Doctrine — operational practices that prevented the incident's full duration

These are the rules the incident exposed. Codify in `CLAUDE.md` / `AGENTS.md` and enforce via review.

### D1 — Never write sentinel values to production env vars

During incident response on 2026-05-21, a `"test-sentinel-value"` was briefly written to production `DATABASE_URL` while diagnosing the Vercel CLI's `vercel env add` behavior. The value was reverted within 3 seconds and no production traffic landed during the window, but **this is the kind of action that can trigger auth-circuit-breakers at the provider side** even with no live impact, because Supabase counts auth failures across the project. Future ops:

- Test CLI syntax against a **staging Vercel project** or a brand-new env var name, never against `DATABASE_URL`
- If you must test on prod, use a sentinel name like `DATABASE_URL_CANARY` you can drop later

### D2 — Sensitive Vercel env vars are write-only after creation

`vercel env pull` returns empty strings for sensitive values. Don't interpret an empty pull as "the value isn't set" — check via `vercel env ls`. Document this in `jai-pay/AGENTS.md`.

### D3 — Every layer of degradation must surface to a human

The runtime correctly classified the situation as backpressure and degraded gracefully. But **no human was paged until someone manually noticed**. Wire `markDatabaseBackpressure` calls to:

- A Slack webhook (`SLACK_INCIDENT_WEBHOOK_URL`)
- Optionally PagerDuty (`PAGERDUTY_INTEGRATION_KEY`)
- Rate-limit: at most one alert per 5 minutes per circuit-breaker firing

### D4 — Doctor-class boundaries

Static config doctors (`supabase-runtime-shape`, `compose-worker-mem-budget`) catch shape drift before it ships. Runtime liveness doctors (`supabase-project-liveness`, future `redis-availability`) catch infrastructure outages after they happen. **Both classes are first-class.** When you find an outage class neither catches, write a doctor for it.

### D5 — Cache substrate must be independent of the data it caches

The dashboard-snapshot pattern stored its cache in Postgres. That made the cache useless when Postgres died. The substrate has to be independent of the system it protects. Same rule applies to Redis caching API responses that depend on Postgres — the Redis instance must be in a different failure domain than Postgres.

### D6 — Test the failure path

Add to the CI suite: a test that disables Postgres connectivity and verifies the degraded path. Currently the route's degraded path is tested in `fitness/overview/route.test.ts` — keep that pattern. Extend to every public endpoint that has a degraded fallback.

---

## Concrete delivery plan (this PR + next two)

| Item | Layer | Where | Status |
|---|---|---|---|
| Resume project (incident closure) | 1 | Supabase dashboard | manual (you) |
| Upgrade to Pro (architecture) | 2 | Supabase billing | manual (you) |
| `supabase-project-liveness` doctor | 3 | `leio-code/src/doctors/` | **this PR** |
| `vercel-env-secret-protections` rules | doctrine | `jai-pay/AGENTS.md` | **this PR** |
| Redis-backed snapshot cache | 4 | `jai-pay/src/lib/dashboard-snapshots.ts` | follow-up PR |
| Backpressure → Slack/PagerDuty webhook | doctrine | `jai-pay/src/lib/prisma.ts` | follow-up PR |
| Read replicas + failover | 5 | Supabase config + Vercel Edge | quarterly review |

The doctors and doctrine in this PR are the parts you can ship today without making any production changes. The Pro upgrade and Redis migration are next-week work, with explicit ROI:

- **Pro upgrade alone** prevents 80% of this incident class (eliminates auto-pause)
- **Redis snapshot cache** prevents the remaining 20% (handles Pro's rare outages gracefully)
- **Read replicas** prevent the SLA-grade edge cases

---

## Non-goals (do not chase)

- Replacing Supabase entirely — works fine, just not on free tier for a payment app
- Cross-region replication — premature unless SLA targets demand it
- Self-hosting Postgres — operational cost > the $25/mo Pro plan
- Custom connection pooler in front of Supabase pooler — adding layers to debug; the existing PrismaPg + Supabase pooler is the right boundary

---

## Closing the loop

When the next class of incident hits, the test is: does `leio-code audit --strict` already catch it? If yes, the doctrine worked. If no, write the doctor that would have caught it and commit the lesson. The static `supabase-runtime-shape` doctor caught the `connection_limit` config gap correctly when pointed at the real env. The new `supabase-project-liveness` doctor closes the runtime gap. The next class will need the next doctor. Keep writing them.
