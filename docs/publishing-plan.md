# Direct publishing + scheduling + analytics — design notes (TikTok / YouTube / Instagram / Facebook / X)

Status: RESEARCH DOC (2026-07-07, API knowledge as of early 2026 — every
endpoint/limit below MUST be re-verified against the platform's current
docs before building; these APIs drift quarterly). Not an ADR — when a
phase is picked, open it with /grill-with-docs and pin the decisions
there.

## The goal

Close the last gap to the SaaS clippers: a rendered clip goes from the
Studio's Export straight to the operator's accounts — posted now or
scheduled — and a **Published** page shows per-clip performance
(views/likes/comments/shares) across platforms.

## Fit with the app's constraints

- **Offline term (CONTEXT.md)**: network use is user-initiated only.
  Posting a clip and refreshing analytics are user-initiated — same class
  as ingest and Downloads (ADR 0041 widened the term once; publishing
  widens it again — re-word the term in the grill BEFORE building).
- **Scheduling nuance**: a locally-scheduled post fires while the app is
  open (the serial worker checks a due queue) — the app does NOT run as a
  service. Prefer PLATFORM-NATIVE scheduling wherever it exists (YouTube,
  Facebook) so the post publishes even with the app closed; local-timer
  fallback for the rest (TikTok/IG/X), which simply fires the moment the
  app is next open past the due time.
