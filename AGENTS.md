# BlindRSS (Rust) Architecture & Dev Guide

This file mirrors the Python app's `agents.md` in structure and intent,
adapted to the Rust port. Where a Python rule has a Rust counterpart, the
rule carries over; where the port deliberately differs, the difference is
stated here and in `docs/PARITY.md`.

## Working Agreement (read first)
- Treat this file as the project source of truth. If code and this guide
  disagree, verify the code, fix the guide, and keep the operational rule
  here.
- The Python app (serrebidev/BlindRSS) is the behavioural spec. When in
  doubt about what a feature should do, read the Python code first, then
  port the behaviour. `docs/PARITY.md` is the governing parity checklist:
  every batch updates it, and nothing is declared "ported" without its
  resolution paragraph there.
- Install whatever you need to get the job done.
- Debug and test your changes; add or extend tests for any behaviour
  change. A bug report from the user gets a reproduction and regression
  tests as standard practice, not a fix by inspection.
- Fix any warnings or errors you hit along the way: the gate is
  `cargo clippy --workspace --all-targets -- -D warnings` at zero.
- Keep this file current when something here goes stale.
- Never edit `.github/workflows/` without the owner's direct OK.

## System Overview
- Stack: Rust (edition 2024), wxDragon 0.9.24 (wxWidgets GUI), SQLite via
  rusqlite (bundled), feed-rs for feed parsing, ureq (rustls) for HTTP.
- Workspace: `app/` is the `blindrss` binary (GUI: `main.rs`, `player.rs`,
  `settings.rs`, `dialogs.rs`, `help.rs`, `keys.rs`, `hotkey.rs`,
  `startup.rs`); `crates/core/` is the GUI-free `blindrss-core` library.
  Keep core GUI-free: anything testable without a window belongs there.
- Data compatibility is a hard contract: the app opens the Python app's
  existing `rss.db` and `config.json` in place. No re-setup, no
  destructive migration; Python and Rust builds are dual-run safe on the
  same data directory.
- App version source: `version` in `app/Cargo.toml`.
- Entry: `app/src/main.rs` builds the main frame, defers initial tree
  load and background work, and drains worker results through a 150 ms
  timer (`RefreshMsg`). State lives in `Rc<RefCell<State>>` on the UI
  thread; workers never touch widgets.

## Build & Release
- Gate before any push that will be tagged: `cargo fmt` (run LAST, after
  the final code edit), clippy at zero warnings (workspace, plus
  `crates/core` for `x86_64-pc-windows-gnu` when `cfg(windows)` code was
  touched), `cargo test --workspace` fully green, `cargo build -p
  blindrss`.
- Ship a release:
  1. Bump `version` in `app/Cargo.toml` to the next number above the
     latest PUBLISHED GitHub release AND the latest tag. Never reuse or
     re-tag an older version, even one whose release failed.
  2. Push to `main` and wait for the CI workflow to go fully green on
     all OSes. Read the run's `conclusion` field directly (via
     `gh api`/`gh run view`); never infer success from a piped watch
     command's exit code.
  3. Tag `vX.Y.Z` on the green commit and push the tag. `release.yml`
     builds the three platform zips and `SHA256SUMS`, then publishes.
  4. Verify by direct API read: the release exists, `draft=false`, all
     four assets are present, and `/releases/latest` returns the new
     tag. A handoff or script summary is not verification.
- Artifacts: `BlindRSS-vX.Y.Z-Linux.zip`, `BlindRSS-vX.Y.Z-macOS.zip`,
  `BlindRSS-vX.Y.Z-Windows.zip`, `SHA256SUMS`. Tag pushes also trigger a
  duplicate CI run on the tag ref; that is harmless.
- Pushing to `main` cancels the in-flight main CI run. Do not push
  mid-verification unless the new commit is the one being verified.
- Signing: releases use the previous (unsigned/development) method until
  the owner says otherwise. SignPath scope, when it lands, is BlindPilot
  and BlindTerm only — not this app.
- CI cache gotcha (learned the hard way): the rust-cache round-trip can
  drop `WebView2LoaderStatic.lib` from the wxWidgets CMake tree while
  cargo treats wxdragon-sys as fresh, failing Windows links with
  LNK1104. The workflows carry a guard step that cleans `wxdragon-sys`
  and `wxdragon` when the lib is missing after cache restore, plus
  `cache-on-failure: false`. Do not remove either.
