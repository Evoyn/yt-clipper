# YouTube publishing — full design: multi-account (10 → 50 → 100), upload, scheduling, analytics

Status: DESIGN DOC (2026-07-07; API facts as of early 2026 — re-verify
every endpoint, quota number, and policy line against current Google
docs before building; they drift). Companion to
`docs/publishing-plan.md` (the 5-platform overview). Implementation
opens with /grill-with-docs; the grill pins the ADR.

## 1. The model: accounts are a first-class, compounding entity

The operator runs a growing network of YouTube channels (~10 today,
50–100 later). Every design choice below is judged against "does this
still work at 100 accounts?":

- **Connected account** = one authorized YouTube CHANNEL (a Google
  identity OR one of its Brand Accounts — one Google login can own
  several channels; the channel is chosen inside Google's own OAuth
  consent flow). The app stores one refresh token per connected account.
- **Account registry** = the persistent list of connected accounts with
  label, channel id/title, group tags, and token health — rendered like
  the Diagnostics dependency registry (a row + status dot + action
  button; that pattern already scales visually and is operator-familiar).
- **Posting profile** = a saved selection (accounts + title/description
  template + schedule stagger). At 10 accounts a checkbox list is fine;
  at 100, profiles + group filters are the UI. Compounding-ready from
  day one costs little: store `groups: ["id-clips", "en-clips"]` per
  account now.

## 2. Google Cloud setup — ONE project, two paperwork tracks

One Google Cloud project, one OAuth **Desktop app** client ID. All N
accounts authorize against this single client — N is unbounded on the
OAuth side; each account's grant mints its own refresh token.

Two one-time paperwork tracks gate real usage (start both immediately;
build against their pending state):

1. **OAuth verification** (consent-screen review for the sensitive
   `youtube.upload` scope). Until it passes: (a) a consent screen left
   in "Testing" expires refresh tokens every 7 days — at 10+ accounts
   weekly re-auth of every account is unworkable, so publish the consent
   screen early even while unverified; (b) **videos uploaded via an
   unverified API project are locked PRIVATE** — fine for development,
   blocking for production.
2. **Quota extension** (YouTube API Services audit). This is the real
   scaling gate — see §3. The form wants the use case, expected daily
   usage, and a demo; honest answer: "desktop tool uploading N
   Shorts/day across the operator's own channels."

**Hard rule (ToS)**: do NOT spin up multiple Cloud projects to multiply
quota — Google's developer policies explicitly forbid circumventing
quota with extra projects, and the audit checks for it. One project,
one quota, extended through the form. Plan capacity honestly.

## 3. The quota wall — the numbers that shape everything

Defaults (re-verify): **10,000 units/project/day**; `videos.insert` =
**1,600 units** regardless of file size; `videos.update` = 50;
`channels.list`/`videos.list` = 1; Analytics API is a separate, cheap
quota.

| Scale | Uploads/day | Units needed | Fits default 10k? |
|---|---|---|---|
| 10 accounts × 1 clip | 10 | 16,000 | **NO** (≈6 uploads max) |
| 10 accounts × 3 clips | 30 | 48,000 | NO |
| 50 accounts × 1 clip | 50 | 80,000 | NO |
| 100 accounts × 2 clips | 200 | 320,000 | NO |

Consequences:

- The quota-extension request is not optional polish — **it gates the
  10-account goal on day one**. File it with the verification.