- **No async runtime** (ADR 0041's choice): all five platforms are plain
  HTTPS REST — `ureq` (already a dep) + chunked/resumable upload loops on
  the existing serial worker (`Job::Publish`), Cancel + progress riding
  the existing channel. OAuth needs one tiny local listener
  (`std::net::TcpListener` on `127.0.0.1`) for the redirect — no tokio.
- **Token storage**: `workspace/publish/tokens.json`, encrypted at rest
  with Windows DPAPI (`CryptProtectData` — machine+user bound), never
  committed (gitignore). Refresh tokens are long-lived credentials;
  treat like passwords.

## The honest platform reality (verify before building)

| Platform | Upload API | Native schedule | Analytics API | Gatekeeping (the real cost) |
|---|---|---|---|---|
| **YouTube** | Data API v3 `videos.insert`, resumable; Shorts = vertical ≤ ~3 min | **YES** — `status.publishAt` (+ private) | YouTube Analytics API (rich: views, watch time, retention) | OAuth verification: **unverified API projects upload PRIVATE-locked videos** → an API audit/verification pass is required for public posting. Default quota 10,000 units/day; `videos.insert` ≈ 1,600 → ~6 uploads/day until a quota raise. |
| **TikTok** | Content Posting API — Direct Post or Inbox/Draft; `FILE_UPLOAD` chunked (PULL_FROM_URL needs a verified public URL — not us) | NO — schedule locally | Display API: per-video stats (views/likes/comments/shares) | **App audit**: unaudited clients can only post PRIVATE/self-visible. Direct-Post approval needs a developer app, demo video, privacy policy + ToS URLs. Apply EARLY — weeks of lead time. |
| **Instagram** | Graph API Reels: media container + publish; resumable upload protocol (or public `video_url`) | Container-then-publish (containers expire ~24 h) → effectively local scheduling | Insights API per media (plays, reach, saves, shares) | **Professional (Business/Creator) IG account required**, linked per the API flavor. Solo-operator path: your own account with an app role (tester/admin) works in dev mode WITHOUT full App Review; full review only if shipping to others. ~25 API posts / 24 h cap. |
| **Facebook** | Pages Graph API: `/{page}/videos` (resumable) + `/video_reels` (phased upload) | **YES** — `scheduled_publish_time` (unpublished post) | Page/video insights | Requires a **Page** (personal profiles can't API-post). Same Meta dev app as IG; dev-mode with your own admin account avoids review for solo use. |
| **X (Twitter)** | v1.1 chunked media upload (INIT/APPEND/FINALIZE, async video processing) + v2 `POST /2/tweets` | NO — schedule locally | `public_metrics` on own tweets (views/likes/reposts/replies) | **Pricing is the hurdle**: Free tier = tiny write allowance (~500 posts/mo, negligible reads); useful read access (analytics) starts at Basic (~$100/mo, as of early 2026 — RE-CHECK, X repriced repeatedly). Decide if X is worth it. |

Ranked by API friendliness for THIS use case: **YouTube ≥ Facebook > Instagram > TikTok > X.**

## Per-platform notes (what bites)

### YouTube (your primary — do it first)
- OAuth 2.0 **installed-app loopback flow** (`http://127.0.0.1:<port>`
  redirect) is first-class for desktop — the cleanest of the five.
- While the Google Cloud project's consent screen is in "Testing",
  refresh tokens **expire every 7 days** (weekly re-login). Publishing
  the consent screen (even unverified) fixes token life, but *upload*
  scope on an unverified project = private-locked videos → plan the
  verification pass (privacy policy page, scope justification video).
- Shorts need no special endpoint: vertical + short = Short.
  `snippet.title/description/tags`, `status.privacyStatus`,
  `status.publishAt` (RFC3339) for native scheduling.
- Analytics: `youtubeAnalytics.reports.query` per video id — views,
  estimatedMinutesWatched, averageViewPercentage; day granularity.

### TikTok
- Developer portal app → `video.publish` scope (Direct Post) +
  `video.list`/`user.info.stats`-class scopes for analytics.
- Direct Post flow: `POST /v2/post/publish/video/init/` (declares
  `chunk_size`/`total_chunk_count`) → PUT chunks → status poll. Caption,
  privacy, duet/stitch flags in the init payload.
- **Until the app passes audit, posts are forced private** — build +
  test unaudited (private posts), flip to public after approval.
- No native scheduling → local due-queue.

### Instagram (Reels)
- Needs IG **Professional** account. Two API flavors as of early 2026:
  "Instagram API with Instagram Login" vs via Facebook Login + linked
  Page — pick per current docs; the Page-linked flavor shares the
  Facebook dev app (one Meta app serves IG + FB).
- Publish flow: create media container (`media_type=REELS`, caption,
  cover) with the **resumable upload** protocol (local file — the
  `video_url` variant needs a public URL we don't have) → poll container
  status → `media_publish`.
- Meta OAuth redirect must be **HTTPS** — the desktop workaround is the
  manual flow against `https://localhost` with a self-signed cert, OR
  run the auth step in the system browser and paste the code (one-time
  per ~60-day long-lived token refresh; acceptable for solo use).
- Insights: `GET /{media-id}/insights?metric=plays,reach,likes,comments,shares,saved`.

### Facebook (Page)
- Reels: `/{page}/video_reels` start/upload/finish phases; regular video
  posts via `/{page}/videos` with `published=false` +
  `scheduled_publish_time` for native scheduling.
- Page access token derived from the user token (`pages_manage_posts`,
  `pages_read_engagement`, `read_insights`).

### X
- v1.1 `media/upload` chunked (APPEND ≤ 5 MB chunks; `media_category=tweet_video`;
  poll `STATUS` until processing succeeds) → `POST /2/tweets` with
  `media.media_ids`. OAuth 2.0 PKCE (loopback redirect works).
- Video ≤ 140 s on standard accounts (longer needs Premium) — most clips
  fit, but check per-account.
- Recommendation: implement LAST, behind the same trait; decide with
  real pricing in front of you whether analytics (paid tier) is worth it
  vs post-only on the free tier.

## Architecture in this codebase

```
crates/app/src/publish/
  mod.rs        — Publisher trait + PublishJob/PublishRecord types
  oauth.rs      — PKCE helper + 127.0.0.1 TcpListener redirect catcher
  youtube.rs    — resumable insert + publishAt + analytics query
  tiktok.rs     — chunked direct-post + status poll + display stats
  meta.rs       — shared Meta client; ig.rs / fb.rs on top
  x.rs          — chunked v1.1 upload + v2 tweet + public_metrics
```

- **`trait Publisher`**: `auth() / ensure_fresh_token() / upload(clip,
  meta, schedule) -> PostId / fetch_metrics(PostId) -> Metrics`. One
  impl per platform, registered like `download_specs()` — the
  Diagnostics-registry pattern (a row per connected account: dot =
  token present + unexpired; Connect button runs the OAuth flow).
- **`Job::Publish`** on the existing serial worker (uploads never race
  a render for the GPU… they don't use the GPU, but serialization keeps
  progress/Cancel semantics uniform — revisit in the grill if parallel
  uploads matter).
- **Publish queue**: `workspace/publish/queue.json` —
  `{clip_path, platforms, caption, hashtags, due_at, status}`; the UI
  ticks it while open; native-scheduling platforms convert `due_at` to
  the platform's field at upload time and mark done immediately.
- **Publish records**: `workspace/publish/published.json` — clip →
  per-platform post ids + timestamps; the analytics fetch appends
  `{ts, views, likes, comments, shares}` snapshots per post
  (append-only history → trend lines).
- **UI**: Export summary modal gains platform checkboxes + caption box +
  "Post now / Schedule at…"; a new **Published** left-rail page lists
  posted clips × platforms with metrics chips + a Refresh button
  (user-initiated fetch), sparklines via egui_plot.
- **Caption/title source**: the judge already writes hook-first titles —
  seed the caption box from it + per-platform hashtag presets stored on
  the Creator (ADR 0016 pattern).

## Suggested phasing (one gated slice each, grill first)

1. **P1 — YouTube** (highest value, best API, native scheduling): OAuth
   loopback + resumable upload + publishAt + the Published page skeleton
   with YT analytics. Includes the Offline-term rewording + token
   storage + registry rows. *Start the Google verification paperwork the
   same week — it gates public posts, not development.*
2. **P2 — TikTok**: apply for the developer app + audit IMMEDIATELY
   (longest lead time), build against private-post mode meanwhile.
3. **P3 — Instagram + Facebook** (one Meta app, shared client): FB Page
   first (easier, native scheduling), IG Reels second.
4. **P4 — X**: post-only on free tier unless the paid tier's analytics
   earn their cost.
5. **P5 — Analytics polish**: snapshot scheduler-on-open, trend charts,
   per-Creator rollups.

## Operator prerequisites checklist

- [ ] Google Cloud project + OAuth consent screen + YouTube Data/Analytics
      APIs enabled; start verification (needs a privacy-policy URL — a
      GitHub Pages one-pager suffices).
- [ ] TikTok developer account + app; submit for Content Posting audit.
- [ ] IG account switched to Professional; Meta developer app; (if
      Page-linked flavor) a Facebook Page + link IG↔Page.
- [ ] X developer account; decide Free vs Basic.
- [ ] A published privacy-policy/ToS page (all five ask for it).

## Open questions for the grill (do not decide here)

- Offline-term wording for scheduled posts firing without a fresh click.
- Serial vs parallel uploads; retry policy (yt-dlp's ADR 0028 retry
  pattern probably transfers).
- Where captions/hashtags live per platform (per-Creator presets vs
  per-clip overrides — likely both, ADR 0016/0031 layering pattern).
- Metrics retention + chart scope (per clip? per Creator? per platform?).
- Failure UX: a scheduled post that fails while the app is closed
  (native-scheduled ones can't fail this way — another reason P1=YT).