- Updater visibility: the in-app self-update reads only the latest
  published GitHub release. A release that stays a draft, or is not
  Latest, reaches no one — the publish + verify step above is part of
  shipping, not an optional extra.

## Casting
Casting is Caster's engine (github.com/serrebidev/Caster), ported — never
re-implemented from scratch. The Python app syncs Caster's code with
`tools/sync_caster.py`; this port carries the same engine in
`crates/core/src/cast.rs`, `castwire.rs`, `castsession.rs`,
`castupnp.rs` (Chromecast, UPnP/DLNA) and `raop.rs` (AirPlay RAOP v2
audio sender). Behavioural questions are answered by Caster and the
Python `core/casting.py`, in that order.
- Local files, localhost URLs and header-protected URLs reach receivers
  through the local media proxy (`crates/core/src/proxy.rs`).
- Live tests may only target R&B Room (Cast name "RB Room").

## File Structure & Responsibilities

### `crates/core/` (GUI-free library)
- `db.rs`: SQLite schema setup/migrations, connection helpers, all
  article/feed/category queries.
  - Schema DDL uses `IF NOT EXISTS`; the migration loop in schema init
    is the ONLY `ALTER TABLE` site in the codebase.
  - Schema init runs inside `BEGIN IMMEDIATE` with
    `PRAGMA busy_timeout=5000`. The previous check-then-ALTER pattern
    raced when two connections opened an unmigrated database
    ("duplicate column name") — that failure shipped in the v0.1.30 tag
    CI and again in v0.1.33's macOS release run. Do not reintroduce
    check-then-act schema changes.
  - "Ensure" helpers follow the same rule: `ensure_category_path` is
    `INSERT OR IGNORE` plus a re-read of the winning row, never
    SELECT-then-INSERT.
  - Category identity is the full path string (`Parent / Child`) in
    storage; the UI may display only the leaf. The system category is
    always the English sentinel `Uncategorized` in storage, provider
    calls, OPML and config; translate only at display time.
  - Category article views use the ported membership clause
    (`articles_for_category`): feed category minus moved-away, plus
    moved-here via `category_override`, plus labeled via
    `article_labels`. Never a raw `feeds.category IN (...)`.
  - Local article deletion is persistent: a `deleted_articles`
    tombstone is recorded before the row is removed, and refresh skips
    tombstoned item IDs/URLs. The "purge" delete behaviour keeps the
    tombstone with `purged=1`; delete-to-category does NOT tombstone,
    it sets `category_override`.
  - Retention and refresh share one boundary (Python issue #106): the
    cleanup sweep deletes past the window, so every article insert path
    must refuse items outside retention or each refresh resurrects
    purged items as unread.
- `config.rs`: `config.json` load/save.
  - A UTF-8 BOM is stripped on load. An unparseable or non-object
    config is copied to `<name>.unreadable.json` and loads as empty;
    `save()` never writes a non-object back over the user's file
    (v0.1.32 data-safety fix). Never stream a partial write into the
    live config.
- `feeds.rs` / `refresh.rs`: feed parsing (RSS/Atom/JSON Feed via
  feed-rs plus the ported compatibility handling) and parallel refresh
  with conditional GET (ETag/Last-Modified), revalidation headers,
  finite timeouts, and per-feed error recording. Feed-format
  compatibility is part of refresh, not discovery only: bad dates,
  wrong MIME types, missing GUIDs, HTML in descriptions, relative
  links and podcast enclosures are the normal case. When an entry has
  no stable ID/link, generate the deterministic feed-scoped ID the
  Python parser generates.
- `sources.rs`: the source-feed family — subscriptions with no native
  RSS (Odysee, Rumble, YouTube search, SoundCloud, Mixcloud, Google
  Groups, Groups.io search) enumerated at refresh in the branch order
  of Python's `providers/local.py::_refresh_single_feed`, plus the
  YouTube channel uploads fallback/history backfill and the NPR/Sky
  audio attach. Rumble/Odysee feed URLs are normalized and persisted
  back to the feeds row at refresh, as Python does.
- `extract.rs` + `postprocess.rs` + `article_html.rs`: full-text
  extraction. The cascade is fixed: rust-trafilatura base, dom_smoothie
  (Readability) second opinion, and the ported BlindRSS gate/site layer
  owns every final call. `ytfull.rs` reconstructs YouTube watch pages
  through yt-dlp (Description / Chapters / Subtitles / comment trees);
  `threads.rs` rebuilds Lemmy/GitHub/Reddit/Discourse threads and
  GitHub commit/release/discussion pages. Both readers follow
  multi-page articles (`_find_next_page` rules) and merge with
  `_merge_texts` semantics; direct MP3 URLs resolve ID3 show notes.
  - Never replace fuller feed/article text with a shorter scrape, a
    translation failure, or a bot-interstitial page. When web
    extraction returns less than the feed already has, the feed wins.
  - The Python site fixes are ported, not dropped: BBC, Sky (including
    the Akamai block-page detection and Flourish table expansion),
    Bloomberg, Axios `__NEXT_DATA__`, The Register story-`<article>`
    selection, GSMArena battery-table completeness and sort order,
    Reddit/Lemmy thread reconstruction, forum linearization, and the
    per-host no-pagination-follow list. New site behaviour is
    researched against the Python implementation and live evidence
    before it is declared unportable.
  - `article_html.rs` renders the Rich View document through an ammonia
    sanitizer pass; raw embedded HTML (e.g. ID3 show notes) only ever
    reaches the reader through that sanitizer.
- `providers.rs` / `provider.rs`: hosted providers (Miniflux,
  Inoreader, The Old Reader, BazQux) behind one interface, mirroring
  the Python providers' semantics. Do not trust one provider shape:
  article IDs, chapter APIs, category behaviour and favorite support
  differ per provider. Provider HTTP requests always use finite
  timeouts, and failures surface as user-safe diagnostics, never as a
  silently empty account.
- `media.rs` / `media_url.rs` / `play_cache.rs`: playability decisions
  and the playback download cache. `is_playable_media` covers direct
  files plus HLS/DASH/TS streams (`.m3u8`/`.mpd`/`.ts` and stream MIME
  types). Activation parity with Python's `_should_play_in_player`:
  downloaded file, yt-dlp-supported URL (image extensions rejected),
  playable enclosure, then media detected in article content.
- `proxy.rs`: the local media proxy. `/stream` serves progressively in
  chunks (never buffer a whole file before the first byte); `/play`
  transcodes through ffmpeg to MP3 for backends that refuse podcast
  CDN URLs.
- `rangecache.rs`: the range-cache proxy (Python
  `core/range_cache_proxy.py`, Batch 36). A second, loopback-only
  listener serves /media?id=<sid> from an on-disk segment cache whose
  layout is byte-for-byte Python's (sha256 URL directories,
  zero-padded chunk files, mappings/), so both apps share one cache
  directory. Entries probe once (single-flight; handlers wait via
  await_probe rather than answering without the real total), stream
  misses from the origin while caching them, re-resolve expired
  signed URLs and retry once, and a background downloader fills from
  the play head. `maybe_range_cache_url` is the player gate (Python
  `_maybe_range_cache_url`): the player's primary load goes through
  it, the /stream and /play fallbacks stay behind it.