- The app must treat quota as a budget: a `quota_spent_today` counter
  (resets midnight Pacific — YouTube's quota day), shown in the Publish
  UI ("14/50 uploads left today"), and the queue holds jobs that don't
  fit today for tomorrow — **quota-aware scheduling**, not fail-on-403.
- A 403 `quotaExceeded` mid-batch = pause the queue until reset, notify,
  never drop jobs.

## 4. Multi-account OAuth + token lifecycle

- **Flow**: system browser → Google consent with
  `prompt=select_account consent` (forces the account chooser each time,
  so connecting account #7 doesn't silently reuse account #6's session;
  Google's chooser also surfaces Brand Account channels) → redirect to
  `http://127.0.0.1:<ephemeral>` caught by a `std::net::TcpListener`
  (no async runtime — the ADR 0041 precedent) → exchange code (PKCE) →
  `channels.list(mine=true)` to capture channel id/title/thumbnail →
  registry row appears.
- **Practical tip for bulk onboarding**: connect accounts from separate
  browser profiles (or the chooser's "Use another account") — Google
  session juggling is the only fiddly part; 10 accounts ≈ 15 minutes.
- **Storage**: `workspace/publish/accounts.json` (registry, no secrets)
  + `workspace/publish/tokens/<account_id>.bin` — refresh token
  encrypted with **Windows DPAPI** (user+machine bound). Both
  gitignored. The OAuth client secret for a Desktop client is not truly
  secret (Google's own docs say so) but keep it out of git anyway.
- **Token health is an ops surface at N accounts**: tokens die on
  revocation, password change, 6-month disuse, or Google security
  sweeps. The registry row shows: 🟢 valid (refresh succeeded recently)
  / 🟡 refresh failing, re-auth soon / 🔴 dead, reconnect. A "Test all"
  button refreshes every token serially and updates dots — run it
  before a big batch. A publish job against a dead token fails THAT
  account's job only, never the batch.

## 5. Upload pipeline (per account × clip)

Resumable upload, the only sane choice on consumer internet:

1. `POST videos.insert?uploadType=resumable` with metadata JSON →
   returns a session URI (valid ~24 h — enough to survive restarts if
   the queue persists the URI).
2. `PUT` the file in chunks (256 KB-multiple; 8 MB default) with
   `Content-Range`; on interrupt, `PUT` empty + `Content-Range: bytes
   */total` to ask the server's offset, resume from there. Retries with
   exponential backoff on 5xx/408; a 308 is normal per-chunk progress.
3. Response = video id → store in the publish record.

Metadata per upload (the picker/profile fills these):

- `snippet.title` (≤100 chars — the judge's hook-first title seeds it),
  `snippet.description` (≤5,000), `snippet.tags`, `snippet.categoryId`
  (24 "Entertainment" default), `snippet.defaultLanguage` /
  `defaultAudioLanguage` (per-Creator: id/en/ja — the ADR 0016 store).
- `status.privacyStatus` (`public` / `unlisted` / `private`),
  `status.publishAt` (RFC3339, requires `private` — §6),
  **`status.selfDeclaredMadeForKids` (REQUIRED — COPPA; default false,
  per-account override in the registry)**.
- Shorts need no flag: vertical + ≤ ~3 min auto-classifies. Our exports
  are 1080×1920 and clip lengths cap at 180 s — every export qualifies
  (re-verify the current Shorts length ceiling; it moved once already).
- Custom thumbnails (`thumbnails.set`) need a phone-verified account —
  mostly irrelevant for Shorts; skip in P1.

## 6. Scheduling = native `publishAt` + stagger (the multi-account win)

YouTube schedules server-side: upload tonight as `private` with
`status.publishAt = tomorrow 18:00` → YouTube flips it public on time,
**app closed or not**. This is the backbone:

- **Stagger policy**: posting one clip to 30 channels at the same second
  looks like a botnet and burns quota in one burst. A posting profile
  carries `stagger`: first at T, then +Δ per account (Δ = 5–15 min,
  jittered ±90 s), optionally shuffled account order. The app computes
  each account's `publishAt` at queue time — uploads can still run
  back-to-back serially; only the go-public times spread.
- **Upload-time spreading**: at 100 accounts × 50 MB ≈ 5 GB of PUTs —
  serial on the existing worker is fine overnight; the queue runs while
  the app is open and picks up where it left off (persisted session
  URIs) after a restart.
- **Local due-queue** exists only as a fallback (e.g. "upload AND go
  live only when I click") — YouTube needs none of the local-timer
  machinery TikTok/IG will.

## 7. UI surfaces

- **Accounts page** (left rail, or a Diagnostics sibling): registry rows
  (avatar, label, channel title, groups, health dot, quota-today chip,
  Connect/Reconnect/Remove). Search box + group filter appear the day
  the list exceeds one screen — build them at 10, they're cheap.
- **Export modal** gains a **Publish** section: posting-profile dropdown
  + the account checkbox list (filtered by search/group; "select group"
  bulk toggle), per-clip title/description (seeded from the judge title
  + profile template with `{title}` `{creator}` `{hashtags}` slots),
  schedule picker (Now / publishAt + stagger), and the quota meter.
- **Published page**: rows = clip × account, with status
  (uploading n% / scheduled t / live / failed reason), video link, and
  metrics chips once analytics lands (§8). Filter by clip, account,
  group, status. This page is also the retry surface (per-row Retry).
- **Failure UX**: per-row error strings verbatim from the API
  (`quotaExceeded`, `uploadLimitExceeded` — yes, channels ALSO have
  their own daily upload caps YouTube doesn't publish numerically;
  a new/unverified channel tolerates far fewer uploads/day than an
  aged one — surface it, don't hide it).

## 8. Analytics (per channel, aggregated)

- **YouTube Analytics API** per connected account (add its scope to the
  same consent — one more reason to finish verification early):
  `reports.query(ids=channel==MINE, metrics=views,likes,comments,shares,
  estimatedMinutesWatched,averageViewPercentage, dimensions=video,
  filters=video==<ids>)`. Cheap quota, separate from Data API.
- **Refresh model**: user-initiated ("Refresh" on the Published page —
  fits the Offline term) + optional refresh-on-app-open for accounts
  with live posts < 7 days old. Each refresh appends a snapshot
  `{ts, views, likes, comments, shares, watch_min}` per (account,
  video) to `published.json` — append-only history → sparklines.
- **Aggregation views**: per clip across accounts (which channel's
  audience liked it), per account across clips (which channels are
  growing), per group. egui_plot sparklines; no external charting.
- Public `videos.list(statistics)` is a 1-unit fallback for view counts
  without Analytics scope — useful before verification completes.

## 9. Policy + operational risk (stated plainly, operator's call)

- **Duplicate/spam exposure**: the same clip posted identically across
  many owned channels is exactly the pattern YouTube's spam/duplication
  enforcement looks for; channel networks get terminated in sweeps.
  Mitigations that are also good practice: per-group content strategies
  (different clips per channel group, not 1 clip × 100), per-channel
  titles/descriptions (the template slots), staggered publishAt, aged
  accounts. The tool makes targeting easy; the strategy stays human.
- **Unverified-project private lock** (§2) — dev-time posts stay
  private; don't misread it as a bug.
- **Per-channel upload caps** exist independently of API quota (§7).
- **One IP, many accounts** is normal for a studio/network and fine by
  itself; combined with identical-content spam it strengthens the
  pattern. Same mitigation as above.
- Nothing here uses unofficial APIs, cookies, or scraping — everything
  is the official Data/Analytics API under the operator's own grants.
  That is deliberate: it is the only version of this that survives at
  100 accounts.

## 10. Data model (all under `workspace/publish/`, gitignored)

```jsonc
// accounts.json
{ "accounts": [ {
    "id": "acc_8f3a",                  // internal, stable
    "channel_id": "UC…", "title": "Klip Podcast ID",
    "label": "ID clips #1",            // operator's name for it
    "groups": ["id-clips"],
    "made_for_kids": false,
    "connected_at": "2026-07-08T…", "token_health": "ok|stale|dead",
    "quota_note": "aged channel"       // free-text ops notes
} ] }

// profiles.json
{ "profiles": [ {
    "name": "ID network evening",
    "account_ids": ["acc_8f3a", "…"],  // or "groups": ["id-clips"]
    "title_template": "{title} #shorts",
    "description_template": "{title}\n{hashtags}\n{credit}",
    "privacy": "private", "publish_at": "next 18:00 local",
    "stagger_min": 10, "jitter_s": 90, "shuffle": true
} ] }

// queue.json — pending/active publish jobs (survives restart)
{ "jobs": [ {
    "clip": "workspace/…/Judul Klip.mp4", "account_id": "acc_8f3a",
    "meta": { "title": "…", "description": "…", "publish_at": "…" },
    "state": "queued|uploading|done|failed|quota_hold",
    "session_uri": "https://…",        // resumable-upload resume point
    "video_id": null, "error": null, "attempts": 1
} ] }

// published.json — the permanent record + metric snapshots
{ "posts": [ {
    "clip": "…", "account_id": "acc_8f3a", "video_id": "dQw…",
    "published_at": "…", "snapshots": [
      { "ts": "…", "views": 1042, "likes": 88, "comments": 7,
        "shares": 3, "watch_min": 512 } ]
} ] }
```

## 11. Code layout + job flow (this codebase)

```
crates/app/src/publish/
  mod.rs      — Account, Profile, PublishJob, registry load/save (write_atomic)
  oauth.rs    — PKCE + select_account consent + 127.0.0.1 listener + DPAPI
  youtube.rs  — resumable insert, publishAt, channels.list, analytics query
  quota.rs    — unit costs, daily budget, midnight-Pacific reset
```

- `Job::Publish { job_id }` on the existing serial worker; progress via
  `Progress::Publish { account, frac }`; Cancel aborts the current PUT
  chunk and re-queues (session URI keeps the offset). Uploads are
  network-bound, not GPU — if serial ever feels slow, the grill can
  revisit 2–3 parallel uploads, but serial + overnight + publishAt
  stagger likely never needs it.
- The Accounts page reuses the Diagnostics row idioms; the quota meter
  reads `quota.rs`.
- Everything hides behind a `publish` cargo feature until the gate
  passes (the `correct`/`enh`/`sep` precedent).

## 12. Build phases (each a gated session, grill first)

- **P1a — one account, end to end**: Cloud project + consent screen
  (publish it), Connect flow, DPAPI storage, resumable upload of a real
  rendered Short as PRIVATE (verification pending = private anyway),
  registry row + health dot. Gate: a clip uploaded from the Export modal
  appears on the channel (private), resumable survives a mid-upload
  app kill. **File verification + quota extension the same day.**
- **P1b — multi-account + picker**: N accounts, groups, Export-modal
  checkbox picker + templates, per-account jobs + per-row failures,
  quota budget + `quota_hold`. Gate: one render → 3 accounts, 3 private
  posts, one deliberately-dead token fails alone.
- **P1c — scheduling**: publishAt + stagger/jitter/shuffle in profiles;
  queue survives restart mid-batch. Gate: 3 posts go public unattended
  at staggered times with the app closed.
- **P1d — Published page + analytics**: snapshots, sparklines,
  aggregation, Refresh. Gate: real metrics on real posts on the
  operator's eyes.
- **P1e — verification lands**: flip a test post public via API,
  confirm no private-lock; raise real volume gradually per §9.

## 13. Open questions for the grill

- The Offline term's exact re-wording (scheduled publicization happens
  server-side — is upload-at-click + publishAt enough, or is a local
  due-queue also wanted?).
- Account groups: flat tags (proposed) vs hierarchy.
- Per-account language defaults source: Creator store vs account row.
- Does the judge title get per-profile LLM rewriting (per-channel
  uniqueness helps §9) — later slice?
- Retry policy numbers (attempts/backoff) — steal ADR 0028's?
- 100-account onboarding ergonomics: is 15 min of browser account-
  switching acceptable, or is an import/re-auth-all flow needed?

## 14. Re-verify before building (early-2026 knowledge)

- Quota unit costs + default (10k) + the extension form's current shape.
- Shorts length ceiling (was 60 s → ~3 min in late 2024; check today).
- Consent-screen "Testing" 7-day token expiry; verification
  requirements for `youtube.upload`; the unverified private-lock rule.
- Resumable session URI lifetime (~24 h) and chunk-size rules.
- `publishAt` precision/limits (min lead time, max horizon).
- Brand-account channel selection behavior in the OAuth chooser.
- Per-channel daily upload caps behavior for new/unverified channels.