- `dependency.rs`: external tool resolution and installation
  (yt-dlp, ffmpeg, ffprobe). Detection order: user-chosen path from
  Settings, then the managed runtime copy, then PATH, then known
  package locations. ffmpeg/ffprobe install automatically in the
  background when missing (winget/Homebrew/static-build routes per
  platform) and are validated by running them; the UI reports progress
  in the status bar instead of showing a dead-end dialog (v0.1.27).
  - yt-dlp stays a wrapped executable, never a library port, and must
    always be current — a stale yt-dlp stops working outright.
  - The extraction crates follow the same bleeding-edge intent as
    Python's trafilatura rule: track current rust-trafilatura /
    dom_smoothie releases; extraction quality is the product.
- `youtube_account.rs` / `oauth.rs`: YouTube TV device-code sign-in.
  Only the refresh token is stored (config key
  `youtube_account_refresh_token`); never log it. Empty/changed
  responses fail closed and preserve the previous recommendations
  cache.
- `cookies.rs` / `cookies_import.rs` / `chromium.rs`: site-cookie jar
  and browser cookie import, mirroring the Python consent model:
  automatic import copies only clearance-shaped cookies; full-jar
  import happens only through the explicit user action. Chromium v20
  app-bound cookies need the privileged component and remain a
  documented non-port in PARITY.md.
- `rules.rs`: the Filter Rules pipeline port (ordered rules, pure
  evaluation, one DB-writing apply step; the abstract "delete" action
  resolves through the delete behaviour exactly as in `db.rs` above).
- `cast.rs` family and `raop.rs`: see **Casting**.
- `equalizer.rs`: 10-band EQ band definitions (31.25 Hz…16 kHz,
  engine-set) and user-preset helpers. Band count and frequencies are
  fixed by the playback engine; do not invent a different grid.
- `silence.rs`: ffmpeg RMS silence detection for Skip Silence (the
  WebRTC VAD fallback position from Python; see PARITY.md).
- `sponsorblock.rs`: SponsorBlock lookup uses only the hash-prefix
  endpoint (never the bare video id) and returns no segments on any
  failure.
- `translate.rs` / `translation_updates.rs` / `i18n.rs`: optional
  article translation (off by default; API keys live only in local
  `config.json`, never logged) and gettext catalog handling. Downloaded
  translation overrides win only within the same app version and stand
  down entirely when the override root resolves to the bundled tree.
- `maintenance.rs`: backup/restore and self-update staging. Restore
  stages beside the database the app actually opens and swaps at
  relaunch (a restore that targets a path the app never opens silently
  no-ops — fixed v0.1.32). Self-update never deletes a staged update
  when a rename fails.
- `discovery.rs`, `takeout.rs`, `opml.rs`, `download.rs`,
  `archive.rs` (podcast archive recovery), `enrich.rs` (page-metadata
  enrichment — fills only empty author fields, unions tags, never
  issues its own network requests), `shortcuts.rs` (the pure,
  GUI-free shortcut registry), `windows_shortcuts.rs` (the
  PowerShell/WScript.Shell `.lnk` creation port behind the
  Add Shortcuts dialog), `single_instance.rs`, `startup.rs`
  helpers, `announce.rs`, `help.rs`/`help_topics.rs`: ports of the
  same-named Python responsibilities; the Python module is the spec
  for each.

### `app/` (GUI binary)
- `main.rs`: main window — feed tree, article list, reader pane, menus,
  refresh orchestration.
  - All background results arrive as `RefreshMsg` values drained by the
    150 ms timer; workers post messages, they never touch widgets or
    the `State` directly.
  - Menu dispatch goes through the `menu_actions` table; ids must be
    unique — on a duplicate id the FIRST match wins and the second
    handler is silently dead.
  - Tree item data strings encode the view: `""` = all feeds, a feed
    id, `cat:<path>`, `smart:`/`smart:<id>`. `cat:Uncategorized` is the
    real, protected Uncategorized node.
  - Article-list performance rules (measured, from the category-lag
    reports): refill inside Freeze/Thaw; rebuild list columns only
    when the `article_columns` config signature changes; coalesce
    tree-selection refills through `State.refill_pending` and the
    drain timer instead of refilling per arrow keypress; per-row
    annotations are precomputed off the UI path, never parsed during
    row insertion.
  - Categories start collapsed by default; `category_tree_default_expanded`
    opts into expanded, and expand/collapse state is remembered.
  - Article activation (Enter / double-click) follows Python exactly:
    mark read, then play in the player when the activation decision
    says media, else open the article link (honouring the custom open
    command in `opencmd` config).
- `player.rs`: the media player window (wx MediaCtrl today; the libVLC
  port via vlc-rs is the recorded parity endgame in PARITY.md).
  - Playback fallback chain, in order: direct load, local proxy
    `/stream`, ffmpeg `/play` transcode. A load that reports success
    but never reaches Playing is retried and escalated by the autoplay
    watchdog in the player tick (armed on every fresh load; a
    deliberate pause/stop disarms it). Load success is not readiness.
  - Accessibility contract: controls carry accessible names
    (`set_name`); the seek slider exposes its value as position text
    through the Windows `wxAccessible` implementation, which reads
    ONLY lock-free atomic snapshots of position/length. Never call
    MediaCtrl (or any UI-thread-owned object) from an accessibility
    callback — NVDA's MSAA RPC thread calling into MediaCtrl was the
    v0.1.30 freeze. Position readouts are focusable read-only text,
    rewritten only when the text changes; playback position is
    persisted at most every 5 s of movement, not per tick.
  - Play/pause state is announced (`crate::announce`) and the
    Play/Pause button label tracks the real backend state.
  - Sub-5-second clips never count position 0 as completed.
- `hotkey.rs`: the global show/hide hotkey is raw Win32 subclassing
  (wxDragon has no `EVT_HOTKEY`). The subclass must fall through to
  `DefWindowProc` when no original proc was captured — returning 0 for
  every message makes a dead window (fixed v0.1.32).
- `settings.rs` / `dialogs.rs`: settings and dialogs. Tab order and
  control order are muscle memory for screen-reader users: append new
  pages/controls, do not insert mid-list and renumber what follows.
- `startup.rs`: start-at-login registration per platform.

## Data Model (`rss.db`)
Shared with the Python app — same tables, same semantics. Highlights
(full detail in the Python guide's Data Model section, which this port
must stay compatible with):
- `feeds`: `id`, `url`, `title`, `title_is_custom`, `category`,
  `icon_url`, `etag`, `last_modified`, `show_images` (NULL = inherit
  global), `delete_behavior` (NULL = inherit global), `feed_settings`.
- `articles`: `id`, `feed_id`, `title`, `url`, `content`,
  `description`, `date`, `author`, `is_read`, `is_favorite`,
  `media_url`, `media_type`, `chapter_url`, `tags` (newline-separated),
  `category_override`.
- `categories`: `id`, `title` (FULL path), `parent_id`. Duplicate leaf
  names under different parents are valid; never simplify identity
  back to the leaf.
- `deleted_articles`: tombstones with snapshot columns and `purged`.
- `chapters` (local articles) vs `chapter_cache`/`chapter_sources`
  (hosted-provider rows with provider-scoped cache keys) — hosted
  chapter rows never go in the FK-backed local `chapters` table.
- `playback_state`, `article_labels`, `filter_rules`,
  `smart_folders`, `article_versions`, `youtube_recommendations`.

## Key Workflows

### 1. Feed Refresh
- Local refresh runs feeds in parallel; each worker uses its own DB
  connection. Conditional requests use ETag/Last-Modified plus
  `Cache-Control: no-cache` revalidation. Retention cleanup and the
  refresh insert path share the retention boundary (see `db.rs`).
- Progress and completion are coalesced into `RefreshMsg` values; a
  no-change batch must not trigger a full tree reload.
- Startup refresh never blocks the window: no network, dependency
  checks, extraction or media probing on the UI thread at startup.

### 2. UI & Threading
- Everything UI happens on the main thread; everything slow happens on
  workers that report back through the drain timer. No synchronous
  loops over articles/feeds in UI handlers; batch DB reads.
- CHAR_HOOK handlers dispatch the shortcut registry (`keys.rs` /
  core `shortcuts.rs`). Every registry command must have a handler.
  Combos are vetted for screen-reader conflicts: Ctrl+Shift with
  punctuation/digits is a landmine (system and NVDA gestures eat
  several of them); prefer letter keys for new defaults.

### 3. Media Playback & Caching
- Player opens immediately; media/chapter loads continue
  asynchronously through the fallback chain (see `player.rs`).
- Chapters come from Podcasting 2.0 JSON, embedded ID3/MP4 metadata,
  or provider APIs, cached per the Data Model split above.
- Casting proxies media for external devices and remuxes when
  required (see **Casting**).

### 4. Full Text & Discovery
- Full text runs the cascade in `extract.rs`; the reader shows the
  feed body immediately and upgrades to extracted text when it is
  longer. Image alt text surfaces as `[Image: alt]` (global
  `show_image_alt` with per-feed `feeds.show_images` override).
- The Rich View (wx WebView) is opt-in (`full_text_rich_view`, default
  off) and gated on the backend actually being available; navigation
  from it follows the Python link-diversion rules (issue #102).
- The Accessible Browser front-end (Python `gui/accessibility.py`,
  the VoiceOver-first window) landed in v0.1.38
  (`app/src/accessible_browser.rs`, core model in
  `crates/core/src/accessible.rs`; see the Batch 37 resolution in
  PARITY.md). Reading behaviour added to the main window is not
  "done" for macOS until it exists there too.

### 5. Translation
- Off by default. Operates on rendered full-text output in chunks,
  caps total submitted text, and falls back to the original text on
  failure (with the failure shown/logged). Model defaults are
  provider-specific and tested; update code and tests together.

### 6. Updates
- Self-update checks the latest GitHub release, verifies SHA-256
  against `SHA256SUMS` before applying, and stages the swap so a
  failed rename never destroys the working install (see
  `maintenance.rs`). Install-local runtime state (database, config,
  caches, cookie files) is never moved or deleted by an update.

## Tests
- `crates/core/tests/` holds the batch/slice regression suites plus
  unit tests beside the code. Default test runs are deterministic and
  offline; live-network, yt-dlp, VLC or GUI diagnostics are manual
  tools, not tests.
- Tests open COPIES of fixtures (`tests/fixtures/sample.db`), never
  the tracked fixture itself — a test run must leave `git status`
  clean.
- Temp paths in tests must be unique per process even on CI runners
  with coarse clock granularity: process id + nanoseconds + an atomic
  counter. Nanos-only names collided on macOS CI and made parallel
  tests share one database file (the v0.1.34 fix).

## Build Quality
- `cargo fmt` after the last edit, clippy zero warnings, full test
  suite green — all three, every time, before a release tag. A local
  failure waved off as "pre-existing/environment" does not clear a
  release; verify on CI's conclusion instead.

## Blunt Things To Remember
- BlindRSS is accessibility-first. A fix that works visually but
  regresses NVDA, JAWS, VoiceOver or Orca is not done. Screen-reader
  behaviour is the owner's manual acceptance gate; agent-side checks
  never substitute for it.
- Native wx controls with real accessible names first. A custom
  `wxAccessible` is used only where measured necessary (the seek
  slider), and it must never call back into UI-owned objects from the
  accessibility thread. AccessKit was evaluated and rejected with
  evidence: it owns a custom toolkit's whole accessibility tree and
  cannot retrofit a wxWidgets window (see PARITY.md, v0.1.29).
- Do not block startup or the UI thread with network, dependency
  checks, yt-dlp, translation, extraction or media probing.
- Do not replace fuller feed/article text with a shorter scrape,
  translation failure, or bot-interstitial page.
- Do not invent new release steps. The flow in **Build & Release** is
  the release process.
- Never claim a test suite, harness or CI result you did not actually
  read. Verify release state only by direct `gh` API reads.
- Do not log secrets: provider API keys, cookies, Authorization
  headers, the YouTube refresh token, and translation request text
  are user data.

## Operational Mandates
1. User-Agent identity comes from core `user_agents.rs` /
   `http.rs` only — never hard-code a UA string at a call site.
2. Date handling: the ported normalization (title > URL > feed date >
   content priority) is the only date path; feed metadata loses when
   inconsistent.
3. Performance: batch DB reads; no per-item DB loops in UI handlers;
   precompute row annotations off the render path.
4. Threading: one DB connection per refresh worker; never share a
   connection or an HTTP session across threads.
5. Naming: app name is **BlindRSS**; the repo is BlindRSS-Rust only
   because the Python repo holds the plain name during the migration.
6. Timeouts: all provider/feed HTTP requests set finite timeouts.
7. Releases: follow **Build & Release** exactly — version from
   `app/Cargo.toml`, next above latest release AND tag, CI green
   first (conclusion read directly), then tag, then verify published
   + Latest by API read.
8. Tests: add/extend tests for behaviour changes and regressions.
9. Secrets: never print, log, commit, or include API keys/cookies/
   Authorization headers in tests or diagnostics.
10. Parity: a feature is ported when PARITY.md says so with its
    evidence — not when the code merely exists.

## Agent skills

### Issue tracker

Issues are tracked in this repo's GitHub Issues, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: the domain authority for this port is `docs/PARITY.md`
(the parity checklist against the Python spec) plus this guide. See `docs/agents/domain.md`.
