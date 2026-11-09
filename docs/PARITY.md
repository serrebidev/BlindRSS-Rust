# BlindRSS Parity Audit — every provider, every feature

Current source comparison: [Audit 8, 2026-11-03](AUDIT-8-2026-11-03.md),
Python v2.5.8 (`2a7eeb3`) against Rust v0.1.53 (`1df3af2`).
Outstanding work: **160 source-confirmed findings** (54 P1, 93 P2, 13 P3),
with a [31-task implementation plan](superpowers/plans/2026-11-03-parity.md).
These findings govern current open/closed status when historical rows or
resolution paragraphs below disagree. No product fixes or runtime validation
were performed by Audit 8. The [2026-11-01 audit](PARITY-2026-11-01.md) and
earlier rows remain historical implementation records, not current parity proof.

Generated 2026-10-30 by mechanically auditing the Python source
(serrebidev/BlindRSS v2.5.4, commit 9a69e59): AST scans of
`providers/`, `core/`, `gui/`, plus the help-topic and shortcut
registries. This file is the port checklist. Status key:
**done** = shipped in this repo with tests, **partial** = core exists,
app/UI or depth missing, **missing** = not started.

## 1. Providers

Provider interface: `providers/base.py::RSSProvider` — 54 methods.
Implementations found in Python (7):

| Provider | Python size | Interface coverage in Python | Rust status |
|---|---|---|---|
| Local (`providers/local.py`) | 6,063 lines, 55 public methods | Full interface + smart folders, filter rules, feed errors, retention, podcast archive | **partial** — refresh/store, feed CRUD, OPML, categories, search, favorites, delete/restore, smart folders + filter rules (evaluate on store), retention prune done; category hierarchy + tree grouping, feed-errors view + recording, refresh cancel, scheduled + startup refresh done (v0.1.13) |
| Miniflux (`providers/miniflux.py`) | 2,945 lines, 33 public | + test_connection, podcast archive, article delete, fetch_full_content, browser fallbacks for some endpoints | **partial** — sync + remote state + enclosures done; v0.1.19 adds test_connection (Settings button), OPML export/import via API, fetch-content (v0.1.18 arbitration), per-feed refresh, create/delete feed, bookmark toggle. v0.1.52 had already closed remote category CRUD (create/rename/delete_category_remote via /v1/categories, mirrored on local category changes through mirror_hosted_category — the old Remaining note was stale). v0.1.82 ports the Miniflux companion browser fallback (same-origin POST with X-Auth-Token, 404 disables probing, recovery queued on refresh/add/single-feed; miniflux_fallback_tests) |
| Inoreader (`providers/inoreader.py`) | 1,110 lines, 24 public | GReader protocol + OAuth2 (`core/inoreader_oauth.py`), rate-limit handling, metadata/article caches | **partial** — OAuth-token sync (slice 5); v0.1.18/19: paste-URL OAuth + local HTTPS-listener OAuth (rcgen self-signed on 127.0.0.1:18423), 429 Retry-After backoff on the GReader API surface, metadata/article caches with Python's TTL keys + clamps (manual refresh invalidates), enclosures. page-size key wired v0.1.22 (article_request_page_size, default 100, clamped 20..=1000, Inoreader stream requests) |
| BazQux (`providers/bazqux.py`) | 765 lines, 24 public | GReader protocol | **partial** — ClientLogin sync (slice 5); v0.1.17/19: remote subscribe/unsubscribe, per-feed refresh, category set, batch mark-read, starred favorites, enclosures via the shared GReader client |
| The Old Reader (`providers/theoldreader.py`) | 744 lines, 23 public | GReader protocol; defines the family's stream-id helpers | **partial** — same as BazQux |
| YouTube account (`providers/youtube_account.py`) | 231 lines, 22 public | Wraps another provider: subscriptions/recommendations from a signed-in YouTube account (device-code sign-in) | **partial** — v0.1.19: device-code sign-in dialog (core/youtube_account.rs; the TV client's public client id is built in, its secret is read from youtube_account_client_secret config rather than embedded), subscription auto-add sync under Sync Providers (category + known-channels tracking). Recommendations virtual feed landed v0.1.22 (TV browse parse + youtube_recommendations snapshot with retained state, tree node + read/favorite/delete routing, sync under Sync Providers) |
| Hosted podcast archive (`providers/hosted_podcast_archive.py`) | 313 lines, mixin (4 public) | Archive state + refresh for hosted providers | **partial** — hosted articles now carry their enclosures as media (v0.1.19, the mixin's data half: GReader + Miniflux enclosure mapping in sync); the archive-state lineage tracking for hosted feeds is still missing |

Base-interface method groups (54) and where they stand in Rust core:

- Lifecycle: get_name, refresh, cancel_refresh, refresh_feed,
  refresh_feeds_by_ids, scheduled_refresh_tick,
  should_force_startup_refresh — **done** locally (v0.1.13:
  Refresh Category covers by-ids, the refresh worker honours a
  cancel flag between feeds, a 60s tick fires refreshes on
  refresh_interval, refresh_on_startup fires shortly after launch)
- Feeds: get_feeds, add_feed, remove_feed, supports/update feed edit
  and URL update, supports/reset feed title — **done**
  (add/remove, Edit Feed title+URL prompts, Feed Properties dialog
  with per-feed settings, v0.1.13-v0.1.15)
- Articles: get_articles, get_articles_page, get_article_by_id,
  get_feed_errors — **partial** (list + by-id + the Feed Errors
  view/recording done v0.1.13; provider-paged listing is the
  hosted cache's shape instead, no separate pager)
- Read state: mark_read, mark_unread, mark_read_batch, mark_all_read —
  **done** (local + batch; remote propagation wired: GReader
  set_read_remote and Miniflux entry state ride the toggle actions,
  and the active provider writes through its cache + server)
- Favorites: supports_favorites, toggle_favorite, set_favorite — **done** locally
- OPML: import_opml, export_opml — **done** (local)
- Categories: get_categories, add/rename/delete, move_category,
  supports_subcategories, get_category_hierarchy,
  uncategorized_is_real_category — **done** locally (v0.1.13:
  categories carry parent_id + full-path titles,
  ensure_category_path/delete_category_path with Python's
  reparenting, rename moves the subtree, the tree groups feeds
  under nested category nodes)
- Full text: fetch_full_content — **done** (cascade wired into
  the app via Read Full Text; the fetch fallback chain landed
  v0.1.18, arbitration v0.1.18)
- Media/chapters: get_article_chapters, HostedArticleMediaMixin
  (update_article_media) — **done** (chapters stored/listed and
  navigated in the player; update_article_media landed v0.1.11;
  hosted enclosures map into local media v0.1.19)
- Delete/restore: supports_article_delete, delete_article,
  supports_restore_deleted, restore_article — **done** (local)
- Smart folders: supports_smart_folders, get/create/update/delete —
  **done** (CRUD + rule evaluation + tree display; the full
  condition editor dialog landed v0.1.15)
- Filter rules (LocalProvider extension): get/create/update/delete,
  reorder, apply to existing — **done** (update/reorder +
  apply-to-existing v0.1.14; the editor dialog v0.1.15)

## 2. Core feature areas (Python `core/`, 46k lines)

| Area | Python module(s), size | Rust status |
|---|---|---|
| Database (schema, counts, chapters, playback state, smart folders, filter rules) | db.py 2,656 | **partial** — feeds/articles/categories/deleted/chapters/playback_state/smart_folders/filter_rules all read+written; podcast archive state tables + article counts unread |
| Config (defaults incl. providers, shortcuts, columns) | config.py 886 | **partial** — reader + writer (dotted set_path, pretty save) + settings dialog done; Python's default surface is 189 keys (Audit 2, §5B) and Rust touches 13 — the settings-depth gap lives there. Wired so far beyond the 13: remember_last_feed/start_maximized (v0.1.9), announcements map (v0.1.12), refresh_interval/refresh_on_startup/category_tree_default_expanded (v0.1.13) |
| Feed discovery + site search | discovery.py 5,530 | **partial** — social transforms (Reddit/Bluesky/Mastodon/Lemmy/PieFed/Kbin/YouTube channel+playlist) + HTML alternate discovery + Add Feed resolution chain done; directory + social + yt-dlp search ported (feedsearch.rs: Feedsearch, Podcast Index, Apple, Fyyd, GPodder, Podverse, Feedly, NewsBlur, Reddit, Lemmy, Mastodon, Bluesky, PieFed providers plus yt-dlp YouTube search; reconciled v0.1.82 — the old "missing" note was stale). FeedSearchDialog depth is tracked with the dialog notes below; dedicated Reddit/Lemmy subscription enumeration remains under Forum sources |
| Article extraction (layered cascade) | article_extractor.py 5,098 | **partial** — cascade + full gate-marker list, paywall/link-list rejection, JSON-LD, Register + Axios dedicated extractors, metered notice, zero-width strip (slices 1, 6); engine-disagreement vetting hardened v0.1.4 (error-page title/body markers + prose shape, with BBC article + BBC 404 fixture regression tests — a blanket disagreement rejection discarded good extractions); precision-first base + lean lead recovery v0.1.11 (Python's favor_precision recipe: balanced mode dragged footer/affiliate boilerplate into Brandon's 9to5Mac full text; precision reproduced Python's output exactly; regression fixtures 9to5mac_whatsapp.html + cbc_syndicated.html); remaining: Python's meta-description lead recovery depth. Landed v0.1.18: fetch fallback chain (stored cookies -> Sky News via Google Translate -> Jina-as-text on gates -> Smry -> Wayback raw snapshots; curl_cffi TLS impersonation and the warm-browser stage are documented non-ports — no Rust equivalent exists for either), byte-accurate decoding (override -> BOM -> header charset -> meta charset -> UTF-8, encoding_rs), BBC/WSJ/TechRadar boilerplate strips, Bloomberg video JSON-LD text, GSMArena battery-table sort (HTML-level; not ported — the table extracts unsorted), thread rebuilds for the API platforms (Lemmy api/v3, GitHub REST, Reddit .json, Discourse .json — core/threads.rs; the HTML-selector forum configs (Invision/vBulletin/XenForo/Google Groups) are not ported: no CSS-selector engine), captcha solver client (core/captcha.rs; its only Python caller is the browser stage) |
| HTML rendering of articles | article_html.py 1,190 | **missing** (native TextCtrl reader instead; rich view is a Phase-6 manual gate) |
| Utilities (dates, text, URLs) | utils.py 2,893 | **partial** — pulled in as needed |
| Browser cookies (Chromium) + site cookies + clearance | chromium_cookies.py 2,184; site_cookies.py 1,533 | **done (v0.1.21)** — Netscape jar + matching + 8KB budget + fetch wiring; Chromium import: profile discovery for the full Chromium family on all 3 OSes, v10 keys (Windows DPAPI + AES-256-GCM; macOS Keychain PBKDF2 + AES-128-CBC; Linux peanuts default), DB copy/read/decrypt with domain-hash strip + merge into the jar (core/chromium.rs) + Tools > Import Site Cookies dialog. NOT ported: the v20 app-bound scheme (Python resolves it via SYSTEM impersonation + CNG + a UAC-elevated helper; the Rust app ships no elevation machinery) — v20 rows are counted as skipped; the exported-jar auto-import watcher landed v0.1.22 (core/cookies_import.rs: YouTube export auto-import with mtime persistence, Downloads exports merged into the site jar, changed Chromium profiles re-read hands-free; a 45-second watcher timer in the app mirrors CookieImportWatcher) |
| Range cache proxy / stream proxy / play cache | range_cache_proxy.py 2,000; stream_proxy.py 818; play_cache.py | **partial** — the cast LAN proxy (Range passthrough + ffmpeg HLS) and the YouTube play cache landed v0.1.20 (core/proxy.rs, core/play_cache.rs); Python's general range-cache proxy for ordinary playback is not ported (MediaCtrl streams directly) |
| Dependency check (yt-dlp, ffmpeg, VLC…) | dependency_check.py 1,868 | **done (v0.1.20)** — core/dependency.rs: tool detection, managed yt-dlp download/update, startup prompt gated by prompt_missing_dependencies_on_startup, Settings surface; v0.1.82 adds the onefile-aware worker kill (Python #150: end_tool_tree + grace before the bootloader kill, so no orphaned %TEMP%\_MEI* — dependency.rs, source_feeds.rs, feedsearch.rs) |
| Casting adapter | casting.py 1,609 (+ Caster engine, see Phase 5 report) | **partial** — caster-core in core: SSDP discovery, hand-rolled CASTv2 wire + TLS session, receiver launch, media load/control, Cast button; DLNA/Sonos/Roku/Kodi landed v0.1.20 (core/castupnp.rs, cast.rs discovery incl. mDNS); AirPlay and the cast queue UI remain |
| Updater + translation updates | updater.py 1,283; translation_updates.py | **partial** — check + button + verified download/stage/apply landed v0.1.16 (SHA256SUMS asset ships with each release); translation updates landed v0.1.22 (core/translation_updates.rs: ETag-conditional catalog fetch from the repo's main branch into the data-dir override tree the loader prefers, version-transition reset, MIN_SANE_MESSAGES sanity gate, Settings section with frequency + Check Now, background due-checks on the watcher tick) |
| Translation (LLM clients) | translation.py 1,020 | **done (v0.1.20)** — core/translate.rs: six providers, Python's system prompt, chunking + caps, language detection; Translate action + Settings section |
| Browser feed detection | browser_feed.py 963 | **missing** |
| GitHub-as-source feeds | github_source.py 946 | **done** — the Python module is pure page completion (no feed-enumeration half) and it is ported whole: threads/github.rs rebuilds pull/issue/commit/release/discussion pages (target parse, API documents, discussion HTML fallback) with one regression test per page kind (reconciled v0.1.82 — the old "missing" note was stale) |
| Podcast archive | podcast_archive.py 716 | **partial** — rescan (live + Wayback union) done; dialog/lineage depth missing |
| YouTube full text (description/chapters/transcript) | youtube_fulltext.py 755; youtube_pytubefix.py | **done** — description + chapters + VTT transcript via yt-dlp (slice 12) |
| YouTube account + browser session | youtube_account.py; youtube_browser_session.py | **partial** — account device-code sign-in + subscription sync + recommendations ported v0.1.19/22 (see the provider row); the anonymous browser-session bootstrap ported v0.1.82 (core/youtube_session.rs: hidden warm-browser visit serialized with the feed fallback, 30-minute identity reuse, Netscape jar + visitor data + browser UA as the final yt-dlp recovery phase in media/play-cache/download). Python's pytubefix client phase stays a documented non-port |
| YouTube Takeout import | youtube_takeout.py | **done** — ZIP subscriptions/playlists/history import (slice 12) |
| Forum sources (Reddit/Lemmy/Groups.io) | forum_sources.py 551; groups_io.py 448 | **partial** — Google Groups + Groups.io archive-search subscriptions ported (forum_sources.rs); Reddit/Lemmy subscription shaping arrives via the discovery social transforms; dedicated Reddit/Lemmy API enumeration depth remains |
| Rumble / Odysee / NPR / Sky sources | rumble.py 582; odysee.py; npr.py; sky.py | **done** — the source-feed family is enumerated at refresh (sources.rs + source_feeds.rs: Odysee/Rumble/YouTube-search/SoundCloud/Mixcloud/Google-Groups/Groups.io-search, URL normalization persisted back at refresh, YouTube channel-uploads fallback + history backfill, NPR/Sky audio attach; reconciled v0.1.82 — the old "missing" note was stale) |
| SponsorBlock | sponsorblock.py | **done** (v0.1.14: core/sponsorblock.rs — YouTube id extraction, skipSegments fetch + category filter; the player applies segments on its tick with the already-skipped guard and sponsor_skip announcements) |
| Audio silence detection + skip | audio_silence.py 534 | **ported** (Audit 8 Task 21, see the resolution at the end of this file: core/silence.rs streams ffmpeg PCM through the WebRTC VAD — libfvad via the `webrtc-vad` crate, the same detector Python drives — with Python's RMS streaming detector as the fallback; the scan is incremental, paced and cancellable, and the player runs Python's guarded skip policy. The v0.1.14 silencedetect substitute and its shrink-padding are gone) |
| Playback state persistence | playback_state.py | **done** (slice 7: get/upsert/delete against the Python table) |
| Equalizer (libVLC 10-band presets) | equalizer.py | **missing** |
| User agents / fetch identity | user_agents.py 540; http_headers.py | **partial** — all fetches ride the shared http agent with the resolved UA (8 presets + custom + auto=platform preset) and yt-dlp gets the same UA, v0.1.16; installed-browser version detection + per-feed identity + http_headers.py per-channel headers missing |
| Backup/restore | backup.py | **done** — ZIP backup (VACUUM INTO + config) + restore + app buttons; restore is staged to restore_pending/ and applied at startup before the DB opens, replaced files kept as .pre-restore (Python semantics; the earlier live swap was a correctness bug, fixed v0.1.13) |
| Retention/purge | retention.py | **done** (v0.1.16: named policies ported in core/retention.rs incl. the hidden 2_days/3_months ids; article prune honours article_retention, downloads pruned by download_retention at startup) |
| Smart folders / filters logic | smart_folders.py; filters.py | **partial** — rule tree evaluator + action pipeline ported and tested (tag field awaits articles.tags) |
| Metadata enrichment | metadata_enrich.py | **done** (v0.1.18: core/enrich.rs — meta tags + JSON-LD author/keywords/section; Db::apply_enrichment fills a missing author and merges tags into articles.tags with Python's dedupe/caps; the full-text drain enriches from the fetched page; smart-folder Tag rules read the stored tags) |
| Media URL resolution | media_url.py; download_formats.py | **partial** — yt-dlp info/stream resolution + download presets done; playback resolution chain ported v0.1.10 (enclosures keep their MIME type, audio/video enclosure preferred over images, yt-dlp gated to Python's supported-site allowlist, page media detection scans stored content then the fetched page, detected media persisted via update_article_media, yt-dlp generic only as last resort — fixes Brandon's Double Tap "Unsupported URL" failure); media_url.py normalize rules landed v0.1.22 (core/media_url.rs, wired into Open Media URL + title_from_url naming); download_formats.py presets **done** v0.1.16 (core/download.rs: 8 named formats, Python's exact selectors + mp3 bitrates) |
| Article language detect | article_lang.py | **done (v0.1.22)** — core/article_lang.rs: BCP-47 normalize_lang, lang_from_page_html, resolve_content_language priority chain; wired into the Translate action (target normalization + already-in-language check). Content lang marking of the reader itself awaits the rich reader (Phase-6 gate) |
| Announcements | announcements.py | **partial** — event table + mode resolution ported (v0.1.12, core/announce.rs, Python's normalize_modes semantics); NVDA controller-client speech + Braille backend on Windows (vendored nvdaControllerClient.dll ships in the Windows zip), macOS `say` speech; Settings > Announcements page with per-event mode choices + Test button; wired events: start_update, refresh-complete (general), favorite_toggle, status_toggle, filter_change, playback_speed, media_navigation, sponsor_skip. JAWS COM backend landed v0.1.21 (FreedomSci.JawsApi SayString via hand-rolled IDispatch, gated on a live jfw/jaws/fusion process, tried after NVDA exactly as Python; Braille stays NVDA-only as in Python). v0.1.82 ports the accessible-output2 speech layer without the library: Windows speaks through SAPI (sapi::speak, tried after NVDA then JAWS, exactly Python's order) and Linux through spd-say --wait/--cancel with accessible_output2's roles; macOS keeps `say`. Live SAPI dry-run passed (sapi_voice_resolves_without_speaking) |
| Single instance, runtime env, self-test | single_instance.py; runtime_env.py; runtime_self_test.py | **missing** |
| Platform integration (Windows/macOS) | windows_integration.py; macos_integration.py | **missing** |
| Screen-reader announcements | screen_reader_announce.py | **partial** — status-bar text; no dedicated announce channel yet |
| i18n (17 locales) + po compile | i18n.py; po_compile.py | **partial** — PO parser + catalog load + language resolution done; v0.1.21 wired the catalog into the app (crate::tr) and wrapped the Settings/dialog/player control labels (63 literals) — entries translate where the msgid matches Python's; menu titles, field labels, status and announcement strings are not yet wrapped, and the .mo compile step is still missing (the PO parser reads .po directly, so .mo is a packaging concern only) |
| Help system (80 top-level topics; guide in 17 locales) | help_topics.py; docs/help/*.md | **partial** — guide loader with locale fallback, topic split, plain-text viewer dialog done; context-sensitive help mapping (help_topics.py: topic per dialog/command) not wired; release zips do not bundle the guide/locale assets yet |
| Shortcuts registry (70 actions, remappable) | shortcuts.py 439; gui/shortcut_keys.py; gui/hotkeys.py | **partial** — full registry extracted from Python, normalisation/conflicts, overrides dialog; live key dispatch for the main-window commands since v0.1.13 (see §7.5); remaining commands dispatch as their features land |

## 3. GUI surface (Python `gui/`)

- MainFrame: 127 user-action handlers. Rust app today covers (as
  buttons; there is no menu bar yet — Audit 2 §5C): refresh all,
  add/remove/edit feed, rename category, set feed category,
  import/export OPML, sync providers, toggle read/favorite,
  delete + deleted restore, mark all read, search, read full text,
  play media + player window, download, Takeout import, archive
  rescan, cast, backup/restore, prune, settings dialog, shortcuts
  dialog, help viewer, check updates, smart folder new/delete,
  category tree grouping, refresh category, stop refresh, feed
  errors dialog, new/delete category, import OPML here, export
  category OPML (v0.1.13).
  **Missing:** menu bar + context menus, feed properties, sort
  controls, article/media filters, persistent searches,
  smart-folder edit + filter-rules UI, copy link/text variants,
  open in browser, download-as, podcast archive dialog, find in
  article, article history, play queue UI, rich-view toggle,
  accessible browser, per-feed notifications, equalizer UI,
  YouTube account UI, about dialog, global hotkeys.
- PlayerFrame: 20 handlers (play/pause/stop, seek slider, rewind/
  forward, volume, speed, chapters menu + prev/next/jump, cast) +
  CastDialog — **partial** (slice 7 player window: play/pause/stop,
  +-30s seek, volume, chapter list with seek, resume; speed,
  slider, cast missing).
- Dialogs: SettingsDialog (24 handlers: provider accounts incl.
  Inoreader OAuth, translation provider + OpenRouter models,
  notifications, paths for downloads/cookies/backups/play cache,
  media tool, article-open method, user-agent mode), FeedSearchDialog
  (discovery), YtdlpGlobalSearchDialog (18 handlers), QueueDialog,
  PodcastArchiveDialog, FeedErrorsDialog, EqualizerDialog,
  KeyboardShortcutsDialog, YouTubeAccountDialog,
  ExcludeNotificationFeedsDialog, ImportSiteCookiesDialog,
  PersistentSearchDialog, FilterRulesDialog + editor,
  AddFeedDialog, FeedPropertiesDialog, OpenMediaUrlDialog,
  ChooseCategoryDialog — all **missing** except basic prompts.
- Tray icon — **done** (TaskBarIcon, placeholder bitmap; menu/actions missing). Global hotkeys — **missing** (hotkeys are wxDragon's
  one binding gap; needs a platform layer).

## 4. Port queue (derived from this audit, in order)

1. Extraction wiring + remaining gates/site fixes/thread rebuilds
   (slice 6, in progress).
2. Feed edit/properties, rename/move categories, feed errors,
   mark-unread + batch, remote read-state propagation (provider
   depth pass).
3. Media: yt-dlp wrapper, MediaCtrl player window, playback_state,
   chapters, play queue (slice 7).
4. Discovery (feed detection + directory search) and subscribe
   shortcuts (YouTube/Bluesky/Mastodon/GitHub URL transforms).
5. Smart folders + filter rules.
6. Downloads, podcast archive, YouTube full text + Takeout.
7. Cookies (browser import + site cookies) and fetch identity.
8. Casting via caster-core (slice 8).
9. Settings dialog (provider accounts incl. OAuth), backup/restore,
   updater, retention. — DONE (slice 8): settings dialog with
   Inoreader paste-redirect OAuth; remaining settings depth in the
   dialogs row.
10. i18n (17 locales), help viewer (92 topics), shortcuts registry,
    tray.
11. CI + release (slice 9).
12. Manual NVDA/JAWS acceptance — Brandon's hands, release-blocking.

## 5. Audit 2 — feature-surface sweep (2026-10-30)

Second mechanical pass (audit/feature_audit.py in the migration
workspace; report: reports/feature-audit-2026-10-30.md). Extracted:
189 Python config default keys, 65 MainFrame menu labels, 31
dialog/frame classes, every core/providers/root module surface, 80
help topics, the 2 bundled sounds; Rust side: core pub fns per
module, 42 app UI labels, 13 config paths. Findings beyond §1–§4:

### 5A. Provider architecture gap (biggest single finding)
Python runs against ONE active provider (`active_provider` config +
`core/factory.py::get_provider`): Local, Miniflux, Inoreader,
BazQux or The Old Reader can each be the live backend the whole UI
reads/writes through the 54-method interface.

**Landed v0.1.17** (core/provider.rs): the factory
(`get_provider`, same fallback-to-Local semantics), the
app-facing Provider trait (feeds / articles / mark_read /
set_favorite / add_feed / remove_feed / refresh), LocalProvider
over the database, and HostedProvider for the four hosted
backends. Hosted providers are cache-backed: sync pulls remote
content into the local database, reads serve from the cache,
and writes go to the cache AND the server — read state
(edit-tag / entry status), favorites (GReader starred state,
Miniflux bookmark toggle), subscriptions (GReader
subscription/edit, Miniflux POST/DELETE /v1/feeds). The app
routes through the active provider for Refresh (hosted =
sync that provider only), Add Feed (hosted = remote
subscribe + pull), Remove Feed (hosted = remote unsubscribe +
cache removal), and favorite toggles; read-state pushes keep
the earlier per-service propagation. Settings gained the
Active Provider choice. Remaining depth (batch 8): the rest
of the 54-method surface as live remote calls (remote
categories, OPML via API, server-side fetch-content), the
youtube_account wrapper Python's factory always applies, and
per-provider article caches with TTLs.

### 5B. Settings surface: 189 default keys + dynamic keys
Rust's settings dialog covers provider credentials + download
folder (13 paths). Missing families, by config key group:
- Reading: article_columns layout, article_sort_by/ascending,
  article_structure_{headings,links,lists,quotes,tables},
  show_image_alt (+ per-feed tri-state in menus), article_open_method,
  article_open_command, cache_full_text, full_text_rich_view,
  max_cached_views, remember_last_feed + last_selected_feed
  (dynamic keys, verified in mainframe.py).
- Refresh: refresh_interval, refresh_on_startup,
  max_concurrent_refreshes, feed_timeout_seconds, feed_retry_attempts,
  refresh_stop_pause_seconds, automatic_feed_refresh_workload,
  ignore_feed_cache.
- Player: seek_forward_ms/seek_back_ms (Rust hardcodes 30s),
  volume + volume_step, playback_speed default, resume_* family
  (resume_playback/min/complete threshold/save interval/back),
  preferred_soundcard, show_player_on_play, vlc caching knobs,
  custom_ffmpeg/ffprobe/ytdlp paths, prompt_missing_dependencies.
- Downloads: downloads_enabled, download_format (download_formats.py
  presets + mp3 bitrate), max_downloads, download_retention,
  auto_download_podcasts + auto_download_period, downloaded_media.
- Streaming caches: range_cache_* (11 keys; range_cache_proxy.py +
  stream_proxy.py + play_cache.py incl. youtube_play_cache_*).
- Silence/SponsorBlock: skip_silence + silence_* (11 keys,
  audio_silence.py + webrtcvad), sponsorblock_{enabled,categories}.
- Translation: translation_enabled/provider/target + SIX provider
  key+model pairs (grok, openai, groq, gemini, openrouter, qwen) —
  translation.py is 1,020 lines, not a single-client stub.
- Notifications: windows_notifications_* (6 keys), sounds_enabled +
  refresh_complete/refresh_error sounds (sounds/*.wav, not bundled
  in Rust), announcements modes (announcements.py).
- Window/tray/startup: close_to_tray, minimize_to_tray,
  start_in_system_tray, start_maximized, start_on_windows_login,
  category_tree_default_expanded, show_search_field, search_mode,
  delete_behavior, confirm_article_delete, start menu/desktop
  shortcut integration (windows_integration.py, macos_integration.py).
- Accounts/search extras: github_token, groups_io_api_key,
  newsapi_api_key, thenewsapi_api_key, enable_adult_search,
  discovery_{city,country,language,region}, captcha_solver_*
  (captcha_solver.py — Turnstile solving service), data_location,
  debug_mode, global_show_hide_hotkey.
- Updates: auto_check_updates, install_updates_automatically,
  auto_backup_{enabled,dir,last} (scheduled auto-backup — Rust has
  manual backup only).
- Inoreader depth keys: article/metadata cache TTLs, page size.

Batch 5 resolution (v0.1.16): the Settings dialog now carries
pages/sections for all of the above families except the ones
called out below, and the behaviours behind the keys landed:
refresh engine (worker pool = max_concurrent_refreshes,
per-host gate = per_host_max_connections, retries =
feed_retry_attempts, timeout + UA on the shared http agent =
feed_timeout_seconds / user_agent_mode+custom with Python's 8
presets; "auto" resolves to the platform preset — installed-
browser version detection is not ported), search_mode
(title_only honoured by Db::search_articles_mode),
delete_behavior (global + per-feed feeds.delete_behavior
column; "deleted" / "purge" / "category:<path>") +
confirm_article_delete prompt, downloads (downloads_enabled
gate, download_format named presets with Python's exact
selectors + mp3 bitrates, download_retention pruning),
article_retention policies replacing the fixed 30-day prune,
player keys (seek back/forward, volume step, start speed,
SponsorBlock categories, silence threshold/minimum — all read
by the player since batch 3), notifications
(windows_notifications_* with excluded-feeds picker storing
feed IDs + Test button; tray balloon on Windows, notify-send /
osascript elsewhere), sounds (sounds_enabled; Python's
refresh_complete.wav / refresh_error.wav now bundled in the
zips under sounds/ and played on refresh end), tray
(close_to_tray via CLOSE_WINDOW veto + hide, start_in_
system_tray, popup menu Show/Refresh/Exit, double-click
restore; minimize_to_tray is stored but the wxDragon binding
exposes no ICONIZE event to hook), auto-backup (24h due check
at startup, BlindRSS-auto-backup-*.zip, keep 7,
auto_backup_last), updates (auto_check_updates at startup;
install_updates_automatically downloads the platform zip,
verifies it against the release's SHA256SUMS asset, stages it
in update_pending/, and the next start swaps it in via the
rename-aside trick). Still open from this list:
article_columns (batch 9), article_structure_* / rich view /
max_cached_views (native-reader design differences, §7.3),
streaming caches (batch 9), translation (batch 9),
resume_* family + preferred_soundcard + vlc knobs
(engine-specific to libVLC; MediaCtrl resumes via
playback_state only), start_on_windows_login + OS shortcut
integration, github/groups_io/newsapi keys (batch 7/8
features), captcha_solver (keys stored; the solver runs as
the fetch chain's last resort, batch 7), data_location move
(the current folder is shown; relocating is Python's
app_folder/user_data choice — Rust resolves one location the
Python way), global_show_hide_hotkey (batch 9).

### 5C. Menu bar and context menus (structural)
Python: full menu bar (File/Feeds/Edit/View/Player/Tools/Help) +
tree/list context menus. Rust: menu bar SHIPPED in v0.1.2
(File/Feeds/Article/Tools/Help; every item invokes the same Rc
action as its button, so the two can never drift; mnemonics only,
no accelerators yet). Context menus SHIPPED in v0.1.3 (tree:
Refresh Feed — a new per-feed action also added to the Feeds menu —
Refresh All, Edit/Remove Feed, category ops, archive rescan, smart
folders; list: read/full-text/favorite, play/download/cast,
delete/mark-all). Still missing: the Python-only menus
(Edit/View/Player). Menu-only actions still
missing in Rust include: Edit/Remove Category as dedicated
  dialogs (delete + rename exist), Delete Category and Feeds
  (combined), Open Article, Open in Default Browser, multi-select
Mark {count} as Read/Unread, Copy Links / Copy Media Link /
Copy Image Link, Download As..., Detect Audio, play-queue
Add/Remove/Open, View History, View Feed Description,
Notifications for This Feed, playback speed selector,
Show Chapters + Previous/Next Chapter (main window), in-player
Cast to Device..., Open Chapter Link, Cut/Copy/Paste/Select All.

### 5D. Dialogs: 31 Python classes vs Rust
Landed v0.1.15 (app/dialogs.rs unless noted): AboutDialog,
FeedPropertiesDialog (title/URL/category + title reset by
re-fetch), FeedSearchDialog (discovery UI with worker fetch),
FilterRulesDialog + FilterRuleEditorDialog (list, reorder,
apply-to-existing), OpenArticleDialog (open-by-URL, worker
fetch + extract into the ArticleWindow), OpenMediaUrlDialog,
PersistentSearchDialog (saved searches, run into the article
list), PodcastArchiveDialog (media-feed picker feeding the
rescan worker), QueueDialog (config play_queue; the player
advances on media-finished, index mirrored in config),
SmartFolderDialog (full condition editor: name, All/Any,
field/operator/value rows incl. Python's boolean rows),
TakeoutImportSelectionDialog (check list, all checked by
default), ShortcutCaptureDialog (app/keys.rs, CHAR_HOOK
capture in the dispatcher's normalized format), ArticleWindow
(plain-text window; Python's HTML View menu waits on the
WebView decision in §7.3).
No Rust equivalent yet: AccessibleBrowserFrame (§7.3),
AddShortcutsDialog, CategoryPropertiesDialog (rename-by-prompt
exists since batch 1), YtdlpGlobalSearchDialog (18 handlers),
ExcludeNotificationFeedsDialog (batch 5, with notifications).

### 5E. Module-level gaps not itemised in §2
- article_columns.py — configurable article-list columns: **missing**.
- captcha_solver.py — **missing** (see 5B).
- Caster engine protocols: caster_devices.py + caster_extras.py add
  Sonos, Roku, Kodi and generic UPnP/DLNA beside Chromecast —
  Rust caster-core is Chromecast-only: **partial** widened.
- Single-instance activation (single_instance.py): **missing**.
- Runtime self-test (runtime_self_test.py) + startup dependency
  prompt (dependency_check.py): **missing**.
- Screen-reader announce channel (screen_reader_announce.py:
  speech + braille): **partial** — status bar only.
- Context help mapping (help_topics.py): **missing** (see §2 help row).
- text_encoding.py — charset detection for feeds/pages: **missing**
  as an explicit layer (feed-rs/ureq defaults only).
- Translation updates channel (translation_updates.py): **missing**.
- Player engine note: Python plays through a shared libVLC instance
  (vlc_instance.py/vlc_options.py, hence the EQ + caching keys);
  Rust uses wx MediaCtrl. Parity for EQ/silence/SponsorBlock/speed
  must be re-proven against MediaCtrl backends per OS.

### 5F. Port queue additions (after §4 items, rough order)
1. Menu bar (done, v0.1.2) + context menus on the existing actions
   (discoverability is an accessibility feature for screen-reader
   users); menu accelerators with the shortcut dispatch slice.
2. Active-provider architecture decision + factory (5A).
3. Settings depth in SettingsDialog pages mirroring the help
   topics' 10 settings pages, family by family (5B).
4. Per-feed/category refresh + refresh cancel + scheduled refresh.
5. Play queue UI + history + persistent searches + smart-folder
   editor + filter-rules dialogs.
6. Article columns, sort controls, open-by-URL/media dialogs.
7. Caster protocols (DLNA/Sonos/Roku/Kodi), play/range caches.
8. Translation (6 providers), announcements + sounds, single
   instance, startup dependency check, context help.

**Batch 9 resolution (v0.1.20):** item 7 landed — core/cast.rs
discovery now covers Chromecast + DLNA/AVTransport + Sonos +
Roku ECP + Kodi (hand-rolled mDNS), core/castupnp.rs plays to
DLNA/Sonos/Roku/Kodi, core/proxy.rs serves the LAN stream/file/
HLS proxy (HLS transcodes synchronously via ffmpeg), and
core/play_cache.rs caches YouTube audio downloads under
youtube_play_cache_max_mb. Item 8 landed except context help:
article translation for all 6 providers (core/translate.rs,
Tools > Translate + Settings section), single instance
(core/single_instance.rs, activation raises the running copy),
startup dependency check (core/dependency.rs: yt-dlp/ffmpeg
detection, managed yt-dlp self-update into <data_dir>/bin,
prompt gated by prompt_missing_dependencies_on_startup with a
Settings checkbox; ffmpeg offers no in-app install, matching
Python's Linux behaviour). Also landed in this batch: article
change history (article_versions table — seeded on first
fetch, appended on changed re-fetch, SHA-256 title+NUL+content
dedupe, Python providers/local.py semantics; View History in
the Article menu and list context menu), which also fixed the
smart-folder `opened`/`updated` facts (previously hardcoded
false in the refill), the Article Columns editor (Settings >
Article Columns...), and Video Search (core/videosearch.rs +
Tools > Video Search...: the mainstream yt-dlp site set +
Mixcloud API + adult site behind enable_adult_search,
per-site threads, canonical-key dedupe, Play through the
normal resolve path, Subscribe through feed discovery; the
runtime-derived extractor site list, the separate Filter
choice, and title-enrichment pools are not ported). Two macOS
build breaks from batch 5 were fixed in this batch: an
unescaped-quote osascript format string that only parses
under cfg(macos), and the tray double-click binding, which
wx does not provide on macOS (the tray menu Show item is the
restore path there). Release note: v0.1.16's macOS package
failed on those breaks and v0.1.17-v0.1.19 were not released
standalone; batches 6-9 ship cumulatively in v0.1.20.
Batch 9c resolution (v0.1.21): F1 context help landed — the
help_topics registry is generated from Python's module (80 topics,
70 command mappings, dialog/notebook/menu/context maps; integrity
tests assert every mapped id is a real guide section and every
registry topic exists in the shipped guide); the main frame
resolves F1 by the focused control (tree/list/reader/search), and
Settings/Filter Rules/Feed Properties/Video Search/Article
Columns/Play Queue/Persistent Search/Podcast Archive/Find Feeds
dialogs open their mapped sections. Chromium cookie import and
the JAWS backend landed (see the section 2 rows). Global hotkeys
landed for Windows: raw RegisterHotKey with MOD_NOREPEAT on the
frame HWND, WM_HOTKEY received through a wndproc subclass into
the message drain (toggle = hide to tray when foreground, else
show + raise + SetForegroundWindow), Settings field with
Python's revert-on-rejection semantics; core
shortcuts::global_hotkey_spec ports the acceptance rules. Still
open after 9c: the full i18n string sweep (see the section 2
row), the cast queue UI, translation_updates.py, and the
exported-jar cookie auto-import watcher.

## 6. Audit 3 additions (2026-10-30, depth pass)

Full report: reports/feature-audit-3-2026-10-30.md in the migration
workspace; raw extraction audit3-features.json. Newly inventoried:

- Settings pages mapped exactly (10 pages, per-page key sets in the
  report) — the 5F.3 settings build now has a precise target,
  including refresh knobs (feed_timeout_seconds, feed_retry_attempts,
  per_host_max_connections, ignore_feed_cache), search_mode,
  delete_behavior, user_agent_mode/custom, data_location.
- browser_feed.py/browser_bridge.py: warm-browser fetch fallback
  (challenge completion) as a first-class API — **missing**.
- stream_proxy.py: local playback/cast proxy with HLS conversion —
  **missing**.
- play_cache.py: playback cache with cap/prune/clear — **missing**.
- Player depth: PlayerFrame has 167 methods; equalizer.py (band
  model + user presets), audio_silence.py (PCM silence scan),
  sponsorblock.py (per-category segment skip) are separate port
  units — **missing**.
- metadata_enrich.py: async article enrichment (author/tags/section
  from the article page) — the source of article tags — **missing**.
- backup.py semantics: restore is STAGED to restore_pending/ and
  applied at next start before the DB opens; auto-backup rotates.
  Rust restore staged + applied at startup since v0.1.13
  (correctness fix landed); auto-backup **missing**.
- download_formats.py / retention.py: named preset systems (yt-dlp
  format selectors, retention id -> days) replace Rust's hardcoded
  download presets and fixed 30-day prune — **partial, generalize**.
- dependency_check.py: media-tool detect/install + yt-dlp
  self-update + user tool paths — **missing**.
- updater.py: manifest-driven download/verify/stage/apply
  self-update; Rust is notify-only — **partial**.
- announcements.py: per-event announcement modes (speech/braille/
  sound/off per event) — richer than a single toggle — **missing**.
- http_headers.py (per-channel headers for players/casters) and
  article_lang.py (content-language priority order) — **missing**.

</content>

## 7. Deep audit additions (2026-10-30, behaviour pass)

Full report: reports/deep-audit-2026-10-30.md in the migration
workspace; raw extraction deep-audit.json. Behaviour systems the
surface audits missed, in port order:

1. Focus choreography: startup focus in tree (FIXED v0.1.7 —
   re-audited against Python after Brandon's v0.1.5 report: Python
   defers via wx.CallAfter with an IsShown guard AND every tree
   build ends with SelectItem, defaulting to All Feeds. The first
   v0.1.6 cut only did a synchronous set_focus and the tree never
   selected an item, so NVDA had nothing to announce. Port now:
   one-shot 75 ms timer (call_after needs Send; handles are not)
   with is_shown check + set_focus, and rebuild_tree preserves the
   selection by item data, defaulting to the root),
   focus restore after dialogs (DONE v0.1.8 — State tracks the
   working surface from the selection handlers; every modal
   dialog site restores focus to the tree or article list right
   after it closes, via restore_focus/prompt_focus),
   reader-surface focus, large-reader mode — **partly missing**.
2. Startup state restore: remember_last_feed/last_selected_feed
   and start maximized (DONE v0.1.9 — Settings > Startup
   checkboxes write the Python config keys; selection changes
   persist last_selected_feed, startup reopens on it, and
   start_maximized maximizes before show). Tree expansion
   default DONE v0.1.13 with category grouping in the tree;
   start_in_system_tray waits on a tray restore path.
3. AccessibleBrowserFrame + accessible view entries (flattened
   speech-first view model with special views and category
   hierarchy) — **missing entirely**.
4. Announcements: 9 events x none/speech/braille/both modes
   (config["announcements"]), NVDA speech + braille channel —
   **missing** (status bar only).
5. Keyboard model: frame-level char hook; Ctrl+1..6 filters,
   Backspace toggle-read, F5/Shift+F5 refresh start/stop,
   Ctrl+D favorite, find-in-article with spoken results —
   **done** (v0.1.13: CHAR_HOOK binding on the frame resolves
   keys through the registry + user overrides and dispatches
   refresh/stop/refresh-selected/add/feed-errors/favorite/
   play-pause, the six filter commands (announced via
   filter_change), the sort commands, and view.focus_search;
   plain Backspace in the article list toggles read (the search
   box is focus-tracked so its editing is never eaten); Ctrl+F /
   F3 / Ctrl+G run find-in-article with spoken Found/Not found.
   Remaining registry commands dispatch as their features land.)
6. Full-text arbitration: web vs feed vs provider server-side
   fetch (Miniflux fetch-content), completeness wins, cacheable
   flag, per-feed encoding override, enrichment on the same fetch
   — **done** (v0.1.18: Miniflux fetch-content competes with
   the local cascade in the full-text worker, more complete
   text wins; per-feed encoding override lives in
   feeds.feed_settings and wins the decode chain; the same
   fetch's HTML feeds metadata enrichment). HTML in reader
   FIXED v0.1.6 via core html_to_text.
7. Player behaviours: raise on play, announced speed control,
   accessible chapter presentation, announced SponsorBlock/
   silence skips — **done** (v0.1.14: Slower/Faster ladder with
   playback_speed announcements, Previous/Next Chapter buttons +
   chapter-crossing announcements via media_navigation, seek
   slider, seek/volume steps and start speed from config,
   show_player_on_play gates the raise, SponsorBlock + silence
   ranges applied on the player tick with sponsor_skip
   announcements).

Batch 22 resolution (final sweep, v0.1.22): media_url and
article_lang ported with the Python test cases; translation
updates (OTA catalogs) ported and wired (override tree preferred
by the loader, version-transition reset, Settings section,
watcher due-checks); the cookie auto-import watcher ported
(YouTube exports, Downloads exports into the site jar, Chromium
profile re-reads on a 45-second tick); Inoreader
article_request_page_size wired; the YouTube Recommendations
virtual feed landed end-to-end. This sweep also reconciled the
section 1/2 rows against the tree: several rows still said
"missing" for work that shipped in v0.1.15-v0.1.20 (dependency
check, translation, stream proxy + play cache, DLNA casting,
filter-rule and smart-folder editors, the full-text fallback
chain) and now say so. What remains after v0.1.22, deliberately:
the rich HTML reader and WebView (Phase-6 manual NVDA/JAWS
gate); the libVLC equalizer (no MediaCtrl equivalent); AirPlay
and the cast queue UI; Miniflux remote category CRUD and
provider browser fallbacks; github_source and forum/rumble/
odysee/npr/sky source-feed types (thread reconstruction for
forums landed in v0.1.18; source-feed subscriptions did not);
browser_feed detection; the general range-cache proxy for
ordinary playback; the full i18n string sweep (menus, field
labels, status strings unwrapped); Chromium v20 cookies;
the warm-browser and curl_cffi extraction stages; GSMArena
table sort; WebRTC VAD (ffmpeg detector stands in).

Residual audit (v0.1.23, at Brandon's order — "audit the
residuals, a lot of it is in the sourcecode"): every remaining
item was re-checked against the Python source and the
SmartTube precedent. Three were simply unported source and
landed in v0.1.23: (1) the YouTube TV client secret — Python's
core/youtube_account.py carries CLIENT_ID and CLIENT_SECRET as
constants (the SmartTube device flow; Google documents an
installed app's secret as non-confidential), while the Rust
port had made the secret config-only, which silently broke
sign-in for every fresh install; the constant and an
effective_secret fallback (config still wins) are now in
core/youtube_account.rs, so sign-in and Recommendations work
out of the box. (2) The GSMArena battery-table sort — an HTML
pre-transform in Python's extractor; ported as a string-level
normalize_gsmarena_battery_table_html in core/extract.rs with
its URL gate (gsmarena.com/battery-test-v2.php3 only).
(3) Miniflux remote category rename/delete —
providers/miniflux.py methods ported to the client and
mirrored from the tree's category actions when Miniflux is
the active provider, refusals reported as status notes.
The rest are not unported code but different shapes, recorded
exactly: AirPlay in Python is pyatv (an asyncio library) doing
RAOP — no Rust equivalent exists, a RAOP client is a new
protocol project. Chromium v20 in Python is a short-lived
ELEVATED helper process plus a scheduled task for silent
re-elevation and a cached master key — a privileged install
component, not app code. WebRTC VAD is Python wrapping the
native _webrtcvad extension, optional there too (its RMS
fallback is the ported detector). curl_cffi and the
warm-browser stages (including browser_feed page fetching)
are Python libraries with no Rust counterpart short of
bundling Python. The WebView reader is blocked on the
WebView2 loader build failure that removed the feature in
v0.1.1 plus the Phase-6 manual gate. The equalizer drives
libVLC because Python's player is libVLC-based; the Rust
player is MediaCtrl, which exposes no EQ. Casting the play
queue needs cast-session end-of-media events driving the
next load. The source-feed family (github_source, forum,
rumble, odysee, npr, sky) is portable API/transform work
needing per-type refresh-pipeline integration.

Batch 24 resolution (v0.1.24, Brandon's finds): AirPlay and
the equalizer, both previously recorded as blocked, landed
after Brandon supplied the references the audit should have
found itself (his standing research protocol — web search,
GitHub most-recent 20, most-stars 20 — is now the required
method before any residual is declared blocked here).
AirPlay: shairplay-rust is a RECEIVER (server) library and
LGPL, so it cannot serve; the sender design is RAOP v2 as
implemented by LinusU/rust-raop-player (itself a port of
philippe44's RAOP-Player) and by pyatv, which Python uses.
core/raop.rs implements that protocol directly (no code
copied; the repo carries no license): mDNS _raop discovery
in cast::discover_airplay, the RTSP handshake with the SDP
AES key RSA-OAEP-encrypted to the public AirPort key, ALAC
(alac-encoder crate) over RTP with real-time pacing, the
timing responder, per-second sync packets, retransmit from
a 512-packet backlog, SET_PARAMETER volume, FLUSH/TEARDOWN,
and a Stop AirPlay Cast menu action (RAOP streams from this
app, unlike the URL-handoff protocols). The app decodes the
article media through ffmpeg to PCM, so AirPlay casts audio
(a video article casts its soundtrack). AirPlay 2 pairing
devices stay out of scope, exactly as in Python.
Equalizer: LunaDesktop/eq is a PipeWire LADSPA plugin,
GPL-3.0 and Linux-only, so the plugin itself is not
adoptable into this MIT cross-platform app; its shape (a
10-band graphic EQ) is what landed, implemented
independently in core/equalizer.rs: Python's exact config
model ("equalizer" + "equalizer_user_presets", [-20, +20]
clamping, the libVLC preset table read the way Python
reads it, with the preamp's +12 dB libVLC convention
converted for the DSP), RBJ peaking biquads applied to the
PCM in the AirPlay path, and an ffmpeg filter chain
(core/proxy.rs /eq endpoint) that the built-in player
routes through when the EQ is enabled — a live transcode,
so seeking inside EQ playback is limited; with the EQ off
playback is untouched. Settings gains an Equalizer section
opening the full dialog (presets, user presets, per-band
fields). Remaining from this pair: nothing structural;
live proof needs Brandon's receiver and ears.

Batch 25 resolution (v0.1.25, Audit 4 — Brandon ordered a fresh
line-by-line audit of the Python tree for missing features big
or small, each researched before deciding): twelve items
ported. (1) The extractor's whole postprocess stage, which the
earlier audits had missed entirely: core/postprocess.rs ports
Python _postprocess_extracted_text with all 17 host strips
(zdnet, thetyee, 9to5mac/google/toys, postmillennial,
globalnews, aljazeera, axios, bbc with the published-date
byline completion, wsj, techradar, canada.com, castanet,
slashdot, bloomberg, sky related sections, ning activity) and
the generic strips (leading share/ad chrome, leaked script
source with the GitHub-page exemption, recirculation labels),
plus the proxy-only strips on the Jina path; the regex crate
was added for a faithful port, and Python's own tests were
ported (batch25_slice). The cascade's old three-strip
apply_site_cleaners now delegates to it. (2) The missing
site-specific extractors from _extract_site_specific_text:
socast (split-body rebuild with junk-widget removal), Reuters
paragraph nodes, GSMArena review body, Top Tech Tidbits,
publisher body reconstruction for Ars Technica / ZDNet, and
DOM-boilerplate removal for The Post Millennial, Rebel News,
and Simon Willison — all on a balanced-element scanner, since
the existing blocks helper truncates nested same-tag elements.
(3) Image alt text: html_to_text_opts keep_alt emits Python's
[Image: alt] markers (alt, title fallback); resolution is the
per-feed feeds.show_images column (NULL/1/0) over the global
show_image_alt setting, applied at display time like Python
(reader pane, full-text feed conversion, version viewer,
Miniflux arbitration at the global level). Python's markdown
path drops images unconditionally, so markdown_to_text has no
alt variant. (4) Custom article-open command (issue #31):
core/opencmd ports build_open_command (%1 substitution, shlex
splitting, Windows non-POSIX mode); Enter on an article uses
it, with Python's warn-and-fall-back dialog; Settings gains
the method choice, command field, and Test button. (5) Delete
Category and Its Feeds: feeds_in_category_tree +
delete_category_tree in the Db, remote providers remove feeds
through the provider trait, Miniflux category deletes mirror
deepest-first. (6) View Description on the article context
menu. (7) Player volume persists to the `volume` config key
(int percent), read at open and on queue advance. (8) Custom
tool paths: dependency.rs ports Python's user-tool-path
registry (file or directory values, highest priority) for
ffmpeg/ffprobe/yt-dlp, plus Settings fields and an ffprobe
entry in check_tools. (9) ignore_feed_ssl_errors with
Python's actual semantics (issue #42): NOT a global toggle —
feed fetch only, default on, one unverified retry after a
certificate-validation failure (http::insecure_agent +
looks_like_ssl_cert_error with Python's markers plus rustls
phrasings). (10) Category picker dialog (pick-or-create,
Uncategorized first) behind Choose Category... in Feed
Properties and in the local Add Feed flow (the pick travels
through RefreshMsg::Added); the wxDragon ListBox has no
double-click event, so Python's double-click-to-confirm is
the one piece not carried over. (11) Per-feed notification
exclusion toggle on the tree context menu (the gate already
existed in refresh_side_effects; the Settings exclusion
editor already existed too — the menu toggle was the missing
surface). (12) Start at login: config key
start_on_windows_login, applied on Settings save when changed
(HKCU Run key via raw advapi32 on Windows, LaunchAgent plist
+ launchctl on macOS, disabled elsewhere; failure reverts the
stored key, like Python).

Batch 26 resolution (v0.1.26): the WebView rich reader and
categories parity. Rich reader: core/article_html.rs ports
Python core/article_html.py — main-content pick, social-embed
conversion, chrome/boilerplate/byline drops, the
trafilatura-prune passes, media src normalization (YouTube
and Vimeo embeds rewritten to watch pages, as Python does),
paywall rebuild, and the feed-arbitration render
(render_rich_article + rich_article_from_feed) — with ammonia
as the sanitizer (Audit 4 item 13; Python's nh3-equivalent
allowlist configuration, base-URL rewriting for relative
URLs). One deliberate divergence: ammonia drops data: URLs
where Python passes them through. The app shows the rich
document in a wxDragon WebView (feature webview) on all three
OSes when the new full_text_rich_view setting is on (default
off; the native text reader stays the default and the
fallback when no backend exists, as in Python). Route note:
the original plan was a Windows-only blckngm/webview2 path
with a shipped loader DLL; wxdragon 0.9.24's WebView covers
every platform and wxdragon-sys links WebView2LoaderStatic
on Windows, so no DLL ships and one code path serves all
OSes. Link diversion ports gui/rich_view_links.py (issue
#102): embed-shaped iframes (Twitter, Bluesky, Facebook,
TikTok, Instagram, Threads) navigate inline; other external
links are vetoed and opened through the article-open path.
wxDragon exposes no IsTargetMainFrame / GetNavigationAction,
so the set_page loading window, named-target check, and
embed-shape check substitute for Python's discriminators.
Not ported (residuals): multi-page following (Python follows
rel=next up to 6 pages; the Rust text cascade is single-page
too, so both paths share the gap), the YouTube
structured-text special case, the ID3 show-notes path, the
Google News Jina-proxy path, the metered-preview notice,
Sky/GSMArena HTML expansions, and rich mode in the article
window dialog. Categories: Db::articles_for_category ports
providers/local.py::_category_membership_clause — a category
view now contains subtree-feed articles that were not moved
away, plus articles moved (category_override) or labeled
(article_labels) into the subtree; the filter rules already
wrote those columns on refresh, so moved/labeled articles
were previously invisible in the very views they belong to.
Uncategorized is a real tree node, as in Python's local
provider: uncategorized feeds (NULL, blank, or the sentinel)
group under it with a recursive unread count, selecting it
lists only their articles, Refresh Category covers it, New
Category created "under" it lands at the root, and it is
protected from rename and both delete actions. Expansion
memory ports Python issue #33: the tree records explicit
expand/collapse per category and re-applies it across
rebuilds, with category_tree_default_expanded as the default
for untouched nodes (previously every rebuild reset all
categories to the default). Tests: batch26_slice (sanitizer
and cleaner) and batch27_slice (membership clause).

Batch 27 resolution (v0.1.27, Brandon's order: missing tools
must resolve automatically, not via a prompt): the startup
dependency check no longer ends at a "Missing recommended
tools" dialog. dependency::install_ffmpeg resolves the
ffmpeg/ffprobe pair on its own, following Python's
install_media_tools routes: Windows winget (Gyan.FFmpeg,
user scope then default scope, Python's exact flags) with
the gyan.dev essentials zip as fallback, extracted under
<data_dir>/ffmpeg/extract in Python's %LOCALAPPDATA%\BlindRSS
layout; macOS Homebrew (brew located via PATH or the two
standard prefixes, as Python's _brew_path); Linux takes a
johnvansickle static build — a deliberate extension, since
Python has no automated Linux path. Installed binaries are
validated by running them before success is reported, as
Python validates. Detection learned the managed install:
find_ffmpeg/find_ffprobe probe it through BLINDRSS_FFMPEG /
BLINDRSS_FFPROBE pointers seeded at startup from
managed_ffmpeg_bin_dir (mirroring the managed yt-dlp
pattern), and a successful in-session install prepends the
bin dir to the process PATH so yt-dlp finds the matching
ffprobe (Python's stated reason for the same move). The
DepsMissing flow now installs in the background and reports
through the status bar; a dialog remains nowhere in the
flow — a failed install reports its reason in the status
bar, and the Settings custom-path fields stay as the manual
override. Tests: batch28_slice (managed layout detection,
zip extraction).

Batch 28 resolution (v0.1.28, Brandon's hands-on report:
podcasts do not play, the player is not NVDA-accessible,
categories must collapse by default, and category navigation
lags ~1s per step): (1) Podcasts — the audit found an
engine mismatch, not a resolution bug: enclosures resolve
correctly, but Python plays through libVLC (gui/player.py)
while the Rust player uses wx MediaCtrl, whose Windows
DirectShow backend routinely refuses podcast CDN URLs
(redirect chains, modern TLS). The player now falls back in
order: direct load, the local proxy /stream route (the
app's own HTTP stack fetches the bytes), then the new
proxy /play route (a live ffmpeg transcode). serve_stream
was rewritten to stream progressively — it previously
buffered the entire file before responding, which alone
would have stalled episode starts. A libVLC engine port
(vlc-rs is maintained) remains the playback-parity endgame
and is recorded as an open residual below. (2) Player
accessibility: the seek slider, chapter list and position
readout now carry accessible names (NVDA announced a bare
"slider"/"list" before), visible Seek/Chapters captions
were added, and keyboard focus lands on Play/Pause at open
instead of the unreadable native media pane. (3) Categories
collapse by default: category_tree_default_expanded now
defaults to false in both the tree build and the existing
Settings checkbox ("Expand categories by default") — a
stored true from earlier Settings saves still wins, as a
saved preference should. (4) Category lag: refill_articles
now wraps the fill in Freeze/Thaw (Python's
reader_performance.py pattern), rebuilds list columns only
when the article_columns config actually changed (every
refill previously ran clear_all and recreated them), and
tree selection changes coalesce through the 150 ms drain
timer so arrowing past a category no longer fills it — only
the landed-on node fills. Tests: batch29_slice (proxy
routes + live /stream pass-through).

Batch 29 resolution (v0.1.29, Brandon: "When I press enter
it should play automatically. The media player isn't good
enough — look up Rust NVDA accessibility best practices"):
(1) Enter did nothing in the article list — the Rust list
had NO activation binding at all. Python's
on_article_activate (EVT_LIST_ITEM_ACTIVATED) marks the
article read, then plays it in the player when
_should_play_in_player (downloaded file, yt-dlp-supported
URL, playable enclosure, media detected in stored content)
and opens it in the browser otherwise. The Rust port now
mirrors that: a CHAR_HOOK Enter case for the list (vetoed
so wx does not double-fire) plus on_item_activated for
double-click, both running one act_activate with the same
decision and mark-read. (2) Player accessibility pass,
researched per Brandon's protocol: AccessKit
(AccessKit/accesskit, the Rust accessibility standard,
actively maintained) was evaluated and does not apply — it
owns a custom toolkit's whole accessibility tree and cannot
retrofit a wxWidgets window; the applicable best practice
for wx is native controls + wxAccessible, which is what was
applied. Against the WAI-ARIA media-player pattern (every
control exposes name, role, value and STATE; state changes
are announced; everything is keyboard-operable): the seek
slider now carries a Windows wxAccessible (SeekAccessible)
whose value is time text ("12:34 of 45:00") instead of the
raw 0-1000 number; the Play/Pause button's label now tracks
the real state (its label is its accessible name, so NVDA
hears Play or Pause, never a static "Play/Pause"); the
position readout is a focusable read-only text control;
play/pause, volume changes, queue errors and load failures
announce through the existing NVDA/JAWS speech channel;
and the player window gained keyboard control — Space
toggles play/pause, Left/Right seek, Up/Down change volume
(each announced), Enter activates the selected chapter,
Escape closes — always deferring to the native key handling
of the control that has focus (buttons, slider, list).

Batch 30 resolution (v0.1.30, Brandon's NVDA log of
v0.1.29 in use): the log proved Enter now opens the player
(title set, focus on the Pause button) — and proved the
real autoplay bug: within ~1 s the button read "Play"
again, i.e. the backend had dropped out of Playing; his
manual Space then played the same URL without trouble.
Root cause: wx MediaCtrl load success is not readiness —
the immediate play() raced the DirectShow graph and
silently no-oped, and the v0.1.28 fallback chain never
engaged because it only triggers on load FAILURE. The
player now runs an autoplay watchdog in its 1 s tick: a
fresh load that is not Playing gets re-played at ticks 3
and 6, escalates one fallback level (direct -> proxy ->
ffmpeg) at tick 9, and at ~14 s reports "Could not play
this media" in the status bar and by announcement instead
of sitting silent; a deliberate pause or stop cancels the
watchdog immediately. Freeze audit (Brandon: "it
eventually freezes"): the prime suspect was batch 29's own
SeekAccessible — its get_value called MediaCtrl
length()/tell() from the MSAA callback, which runs on
NVDA's in-process RPC thread, racing the UI thread on the
same DirectShow graph (wx is not thread-safe). The
accessible now reads only a lock-free atomic snapshot the
UI-thread tick refreshes, and its name override was
dropped (the slider's window name already covers it). Also
in the freeze pass: the position readout is rewritten only
when its text changes, and the per-second playback-state
save (a Db::open + upsert on the UI thread every tick) is
throttled to 5 s of actual position movement.

Batch 33 resolution (v0.1.33, Brandon's order after the
v0.1.30 verification found it): the feed_settings
migration was not concurrency-safe. init_schema checked
PRAGMA table_info and then ran ALTER TABLE outside any
transaction, so two connections opening a not-yet-migrated
database at once both saw the column missing and both
added it — "duplicate column name", seen in the v0.1.30
tag CI (batch27 test) and a real upgrade-day risk whenever
the app and a worker open the database together after an
update. Reproduced locally with 8 threads opening one
fresh file (3 of 5 runs failed, on three different
migration columns). Fix: init_schema now runs under
BEGIN IMMEDIATE with a 5 s busy_timeout, so openers
serialize and the check happens inside the write lock.
Regression: batch32_slice (concurrent opens of one fresh
DB, repeated opens) — 12 consecutive clean runs post-fix.
Audit addendum: every CREATE TABLE/INDEX in the schema
already uses IF NOT EXISTS, the migration loop is the only
ALTER site in the codebase, and a panic-path sweep of both
crates found no panic!/unreachable! in library or app
code. This release also carries the previously unreleased
Batch 31 (RAOP/FFI/update hardening) and Audit 5
(data-safety: config preservation, staged-restore path,
HLS/DASH enclosure playability) work — v0.1.31 and v0.1.32
were tagged but never published, and are not re-tagged.

Batch 34 resolution (v0.1.34): the v0.1.33 release run
failed on macOS in batch27 — a second race of the same
class. Two layers: (1) ensure_category_path was
SELECT-then-INSERT, so concurrent ensures of one path
died on the UNIQUE categories.title constraint; it now
INSERT OR IGNOREs and re-reads the winning row, honouring
its idempotent contract. (2) Four test files built temp
paths from wall-clock nanoseconds only; on CI runners
with coarse clock granularity two parallel tests landed
in the same directory and shared one database file (the
collision that fed both this failure and, partly, the
original one). Test temp paths now carry process id +
nanos + an atomic counter. batch32_slice gains a
concurrent ensure_category_path regression; batch27 +
batch32 ran 10x clean locally. v0.1.33 itself is not
re-tagged; its content ships inside this release.

Open residual (playback engine): Python's player is libVLC
end to end (7,276-line gui/player.py: VLC caching knobs,
preferred soundcard, resume_* family, HTTP header control).
The Rust player substitutes wx MediaCtrl plus the batch-28
fallback chain; a libVLC engine port (garkimasera/vlc-rs,
maintained as of 2026-10) behind the same player window is
the parity endgame and its own batch — it also decides the
vlc-knob and preferred_soundcard residuals above.

Batch 35 resolution (v0.1.35): the source-feed family is
ported in `crates/core/src/sources.rs`. Scope correction
first: Python's `core/github_source.py` is NOT a refresh
subscription type (nothing in the provider refresh path
references it) — it is the extractor's GitHub page
reconstruction, ported under Batch 35b. NPR and Sky are
likewise media attach inside the generic parse path, not
listing sources. The listing family in refresh, mirroring
the branch order of `providers/local.py::_refresh_single_feed`:
Odysee (yt-dlp info document, listing title from the info,
caps 150 initial / 60 refresh), Rumble (HTML listing scrape
over the extractor's balanced scanner, 3/1 pages, feed URL
normalized and persisted back to the feeds row — Odysee
normalization persists the same way), YouTube search
(yt-dlp flat listing, uuid5 article ids), SoundCloud
(api-v2 with a client_id scraped from SoundCloud's own web
assets, yt-dlp fallback), Mixcloud (public API with paging),
Google Groups (row scrape; the permission-denied page
returns Python's exact sign-in error), and Groups.io search
(topic scrape with DisplayShortTime nanosecond dates,
next-page following). YouTube channel feeds gain the
uploads-playlist fallback (issue #107) and the one-time
history backfill (issue #109), both under the stable
`yt:video:<id>` ids Python uses. NPR audio attach (embed
player first, then story-page candidates) and Sky Podfollow
audio attach run inside `store_parsed` for new articles and
through `attach_hosted_npr_media` for hosted providers,
with the `hosted_article_media` cache table (6-hour miss
retry, 8 resolutions per call) and the
`youtube_history_state` table added to the schema.
Discovery maps all of these subscription shapes to their
canonical URLs. Regression: batch35_slice (9 tests —
classifiers, normalization, uuid5 against CPython values,
listing parses, Db roundtrip).

Batch 35b resolution (v0.1.35): the rich-reader and
extraction residuals from Audit 4 are ported. Multi-page
following now runs in both readers: the text cascade
follows up to 6 pages with Python's `_find_next_page`
(link rel=next, then anchor rel=next, then labelled
anchors; the next-story guard, the per-host
no-pagination-follow list, and social-target skipping all
ported), merging with a faithful `_merge_texts` (paragraph
dedupe on the whitespace-collapsed lowercase key, sub-25
lines kept and outside the dedupe set, linearized table
blocks moved as units on the real marker regex, structure
markers bypassing dedupe) and appending the
metered-preview notice when page 1 carries the marker; the
rich reader follows the same chain, cleaning each page and
deduping whole pages by normalized text. YouTube watch
pages reconstruct through `crates/core/src/ytfull.rs`
(yt-dlp -J with the comments extractor args): Description,
Chapters (info chapters, else the description timestamp
grammar with the 3-line floor), Subtitles (ranked track
selection, cue text only, speaker chevrons stripped), and
Comments as parent/reply trees — in the text reader
directly and in the rich reader as section HTML with
16,384-char accessible chunks. Direct media URLs resolve
their ID3v2 show notes (bounded prefix read, v2.3/v2.4
frame parsing, USLT/COMM/TXXX candidates, longest wins);
the rich reader keeps the sanitized ID3 HTML. Google News
article URLs route to the Jina read-proxy before the
cascade and fail closed when it returns nothing (the local
signed decoder rides curl_cffi impersonation — the
documented rquest non-port). GitHub page reconstruction
gains commit, release, and discussion documents in
`threads.rs`; a repo discussion the REST API cannot serve
falls back to the server-rendered page scrape, as Python's
does. The article window gains Python's rich mode: with
`full_text_rich_view` on it renders the rich article on a
worker and offers the "Show in HTML view" switch between
the WebView and the text reader. One scanner fix rides
along: `tag_tokens` now treats HTML void elements as
self-closing — a bare `<link rel="next">` previously made
the balanced scanner hunt for a close tag that does not
exist and abandon the document, which is what blocked
link-based pagination. Regression: batch35b_slice
(13 tests — pagination forms and guards, merge semantics,
metered marker, ID3 v2.3/v2.4 fixtures, comment trees,
GitHub classification).

Batch 36 resolution (v0.1.36): the general range-cache
proxy is ported as `crates/core/src/rangecache.rs`
(Python `core/range_cache_proxy.py`, ~2,000 lines). A
second loopback-only listener serves /health, /health-split
and /media?id=<sid>; entries probe the origin once with a
one-byte range request (single-flight, with Python's
await_probe waits — 3 s, then up to 20 s more while the
total is unknown, because answering the player without the
real total makes it adopt the served window as the whole
file), learn the total length / range support / final URL,
and then serve byte ranges from an on-disk segment cache
whose layout is byte-for-byte Python's (`<sha256(url)>`
directory, `<start:012>-<end:012>.bin` chunks,
`mappings/<sid>.json`), so the two apps share a cache
directory safely. Cache misses stream from the origin
while being written to disk; partial chunks over 4 KB are
kept; a background downloader fills from the play head
(bootstrap burst, seek following with the 2 MB abort
window, 120 s idle stop); signed URLs that expire
mid-playback are re-resolved from the original URL and the
fetch retried once on transport errors and
401/403/404/410; origins without range support (or an
unknown total) are passed straight through with their own
headers. The player gate is a faithful port of
`_maybe_range_cache_url`: loopback, non-http(s) and .m3u8
pass through; googlevideo and the podcast tracking hosts
always proxy; otherwise `range_cache_enabled`,
`range_cache_apply_all_hosts` and `range_cache_hosts`
decide; the promodj Referer special case and the
`can_deliver_body` machine check (via /health-split,
cached 600 s positive / 120 s negative) are included; the
primary player load now goes through the cache, with the
existing /stream and /play fallbacks behind it. Recorded
simplifications, not silent drops: the server is a
process-lifetime listener rather than Python's restartable
object (its port-reuse and restart-while-VLC-connected
machinery exists to survive restarts this design never
does); responses close per request like the rest of the
local proxy; Python's split connect/read timeouts ride the
dedicated agent's single 60 s timeout; the player's
`prefetch_bytes` entry field is carried but, as in the
current Python, read by no fetch path; the player's
range-proxy retry counters belong to its VLC stall
recovery and stay with the Batch 38 engine port, where
the Rust watchdog already covers the same ground;
playback headers and the pre-redirect original URL are
accepted by the gate but the current player call site has
neither to pass (no header plumbing exists in the Rust
player yet — Batch 38 decides it). Regression:
batch36_slice (5 tests — segment math, header parsers,
gate rules, and an offline end-to-end pass against a local
origin proving the second read of a range costs no origin
fetch).

Audit 5 addendum (2026-11-01, after v0.1.36 — full report in
docs/AUDIT-5-2026-11-01.md): a fresh mechanical + hand audit of
Python v2.5.5 against this tree found fourteen confirmed items
in neither this plan nor the code, now part of the plan: (1)
FeedSearchDialog term search — Python searches 25 directory
sources by name; the Rust dialog only resolves URLs (the §2
row's "directory search missing" understates it and was never
closed); (2) MSAA Direct Annotation control names (Python
#131 — IAccPropServices names for tree/list/reader/player
controls; Rust has none); (3) managed yt-dlp preference
(Python #130 — find_ytdlp never consults the managed copy);
(4) the empty-extraction 15-minute LRU and the escaped-markup
unescape branch (Python #133/#134); (5) ytfull's language_match
simplification vs Python #132 (zh script table, -orig strip,
rank scale); (6) Miniflux targeted refresh (per-feed calls,
workers, 5xx cooldown, soft deadline, force-skip + its six
config keys); (7) the main-window full-text view cache
(max_cached_views / cache_full_text); (8) article structure
options (article_structure_* settings shaping reader
linearization); (9) source listing caps as config keys (Batch
35 hard-coded Python's defaults); (10) github_token and
groups_io_api_key conditional paths; (11) per-channel HTTP
headers for playback/casting (http_headers.py — decide with
Batch 38); (12) article-window rich-view navigation policy;
(13) the installed-browser cookie import consent gate
(installed_browser_cookie_import_enabled); (14)
installed-browser true-version UA detection. The report also
warns that this file's §1/§2 tables and §5 lists are stale
snapshots contradicting the resolution paragraphs in ~15
places — the resolutions and Audit 5 govern.

Batch 37 resolution (v0.1.38): the Accessible Browser
(Python `gui/accessibility.py`, §5 item 3 — the flattened,
speech-first VoiceOver window) is ported. Core
(`crates/core/src/accessible.rs`): the view-entry model —
special views (all / unread:all / read:all / favorites:all),
categories in case-insensitive depth-first path order,
feeds title-sorted with Python's ", unread: N" suffix and
parenthesized category breadcrumb, the `Uncategorized`
sentinel, and visible-entry filtering against the expanded
set with ", expanded" / ", collapsed" label suffixes and
two-space-per-level indent; article-body arbitration
(web extraction vs feed-cleaned vs feed-raw vs provider
fetch, longest wins, the 1,500-character provider floor,
Python's cacheability rules); chapter normalization and
timestamp formatting (h:mm:ss past an hour, mm:ss below);
the reader header, list label, relative-date humanizer and
whitespace-term filter; the macOS VoiceOver probe. Paged
view queries land as `Db::articles_page` (all / unread: /
read: / favorites: prefixes, bare feed ids, and
`category:` paths through the membership-clause query).
App (`app/src/accessible_browser.rs`): the singleton
window itself — views list, 400-article pages with Load
More, the reader (header + body + chapters), a 300-entry /
8 MB body cache with look-ahead prefetch of the next eight
articles, the Rich Full-Text View WebView mode with the
port's link-diversion policy, the full File / Article /
Player / Tools / Help menu set and both context menus
matching Python's commands one for one, MSAA Direct
Annotation names on every control, and Ctrl+W / Ctrl+R /
Ctrl+F / Ctrl+D / Delete handling. Main-window commands
run through delegates wired in `main.rs` (Python's
`_delegate` targets: add feed, add category, smart folder,
OPML import/export, Takeout import, refresh, settings,
the queue trio, the equalizer, persistent search, filter
rules, Find Feeds, the podcast archive, Video Search,
cookie import, shortcuts, feed errors, update check,
About); download, view history, mark-view-read, feed
removal, category removal, notification toggling and
feed-URL copy are per-article/per-view adapters over the
same core paths the main window uses. The window opens
from View > Open Accessible Browser and the remappable
`view.accessible_browser` command, and auto-opens once,
about 900 ms after startup, when VoiceOver is running on
macOS (Python's `_maybe_open_accessible_browser_for_
voiceover`; tray-only launches skip it, as in Python).
Recorded deviations, not silent drops: chapters come from
the local chapters table only (YouTube chapters reach it
through the playback path, which writes the same table;
Python's frame additionally resolves them inline);
"View Feed Description" shows the selected article's
description because Rust feeds carry no feed-level
description field; Delete has no undo inside this window
(Python's restore view is the Deleted view, which this
window's view model does not include — restore stays in
the main window); the relative-date helper formats
absolute dates in the stored wall time rather than
converting to local time (core stays free of a timezone
database); prefetch uses two workers to Python's three.
Regression: batch37_slice (core view model, chapter
formatting, body arbitration, date/header/label helpers,
paged queries).

Batch 39 resolution (v0.1.39): hosted account storage
isolation (PARITY-2026-11-01 gap 1) is closed. The
per-account cache files already existed
(`rss.hosted-<provider>-<credential hash>.db`, core
provider.rs), but three paths still crossed accounts.
(1) The Sync Providers action synced every configured
account into whichever database was active — with Local
active, The Old Reader, BazQux, Inoreader and Miniflux
all poured into rss.db and collided on the shared
`feed/<url>` stream ids; with a hosted account active,
the other accounts contaminated its cache. Each sync
worker now opens its own account's file through the new
`provider::db_path_for_provider` (active_db_path
delegates to it). (2) Read-state propagation tried every
configured GReader service in turn and wrote to whichever
accepted, leaking read state across accounts when the
same item id existed on both; it now writes only to the
active provider, and the Miniflux branch is gated on
Miniflux being active. (3) Stray rows from the
pre-isolation syncs are purged: `Db::purge_hosted_rows`
removes hosted-shaped rows (GReader `feed/…` feeds,
`miniflux:…` feeds, `miniflux-entry:…` articles, plus
their chapter, playback, label and version rows) and
runs at startup whenever Local is active — local feed
ids are UUIDs, so the shapes are unambiguous, and hosted
servers are the source of truth for the dropped caches.
Each hosted cache likewise drops the other kind's rows
before a sync (`purge_hosted_rows_except`, called from
the provider refresh paths). Same-kind rows written into
the wrong account's cache by the old Sync action cannot
be told apart by shape; they age out through normal
sync, and deleting that account's rebuildable cache file
clears them outright. Regression: batch39_slice
(per-account paths incl. credential sensitivity and
re-basing, full purge keeping local data, per-kind purge
keeping the owning kind's rows and playback state).

Batch 40 resolution (v0.1.40): the Accessible Browser is
REMOVED from the Rust port at the owner's direction
(2026-11-01), reversing the Batch 37 resolution above.
Research first, per the owner's instruction ("search
GitHub for voiceover rust... if a separate GUI for Mac is
not required, drop it"): GitHub most-recent and
most-starred, Codeberg and GitLab (no relevant results on
either). Findings: blind-focused wxDragon applications
(BrailleCraft, Wixen Mail) ship ONE GUI for all platforms
with VoiceOver as a primary audience; wxDragon upstream
actively maintains VoiceOver support in the main surface
(dedicated macOS/VoiceOver issues; it adopted the new
wxWidgets accessibility APIs in 3.3.4); wxWidgets 3.3.4
(2026-10) advertises improved accessibility on all
platforms including wxOSX, and the historic 2013
wxListCtrl/VoiceOver tracker item (wxWidgets#15411) is
closed as completed. Nothing found requires a separate
macOS GUI; AccessKit addresses custom-drawn toolkits, not
wx native controls, and was already rejected for this app
with evidence. Python keeps its Accessible Browser (there
it is the primary VoiceOver reading surface); the removal
is a deliberate Rust-only deviation, recorded as Audit 6
item D1. Removed: app/src/accessible_browser.rs, the core
accessible module, Db::articles_page, the View menu item,
the view.accessible_browser shortcut command (the Rust
registry now intentionally differs from Python's by this
one command), the VoiceOver auto-open, the F1 help topic
mapping, and the help-guide section in all 16 locales.
The main window is the single reading surface on every
platform; its reader already carries the mitigations that
matter (content replacement on load, announcements).
Source depth resolution (v0.1.41; gaps 3-4 — PARITY-2026-11-01
gaps 3-4): the specialized source readers are completed.
(1) HTML forum thread linearization is ported
(`forum_sources::forum_blocks` / `forum_thread_text` /
`forum_thread_html`, wired into the extraction cascade's
site-specific branch and the rich reader's reconstruct
step): Python's `_FORUM_LAYOUTS` for Invision Community
(article.ipsComment, [data-role=commentContent],
h3.cAuthorPane_author, time), vBulletin 4
(li.postbitlegacy[id], blockquote.postcontent,
.postcounter/.username/.postdate, .signature junk),
FluxBB and Drupal ride along, expressed over the
extractor's balanced tag scanner as tag/class/attribute
selector steps — no CSS engine needed. Per-post junk is
stripped, ellipsized link text is expanded to the real
href, the body wrapper's class/id is dropped, and the
hosts are Python's `_FORUM_THREAD_HOSTS` gate. XenForo
has NO Python layout (the audit names it, the Python
tree ships none); a XenForo 2 layout in the same shape
(article.message--post, .bbWrapper, .message-name,
attribution-opposite post number) is added keyed on its
distinctive markup alone. (2) Google Groups conversation
rebuild: `google_groups_thread_document` parses the
server-rendered div.eH2Xlc messages (region body, h3
author, .zX2W9c date, data-message-id, quote blocks
dropped) into the semantic thread document, and
`google_support_thread_document` decodes Google Help
Communities' embedded `var thread_view` payload (JS
single-quote literal decode, nested-array topic/replies,
micro/milli/second timestamp labels); both are served
through `web_thread_html` from the reconstructed-article
path. (3) Discourse reconstruction now matches Python's
completeness: print=true topic fetch, missing post ids
from the stream fetched in batches of 100, post_number
ordering, 10,000-post cap, display-name authors,
reply-to markers. (4) YouTube RSS-error/uploads backfill
is finished to Python's semantics: the history state
table now uses Python's own columns (done/last_attempt —
early Rust builds created state/attempted_at, renamed
in place inside schema init), `youtube_history_due`
mirrors Python exactly (done=1 re-lists immediately, the
day-long retry after a failed attempt), the attempt is
marked before listing, the history listing runs before
the RSS fetch at Python's 5,000-item / 300 s budget and
marks done only on a stored non-empty listing, an empty
RSS-error fallback listing re-raises the original RSS
error instead of succeeding with zero articles, and
channel/search listing dates are date-only as Python's
parser emits. (5) FeedSearchDialog depth: all of
Python's directory sources were already present; the
remaining per-source detail is now ported — Reddit
subscriber suffixes, Lemmy/Kbin "Lemmy - name (N subs)"
details, PieFed community/user details, Mastodon
@acct/follower and tag recent-use details, Bluesky
handle/did details plus the OpenRSS tag result,
Mixcloud follower counts and per-user playlist
surfacing, SoundCloud follower/owner/track-count
details, Groups.io per-row details, Feedspot host
details, the NewsBlur 20-result cap, website-scan feed
titles for the first five candidates, and YouTube's
query variants, match scoring, merged playlist-first
ranking and "Name / @handle | N plays" details.
Regression: source_depth_parity (13 tests — the Python
forum fixtures for Invision/vBulletin/FluxBB/Drupal, a
XenForo fixture, Google Groups + Support rebuilds,
history-state due rules incl. the legacy-column repair,
feedsearch details) plus threads.rs discourse unit
tests.
Casting depth resolution (v0.1.42; PARITY-2026-11-01 gap 8):
the cross-protocol cast session depth is ported. Audit
against Python core/casting.py found Chromecast already at
depth (CASTv2 session with play/pause/stop/seek/volume/
status and the persistent controls window) while every
other protocol was play-only in Rust: castupnp could start
DLNA/Sonos/Roku/Kodi playback but had no transport control,
status, seek or volume at all; AirPlay streamed with no
pause, position or live volume; no protocol but Chromecast
could resume, and nothing advanced a queue on a receiver.
Now: (1) Unified sessions — core/cast.rs gains Python's
protocol-neutral layer (CastStatus in get_status's shape,
CastClock porting _clock_set/_clock_position, format/parse
_hms, the per-protocol seek/volume capability table where
Roku has neither, CastDevice display_name/unique_id), and
castupnp.rs gains the full per-kind control dispatch
(pause/resume/stop/seek/set_volume/status): AVTransport
Pause/Play/Stop/Seek REL_TIME plus RenderingControl
SetVolume (URL derived by Python's service-name swap) and
GetPositionInfo/GetTransportInfo status for DLNA and Sonos,
Kodi JSON-RPC PlayPause/Stop/Seek/Application.SetVolume/
GetProperties through a shared rpc helper that treats a
JSON-RPC error as a refusal, and Roku's Play-keypress
toggle gated by the caller's clock state with Home as
stop, exactly Python's gates. Chromecast answers the same
surface through castsession's MediaStatus::to_unified.
(2) Seek/resume — Chromecast loads carry currentTime
(load_media_at) with Python's one corrective seek when a
receiver reports more than 5 s short of a start over 5 s;
DLNA/Sonos and Kodi plays poll for the playing state (8 s,
Python's window) and then seek, since renderers take no
start position with the URL; RAOP resumes by decoding
from the offset. The controls window persists cast
positions into playback_state under a per-item cast key
through the shared ResumePolicy (same back-off, minimum
and completion rules as local playback), so a later cast
of the same item resumes where the last cast left off.
(3) Queue advancement — core CastQueue ports the local
player's media-finished advance for cast sessions: an
idle/stopped status after the item was seen live is the
finish event (an idle report before anything played is
not), and the unified controls window loads the next
item — the window now serves every protocol, appends the
configured play_queue after the current item, adds a Next
button, and saves each finished item as completed.
AirPlay joins the same session shape through raop's new
RaopHandle (Python's _AirSession): pause holds the RTSP
session open and freezes the streamed-frame position
clock, volume rides SET_PARAMETER mid-stream, a seek
reopens the source at the target as Python's pending
seek does, and natural end-of-source raises the finished
flag the queue advance consumes. Verified by
crates/core/tests/cast_depth.rs against fake receivers
on loopback (a recording UPnP renderer, Kodi JSON-RPC
and Roku ECP server, plus clock/queue/HMS unit tests) —
8 tests, including the resume-seek ordering and Roku's
clock-gated toggle. Not live-tested: multicast discovery
is blocked from this VM (the sandbox refuses multicast
sends), so "RB Room" was not discoverable and no physical
device was exercised, per the device rule. One wiring
note: app/src/main.rs still routes only Chromecast into
the controls window (its other kinds go through the
fire-and-forget cast_to_device); main.rs is outside this
batch's files and carries Batch 40's edits, so routing
the remaining kinds through casting::open — which now
handles them — is a one-line follow-up there.
Batch 42 resolution (v0.1.43; Audit 6 G2): the Windows Add Shortcuts
dialog is ported. Core (`crates/core/src/windows_shortcuts.rs`,
GUI-free): a line-for-line port of Python
`core/windows_integration.py`'s shortcut family. Python does
NOT use a raw COM IShellLink path — `_create_shortcut` runs
PowerShell (`-NoProfile -NonInteractive -ExecutionPolicy
Bypass -EncodedCommand <base64 UTF-16LE>`) driving
`WScript.Shell.CreateShortcut` with TargetPath / Arguments /
WorkingDirectory / IconLocation taken from `get_launch_parts`
and Description "BlindRSS"; Save overwrites an existing `.lnk`
silently. The Rust build is a compiled executable, so the
"frozen" launch tuple applies (target = the exe, no arguments,
working dir = the exe's folder, icon = the exe). No new
dependency was added: the `windows` crate is only a transitive
dependency of other crates, not a direct one, and none was
needed — the port spawns powershell exactly as Python does,
with the same 20 s / 10 s timeouts. Desktop: the known-folder
path via `[Environment]::GetFolderPath(DesktopDirectory)`
(with the WScript.Shell SpecialFolders fallback inside the
same script), else Python's OneDrive / OneDriveConsumer /
~/Desktop candidate chain. Start Menu:
`%APPDATA%\Microsoft\Windows\Start Menu\Programs\BlindRSS.lnk`.
Taskbar: exactly Python's best-effort flow — create a temp
`.lnk`, invoke the Shell.Application "Pin to taskbar" verb
("already-pinned" counts as success), and when the verb is
unavailable fall back to writing the `.lnk` into
`%APPDATA%\Microsoft\Internet Explorer\Quick Launch\User
Pinned\TaskBar` with Python's "pin verb unavailable" success
message; no pinning API was invented. Off Windows every
requested kind fails with "Shortcuts are only supported on
Windows.", as in Python. App (`dialogs::show_add_shortcuts`):
Python's AddShortcutsDialog verbatim — title "Add BlindRSS
Shortcuts", the two-line intro (including the taskbar-pinning
warning), Desktop and Start Menu checked, Taskbar unchecked,
all three checkboxes disabled off Windows, 460x280 — followed
by Python `on_add_shortcuts`' result message box ("Desktop:
OK: ..." lines, warning icon when any kind failed, silent
return when nothing is checked). Menu visibility follows
Python `should_show_add_shortcuts`: shown everywhere except
macOS (the brief's "Windows-only" phrasing was narrowed
against the Python spec; the parent wires the File-menu item
per the placement spec in the batch report). Regression:
batch42_slice (PowerShell literal quoting, create/pin script
shapes, EncodedCommand base64 round-trip, Start Menu/taskbar
paths, Desktop fallback chain, launch tuple, off-Windows
results, result-line formatting).
Batch 41b resolution (v0.1.44; Audit 6 item G1.3): the settings
dialog is now a Notebook matching Python's SettingsDialog
(gui/dialogs.py) instead of one long single page. Pages in
Python's fixed order: General, Feeds & Articles, YouTube,
Media Player, Provider, Notifications, Translate, List
Headers, Advanced, CAPTCHA Solving — future pages append
at the end only, never mid-notebook. Every pre-existing
Rust control survives with its config key, default and
read/write behaviour unchanged (verified by diffing the
set of config keys read and written before and after);
what changed is placement, page/group assignment, control
order within each page and labels, which now follow
Python's per-page builders, including Python's absorbed
groups: "Startup & Tray" and "Backup" inside General,
"Translation Updates" inside General, "Media tools",
"Downloads" and "Sounds" inside Media Player, and
"Announcements" inside Notifications. The Downloads panel
follows Python's current semantics: it is not a page of
its own — Python builds it eagerly and shows it as the
"Downloads" group inside Media Player, and the Rust
dialog does the same. Pages are scrolled windows (Python
scrolls its longest page, Notifications, the same way) in
a 660x780 dialog. Deliberate equivalents, recorded here:
the List Headers page opens the existing Article Columns
dialog through the established "columns" intent instead
of embedding an editor, because dialogs.rs's
show_article_columns loads the layout from config and
saves it on its own OK — embedding it would duplicate
that editor and split its save semantics; Python controls
whose form differs in Rust keep the Rust form (refresh
interval and playback speed stay free fields where Python
uses preset choices, the category-tree state stays a
checkbox where Python uses an expanded/collapsed choice,
SponsorBlock categories stay a comma field where Python
uses a checklist, the CAPTCHA service stays a text field
where Python offers 2captcha/SolveCaptcha, and the tree
of per-provider fields stays fully visible where Python
shows only the active provider's panel). Rust-only
controls with no Python settings counterpart (resume
timing fields, seek/volume steps, silence threshold
fields, max downloads kept, the Equalizer... entry and
the maintenance action row, which stays dialog-level
below the notebook) sit on their closest matching page.
Python settings controls with no Rust counterpart are NOT
invented here and remain unported settings surface: the
interface-language picker (`language`), Debug mode
(`debug_mode`), Max Cached Views (`max_cached_views`),
sound file pickers (`sound_refresh_complete` /
`sound_refresh_error`, honoured by the app but not
editable), Groups.io API key (`groups_io_api_key`) and
GitHub token (`github_token`) groups on Provider, the
Inoreader Redirect URI field, YouTube's auto-import
cookies.txt toggle (`auto_import_browser_cookies`),
play-via-download (`youtube_play_via_download`) and
playback cache folder/size (`youtube_play_cache_dir` /
`youtube_play_cache_max_mb`), the range-cache debug toggle
(`range_cache_debug`), the storage-location picker
(`data_location`; the Advanced page shows the current
data folder, as before) and Video Search's adult-sites
toggle (`enable_adult_search`). Several of those keys are
already read by core/app code; only the settings UI is
missing, which is a separate batch from this restructure.
VLC platform resolution (v0.1.46; PARITY-2026-11-01 gap 11,
platform acceptance for the libVLC backend): the audit first
corrected the premise. Python's player is audio-only end to
end: `core/vlc_instance.py` creates its shared instance with
`--no-video` and no window handle is ever passed on any
platform. The Rust backend deliberately diverges — it owns a
video surface (`app/src/media_backend.rs`) and already
embedded it on Windows (set_hwnd) and macOS (set_nsobject);
Linux was the hole, falling back to libVLC's own video
output. (1) Linux embedding is now implemented:
`attach_video` resolves the wxGTK panel handle to an X11
window id through dynamically loaded GTK/GDK
(`gtk_widget_get_window` → display type-name check for
`GdkX11Display` → `gdk_x11_window_get_xid`) and calls
`libvlc_media_player_set_xwindow`. Every step fails closed
to no embedding (unrealized widget, Wayland-only session,
missing GTK/GDK) rather than handing libVLC a foreign
window, and `MediaBackend::load_uri` re-attaches on every
load because the XID does not exist until the widget is
realized. (2) Instance creation now mirrors
`core/vlc_instance.py::_build_args` +
`core/vlc_options.py::build_vlc_instance_args` option by
option through pure, tested builders: `--no-plugins-cache`
first with dedup, `--input-fast-seek`, instance-level
`--network-caching` (config key, 500 fallback, clamped ≥ 0)
and `--file-caching` (max 500), `--http-reconnect`, and —
Windows only, exactly as Python gates them — `--aout=
mmdevice` with `--directx-volume`/`--mmdevice-volume`
seeded from the saved volume. The one deliberate divergence
is `--no-video` itself (see above). When libvlc_new()
returns NULL the backend retries once with Python's exact
minimal set (`--no-plugins-cache --no-video`) before
falling back to wx MediaCtrl, instead of giving up on the
first rejection. (3) Media-option caching now follows
Python's player.py gate exactly: the local caching keys
apply only to range-cache `/media?id=` URLs, not to every
loopback URL (the /stream and /play transcoders take the
normal keys). (4) Release-build validation: Python BUNDLES
VLC on every platform (Windows installer plugins, macOS
VLC 3.0.23 libs+plugins, Linux portable `vlc/lib` +
`vlc/plugins` from the distro packages), while the Rust
release zips bundle nothing and neither CI nor release
installs libVLC — the release default is therefore an
installed VLC 3 when present and wx MediaCtrl otherwise.
The code side now supports the Python bundle layout if one
is ever packaged: candidates cover `<exe>/vlc/lib/<lib>`
and the Linux system directories Python's portable spec
searches, the sibling libvlccore is preloaded on all
platforms (a bundled copy's core is not on the system
search path; this was Windows-only), and a bundled copy
gets `--plugin-path=<exe>/vlc/plugins`. The packaging
side — installing/bundling libVLC in ci.yml/release.yml
and running the ignored libVLC smoke test in CI — needs a
workflow change and the owner's direct OK; it is recorded
here as the remaining gap-11 item, not done. Verification
limits, stated plainly: this VM has no display and no
libVLC installed, so the X11 attach and live playback
could not be exercised here; what is verified is the
absence path (backend construction fails cleanly to
MediaCtrl), the GTK/GDK symbol surface present in the
system libraries, and the option/handle logic by unit
tests ported from Python's own test_media_player_config.py
(wasapi seeding, off-Windows option gating, plugin-cache
dedup, caching clamps, bundle layout, fail-closed XID).
Owner-side acceptance on a real Linux desktop (X11 and
Wayland sessions, a video podcast) remains. Regression:
media_backend::vlc tests (10 non-ignored, incl. the ported
Python config tests).

## 8. Performance audit resolution (2026-11-02; owner-reported)

The owner reported ~30 s startup and article-list lag when
entering podcasts. Audit (reports/perf-audit-2026-11-02.md,
with a 150-feed / 60,000-article benchmark) found three root
causes, all parity misses against Python rather than new
design: (1) the Rust SCHEMA never created Python's article
indexes — Python `core/db.py::init_db` creates six indexes
on articles plus the deleted_articles and chapters indexes
on every init, so a Rust-created database had none and every
list/count query was a full table scan; (2) `rebuild_tree`
ran `Db::unread_count` per feed — once per category node and
again per feed label — where Python runs one grouped
`SELECT feed_id, COUNT(*) ... GROUP BY feed_id`
(`providers/local.py`); per-feed counting measured 11.61 s
at benchmark scale, one grouped query 0.014 s; (3) the list
queries selected the full `content` of 300-500 rows the list
never displays (podcast bodies are the largest text in the
library), through a `(?1 IS NULL OR feed_id = ?1)` shape
that defeats the feed index even when it exists. Fixes: the
SCHEMA now carries Python's full index set (IF NOT EXISTS,
created in the same BEGIN IMMEDIATE schema init, so existing
index-less databases gain them at first open; Python's
idx_articles_id_unique is deliberately skipped — it only
duplicates the primary key); `Db::unread_counts()` returns
the grouped map and the tree build reads it; `Db::articles`
uses two direct-predicate shapes that take
idx_articles_feed_id_date_id / idx_articles_date_id; new
light list variants (`articles_light`,
`articles_for_category_light`, `search_articles_mode_light`)
return rows with `content = None`, and the GUI resolves a
body on demand through `article_by_id` at the points that
read one (reader selection, full text, play, activation,
download, copy-text, description view), caching it back into
the row. Bulk consumers that process bodies (smart folders,
refresh, providers) keep the full loads. The search-results
insert loop now gets the same Freeze/Thaw batching the
refill path already had. Regression: crates/core/tests/
perf_slice.rs (index presence on a fresh database, grouped
counts equal per-feed counts, EXPLAIN QUERY PLAN proves the
feed refill searches by the feed index, light loads skip
bodies while full loads and content search keep working).
Owner-side acceptance: cold-start time and podcast list
responsiveness on the owner's machine and library.

**Audit 7 resolution (2026-11-02).** A fresh sweep of Python
v2.5.5 against the Rust tree (config keys, modules, dialogs,
provider surface; interim files under the migration reports)
found one user-visible miss plus one ignored tuning pair.
Finding A: Python pages the article list at 400 rows behind a
"Load more items (Enter)" placeholder row; the Rust list
hard-capped feed views at 300 and category views at 500 with
no way to browse older articles (a 1,000-episode podcast lost
everything past the newest 300 to browsing). The standard
view (feed / category / all-articles, date sort newest-first,
no read or media filter — the configuration where SQL page
order is display order) now loads the first 400 light rows
plus a total count (`Db::articles_count` /
`articles_for_category_count`, with OFFSET pages via
`articles_light_page` / `articles_for_category_light_page`),
shows the placeholder while rows remain, and appends the
next page when the placeholder is activated, landing on the
first new row. The category query gained the feed query's
`id DESC` tiebreak so page order is total (episodes share
dates; unstable order would repeat or skip rows across
pages). Python's background auto-append thread is
deliberately not ported (rows materialising under a screen
reader mid-browse is worse than an explicit Enter; the
placeholder is Python's own accessible affordance).
Filtered or non-date-sorted views keep their capped loads;
paging them is the recorded follow-up. Also fixed: the
silence skipper hardcoded Python's merge-gap/padding
defaults (260 / 60 ms) and ignored the
`silence_skip_merge_gap_ms` / `silence_skip_padding_ms`
config keys — `detect_silence_tuned` now takes them from the
player's config reads. The remaining Audit 7 verdicts were
non-gaps: `max_cached_views` caps Python's per-view list
cache, which Rust does not need (views re-query cheaply and
stay fresh); the other silence keys belong to Python's
WebRTC-VAD incremental scanner, of which the port ships the
documented ffmpeg fallback position (SUPERSEDED by the
Audit 8 Task 21 resolution at the end of this file: the
VAD scanner and the guarded skip policy are now ported); dead Python keys
(`auto_download_podcasts`, `auto_download_period`) are dead
on both sides. Tests: `audit7_slice.rs` (feed pages tile the
view without overlap, category pages/count cover the
subtree, finalize honours tuned values and the shipped
defaults equal Python's). Owner-side acceptance: browsing
past episode 400 of a large podcast via the placeholder.
Batch 41a resolution (GUI parity, Audit 6 G1.1/G1.2/G1.4/
G1.5/G1.6): the main-window menu bar is rebuilt to
Python's exact map (gui/mainframe.py init_menus +
gui/menu_mnemonics.py): File / Feeds / Edit / View /
Player / Tools / Help, item for item and label for
label — File gains Open Article..., Open Media URL...,
Mark All Items as Read, Add/Remove Category, Back Up
Data.../Restore Data... (renamed from Backup.../
Restore...) and Configure Persistent Search...; Feeds
is Python's nine items; View carries Show Search Field
and Rich Full-Text View checks, the Article Filter
submenu (read-status and media radio groups), the
Accessible Browser item (unchanged here; Batch 40
removes it on rebase), Show/Hide Player, and the Sort
By submenu (six radio keys plus the Ascending check);
Player is new — Show/Hide Player, Play/Pause, Stop,
Play Queue..., Play Next/Previous in Queue, a Playback
Speed submenu (Increase/Decrease/Normal (1x)), Equal-
izer..., Rewind/Fast Forward/Volume Up/Volume Down
with Python's in-label shortcut text, and a Chapters
submenu; Tools and Help match Python, with Check for
Updates... and Announce Version now in Help and a
YouTube Account... item wired to the existing sign-in
dialog. The Rust-only Article menu is gone: its actions
live where Python puts them (context menus and the
shortcut registry); every pre-existing handler remains
in the menu_actions table and reachable (Sync Providers
in the All-Feeds tree context menu, Prune Old Articles
via Settings, Deleted Articles... in the list context
menu, Find via the reader's Ctrl+F/F3 handling).
Tree context menus now vary by node kind as Python's
do (category / feed / smart root / smart folder), with
Python's labels (Edit Category, Delete Category and
Feeds, Export Category to OPML..., Reset Title to Feed
Default — with a new main-window handler that re-fetches
and resets — and the Image Alt Text submenu with
Python's item labels). The article list context menu is
selection-aware: Mark {count} as Read/Unread and Delete
{count} Articles carry counts, queue items read Add/
Remove {count} to/from Play Queue (new selection-wide
queue actions), and it carries Open Article, Open in
Default Browser, Copy Link(s), Copy Text, View Feed
Description..., Copy Media Link (direct media only),
Download/Download As... or Detect Audio when the item
has no media (new handler: page scan on a worker,
persisted through a new RefreshMsg::AudioDetected),
Copy Image Link (new handler, first <img> in content),
Open Play Queue..., a Chapter Links submenu fed from
the chapters table's hrefs (fixed id slots dispatching
to the stored URLs), Toggle Favorite and View
History... when the article has versions. The reader
content context menu is Copy / Select All only, via the
stock ids, as in Python. The article window's menus are
Python's File / Edit / View (Open in Browser, Reload,
Close Window; Copy Text, Copy Link; HTML View check) on
the existing handler ids. The tray menu is Python's
set: a disabled "Unread: {count}" label (kept current
by the drain tick), Restore, Show/Hide Player, Refresh
Feeds, Play/Pause (falling back to playing the selected
article when no player is open), Stop, Rewind, Fast
Forward, Volume Up, Volume Down, Exit. Category
Properties (G1.6) is a real dialog in dialogs.rs —
rename the leaf plus a parent picker headed by
"(None - Top Level)", excluding the category itself and
its descendants — applied through Db::rename_category;
Edit Category (tree context and feeds.edit_selected)
opens it, and the pure path helpers carry unit tests in
dialogs.rs. Recorded gaps: "Add Shortcuts..." is
absent, not dead — it is Batch 42 (Audit 6 G2), and
Python itself only offers it on Windows. Player-menu
and tray Rewind/Fast Forward/Volume dispatch through
player::dispatch_command under the names player.rewind,
player.forward, player.volume_up, player.volume_down,
which have no arms in player.rs yet (outside this
batch's files); until that follow-up lands they report
"no player is open" style status text, while Show/Hide,
Play/Pause, Stop, Speed and Chapters dispatch to the
player's existing arms. Python's Chapters submenu also
lists the current track's chapters and its speed
submenu offers preset rates; both need player state the
main window cannot see in the Rust architecture, so the
submenus carry the static entries only (Show
Chapters...; Increase/Decrease/Normal). Python's
category context menu offers Add Subcategory only when
the provider supports subcategories; the local Rust
provider does, so the item is unconditional here.
Regression: dialogs.rs category-properties unit tests;
full workspace gate at the batch commit.

Integration addendum (Batch 41a, at the v0.1.49 landing): the
branch was written before Batches 40/42 landed, so three
parent adjustments were made while merging. The Accessible
Browser menu item and action were dropped (Batch 40 removed
the feature at the owner's direction). "Add Shortcuts..."
was placed in the File menu after "Configure Persistent
Search..." — its Batch 42 handler exists — with the
not-macOS gate Python applies. And the player dispatch names
the paragraph above flags as armless (player.rewind,
player.forward, player.volume_up, player.volume_down) now
have arms in player.rs: the seek/volume button handlers were
lifted into shared closures the PLAYERS action match calls,
so the Player menu and tray entries work whenever a player
is open. The Chapters/speed submenu statics stand as
recorded.

Release signing resolution (2026-11-02, at the owner's
direction — "using shipfitter like BlindRadio and its
signing key, kept private, not into the repo"): releases
are now minisign-signed exactly as BlindRadio's are. The
workspace gains crates/xtask with the `sign` command ported
from BlindRadio's xtask (the env-parameter split in
sign_file_with is the only deviation, for tests); the
release workflow signs each platform zip in its leg with
the MINISIGN_KEY repository secret (MINISIGN_PASSWORD
unlocks the encrypted key), uploads the `.minisig` beside
the zip, refuses to publish unless every asset and
signature arrived, and lists the signatures in SHA256SUMS.
The public half (key ID 151E848B8939A63F — the same key
BlindRadio publishes) is committed at
crates/core/assets/minisign.pub and copied into every
package; the secret half exists only as the GitHub
repository secret, never in git. The updater closes the
loop: download_and_stage_update still checks SHA256SUMS,
and now also requires the zip's `.minisig` and verifies it
against the embedded public key before staging — fail
closed, so releases from before signing (which carry no
signature) can no longer be staged by a signing-era build.
Regression: the xtask round-trip test signs with a
throwaway keypair and verifies through minisign-verify
(the updater's own call), plus an encrypted-key test
proving the password path; core signature tests cover the
fail-closed cases.
Gap 2 resolution (hosted GUI mutation completeness,
PARITY-2026-11-01 gap 2): the GUI paths that bypassed the
provider trait and wrote only the local cache now route
through the hosted provider, mirroring Python — no more,
no less. (1) Mark All Read (act_markall): with a hosted
provider active it runs on a worker through
provider.mark_all_read (remote first, cache after, per
the trait); the All Feeds view maps to the provider-wide
mark (None), a feed view to that feed, and a category view
fans out to one per-feed mark for every feed in the
category (the trait's per-feed endpoints), with the total
or the first remote failure reported through the
ProviderAction drain in the status bar. Smart-folder and
Recommendations views have no server stream and keep the
local path. Articles moved or labelled into a category
from a feed outside it are not covered by the per-feed
fan-out — Python's server-side label/category mark covers
them; recorded as the one mapping divergence. (2) Feed
Properties (act_feed_props): the pre-dialog feed is the
snapshot; the dialog still applies its edits to the cache
Db (including the Reset Title re-fetch), and the
title/url/category diff is then pushed through
provider.update_feed on a worker — the trait does the
remote edit and re-applies the cache writes idempotently,
and a remote failure surfaces in the status bar. The
encoding override stays local-only (no server field
exists, as in Python). The Edit Feed prompts and Set Feed
Category route their changes through the same
push_feed_update helper (changed fields only; Some("")
clears a category — a GReader label removal, a Miniflux
category_id change via update_feed_remote). (3) Favorite
toggles: propagate_favorite_state now sits beside
propagate_read_state and is called from the favorite
action after the local write — active provider only
(Batch 39 isolation): GReader via set_starred_remote,
Miniflux via set_bookmark_remote (read-then-toggle-only-
if-different). (4) Category operations: New Category
mirrors creation to Miniflux (leaf title only — Miniflux
categories are flat, Python ignores the parent the same
way); the GReader family needs no create call (Python's
add_category there is a no-op; labels appear on
assignment). mirror_miniflux_category is now
mirror_hosted_category: rename/delete still mirror to
Miniflux, and now also to the GReader family through the
new GReaderClient rename_label_remote (/rename-tag) and
disable_label_remote (/disable-tag) — Python's exact
endpoints and form fields — covering rename, delete, and
delete-with-feeds. Regression: hosted_gui_slice (fixture
HTTP servers: GReader rename/disable endpoints, form
fields and failure surfacing; Miniflux category creation
body). The remaining GUI mutation paths are app-only
wiring over the already-tested trait/client layer.

Integration addendum (gap 2, at the v0.1.52 landing): the
Category Properties dialog added by Batch 41a still called
the old mirror_miniflux_category name; it now calls
mirror_hosted_category with the same arguments, so a rename
made through the dialog mirrors to GReader-family labels as
well as Miniflux, like every other rename path.
Podcast archive resolution (PARITY-2026-11-01 gap 6):
podcast archive depth is closed. The core scanner
already had progress + cancel, lineage following, CDX
parsing and identity bridging; this batch adds what the
audit found missing around it. (1) Scheduling lives in
core: `archive_store::ArchiveSettings` reads Python's
config keys (`podcast_archive_enabled`,
`podcast_archive_max_snapshots` clamped 1..=5000,
default 5000, `podcast_archive_rescan_days` default 30,
a stored 0 meaning the default) with Python's interval
formulas (local `max(1, days)` days; hosted at least one
day), and the claim due rules (fresh success skips,
error backoff `min(max(1h, rescan), 1d)`, scanning
backoff 6h) drive `due_archive_feeds` plus the serial
PodcastArchiveWorker queue (`queue_local_scan` /
`queue_hosted_scans`, in-flight dedup per
store+scope+feed, process-wide scan lock). The queue
entry points are core API; calling them from the
refresh paths is app wiring that lands with the main.rs
integration, recorded here rather than silently
dropped. (2) Progress + cancel: `ScanProgress` now
carries `feed_urls` and `episodes_found`, every fetch
goes through Python's retry policy (3 attempts, 1s/2s
backoff, transient 429/500/502/503/504 and network
errors, cancel-aware sleeps), and the local and hosted
orchestrations (`perform_local_scan` /
`run_hosted_scan`) claim, scan, store and finish/reset
state like Python's LocalProvider and
HostedPodcastArchiveMixin flows — hosted failure
preserves earlier sidecar rows, a no-media hosted
document clears them and finishes complete, cancel
resets the claim. (3) Sidecar state: scope strings are
byte-identical to Python's (generic
`<name>:<sha256(name|identity)[:20]>` with the
per-provider identities, Miniflux's own
base-url|api-key scope) and derived from config by
`scope_for_config`; state and entries live in the
active account's own database file under its scope
(Batch 39 file isolation), never in the local store or
another account's file, verified by a two-file /
two-scope regression test. Two store bugs fixed on the
way: `entries()` ordering is Python's `date DESC,
title COLLATE NOCASE`, and `replace_entries` keeps
Python's non-empty-media rule. Episode identity
matches Python exactly: the hosted episode key (sha256
of the sorted identity keys, with Python's fallback
seed), the local stable id, GUID canonicalization only
for URL-shaped GUIDs, the title-date / title-duration
rule (the duration branch is implemented and
unit-tested but inert until the feed parser retains
itunes:duration — `ParsedEntry` carries no duration),
dropping identity-less episodes, and blanking feed-rs's
invented UUID id/URL so guid-less episodes key only by
their real media/title keys, stably across parses
(feedparser leaves both empty). (4) The archive
manager dialog is at Python parity: a feed choice over
all subscribed feeds, the episode list merged with the
account sidecar for hosted providers, a filter over
title/author/date, the four archive-state status
wordings with playable and snapshot counts, a
Downloaded / Available online media column, Play,
Download selected / Download all with Python's
`downloads_enabled` gate, confirmation, per-episode
progress, cancel and summary, in-dialog Rescan with
live progress text and cancel, Find or add podcast,
and Python's close-while-busy veto. Recorded
deviations: the dialog's Play opens the episode window
(in-player playback needs a main-window player
callback; main.rs was outside this batch, so the
dialog returns None and its own workers do the work —
the main window's one-shot `rescan_archive` path is
unchanged but no longer reached from this dialog), and
`rescan_archive(&Db)` still stores without touching
the state store because `Db` exposes no path — the
state-tracked entry points take the store path.
Regression: archive_parity (settings/scope/due-rule
vectors, episode-key and stable-id vectors, identity
rules incl. the invented-id blanking, retry policy,
current-document progress, local + hosted
orchestration incl. cross-account isolation, serial
job queue).

Integration addendum (gap 6, at the v0.1.53 landing): the
app wiring the batch deferred is in. Local: when a refresh
finishes (AllDone, every feed committed), a worker thread
queues queue_local_scan for each feed — Python claims and
queues only after the live-feed transaction commits, and
the claim enforces the due rules, so not-due feeds cost one
state read. Hosted: the provider sync thread queues
queue_hosted_scans for the account's feeds right after a
successful sync, with podcast_source_url applied to the
pairs (its first caller). One recorded deviation:
queue_local_scan's document parameter is now Option — the
app's parsed refresh result carries no raw bytes, so it
passes None and the scan fetches the current document
itself; Python reuses the refresh's downloaded document.
The due-claim rules bound the extra fetch to the rescan
interval (30 days local default), and plumbing raw bytes
through ParsedFeed would have touched every feed-source
construction site for that saving alone.

## 9. Audit 8 — current Python v2.5.8 comparison (2026-11-03)

The [current audit](AUDIT-8-2026-11-03.md) compares Python `2a7eeb3` with
Rust `1df3af2` (v0.1.53). It records 160 outstanding source differences:
54 P1, 93 P2 and 13 P3. The [implementation plan](superpowers/plans/2026-11-03-parity.md)
maps every finding to 31 independently reviewable tasks. This was an audit and
documentation batch; no finding was implemented or verified by a runtime test.

Important corrections to earlier closure claims:

- Source listing/history/fallback helpers exist, but normal application refresh
  and some subscription callers bypass them (F01). Feed storage also runs from
  the GUI drain and may perform media network work (D22).
- Hosted account files and mutation endpoints are implemented, but token refresh,
  remote removal reconciliation and category inventory are incomplete (D07–D11).
- Archive scheduling is connected, but ordinary hosted feed reads omit recovered
  episodes, Play opens text, and busy-close/duration matching remain open
  (F02, F04, F15, F16).
- General range caching exists; first-refusal recovery, request bounds and
  original/resolved caller context remain open (M03, M04, M20).
- SponsorBlock still sends a bare video ID despite the guide's prefix-only
  contract (M02). The new audit supersedes any claim that privacy parity is done.
- Category paths, tombstone state/identity, retention, config writes, enrichment
  tag unions and restore/update swaps have compatibility or data-safety gaps
  (D01–D06, D14–D15, S02, R01, O01–O05).
- Browser/CAPTCHA helpers have no production retrieval callers, and automatic
  article translation has no automatic caller (B01–B02, O07–O08).
- Rich/plain publisher and thread depth remains incomplete despite broad reader
  closure language (R02–R14). Recent Python quiz, recommendation, hidden-player,
  feed-cache and range-probe fixes were explicitly compared.

Conversely, old no-libVLC-bundling and non-Chromecast-routing residuals are
stale: release packaging now bundles VLC, Linux CI contains its smoke command,
and all discovered receiver kinds route through the unified controls. This
audit read those implementations; it did not run workflows or verify assets.
The Accessible Browser remains intentionally removed, and explicit Load More
remains the chosen accessible list behavior. Filtered/sorted paging is still
missing (G03), rather than being covered by default-view paging.

Before closing an Audit 8 ID, add its failing reproduction, fix, application
caller coverage and actual check results here. Screen-reader/platform/hardware
acceptance remains separately pending where required. Historical resolutions
above are preserved as history and must not override this outstanding register.

### Audit 8 Task 1 resolution — atomic configuration and shared location/view identity (S02, S11, G20)

Reproduction first (recorded before the fix): a regression asserting that a
blocked sibling temp write fails the save failed against the old direct
`fs::write` in `Config::save` (the save succeeded and overwrote the live
file), and the new S11/G20 core APIs did not exist (compile errors for
`choose_config_path`, `Config::change_data_location`,
`decode_remembered_view` / `encode_remembered_view`).

- **S02 — closed.** `Config::save` now goes through core
  `config::atomic_write`, mirroring Python `core/config.py::save_config`:
  the serialized config is written, flushed and fsynced into a sibling
  `<path>.tmp`, then renamed over the live file; any failure removes the
  temp and returns an error, leaving the complete old config in place.
  Unknown fields survive saves (the raw object is still preserved), and
  Python's `json.load` reopens a Rust-saved file (verified by hand on the
  regression fixtures). Every existing `Config::save` caller — settings,
  tree selection, shortcuts, provider writes — inherits the behaviour.
- **S11 — closed.** Core gained `choose_config_path` /
  `resolve_config_path` implementing Python's `_resolve_config_path`
  (installed Windows builds always user data; both locations present →
  the newer wins, ties to user data; one present → that one; neither →
  platform default: user data for installed Windows builds and macOS,
  app folder otherwise) and `Config::change_data_location` implementing
  `change_data_location` (sets `data_location`, atomically writes at the
  new location, removes the old file, leaves everything untouched and
  returns an error when the write fails, and refuses `app_folder` on
  installed Windows builds). The app resolves its data directory through
  `resolve_config_path` (`app/src/main.rs::data_dir`, with the
  `.windows-installed` marker check and Python's seed-if-missing copy
  for installed builds via `copy_file_if_missing`); the database keeps
  its identity through the existing copy-across in `base_db_path` on the
  next startup, as Python does. Settings' Advanced page now carries
  Python's Storage Location choice plus both candidate paths (appended
  inside the existing Data Storage Location group, page/control order
  otherwise unchanged); saving a different choice relocates first with
  Python's moved/warning dialogs, and a failed relocation still saves
  the remaining settings to the old file. The in-config `data_location`
  key is written by relocation; resolution itself is path/mtime based,
  exactly like Python's resolver, so no startup rewrite was added.
- **G20 — closed.** Core `decode_remembered_view` /
  `encode_remembered_view` implement the shared `last_selected_feed`
  format from `gui/mainframe.py`: optional `unread:`/`read:` prefix,
  `category:<path>`, `favorites:all`, `deleted:all`, `smart:<id>`,
  `all`, or a feed id; the encoder never wraps smart folders or the
  Deleted view. Legacy Rust values (`cat:<path>`, bare ids) decode too.
  App routing: startup restore (`restored_view_state`) now restores the
  category view AND the read filter from a Python-written value instead
  of treating it as a feed id and resetting the filter, and tree
  selection saves through `remembered_view_value` so Rust-written
  state is Python-readable. Python's Favorites and Deleted have no
  node in the Rust tree (Deleted is a dialog), so those two restore as
  the All view; the stored value is not rewritten by restoring, only
  by an actual selection change — recorded here as the one behavioural
  difference inside this ID.

Evidence: `crates/core/tests/config_compat_slice.rs` (10 tests: blocked
temp write and failed replace keep the old config, successful save
leaves no temp and preserves unknown fields, both timestamp orders and
the tie/single/neither/installed cases for location choice, relocation
round trip preserving fields, relocation failure keeping the old file
and key, installed-Windows restriction, seed-copy never overwriting,
Python view-ID decode and full encode round trips) plus app unit tests
`python_remembered_views_restore_through_app_routing` in
`app/src/main.rs`. Gate for this batch: `cargo fmt --all`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, `cargo build -p blindrss` (results in the
batch commit message / PR notes). A manual restart after a successful
location change (plan Task 1) remains owner-side acceptance, as does
the NVDA pass over the Settings Advanced page.
### Audit 8 Task 2 resolution — D01, D02, D11 (2026-11-03)

Reproduction first: a probe test against the pre-fix code showed
`ensure_category_path("Podcasts / Others")` storing `Podcasts/Others`
(bare-slash split), so Python-created children were invisible to the
Rust subtree queries; `Db::move_category` and the provider sentinel
capability did not exist at all.

- D01: category identity now uses Python's canonical separator
  everywhere. `crates/core/src/db.rs` exports `CATEGORY_PATH_SEP`
  (" / "), `make_category_path`, `category_display_leaf`,
  `category_parent_path` and `sanitize_category_leaf` (ports of
  `core/db.py`); creation (`ensure_category_path`), membership
  (`category_membership`), subtree feed collection, tree deletion,
  OPML import/export nesting, the app tree build, the New Category
  join and the dialogs path helpers all split/join on it. A bare "/"
  inside a leaf (e.g. "AC/DC Fans") is an ordinary character, as in
  Python. Duplicate leaf names under different parents stay
  distinct; no stored duplicates were collapsed.
- D02: rename/move/delete are transactional (`BEGIN IMMEDIATE` …
  COMMIT with rollback on error) and rewrite every reference, per
  Python `rewrite_category_references` plus
  `_update_category_delete_behavior`: category rows, feed
  assignments, `articles.category_override`, `article_labels`,
  per-feed `feeds.delete_behavior` targets, filter-rule
  move/label action targets in `actions_json`, and the global
  config `delete_behavior` target (remapped by the app through
  `remap_delete_behavior` with the mutation's path mapping).
  `Db::rename_category` now takes the new LEAF (the node keeps its
  parent; collision with an existing path refuses with nothing
  changed), matching Python's provider signature; the app's rename
  prompt edits the leaf, and Category Properties applies Python's
  move-first-then-rename order through the new
  `Db::move_category` (which also maintains `parent_id`, refuses
  sentinel/self/descendant moves and destination collisions).
  `Db::delete_category_path` follows Python exactly: direct
  children reparent to the deleted node's parent with shortened
  paths, feeds directly in the deleted category fall back to
  Uncategorized, and on a reparent collision Python's fallback
  drops the subtree with its feeds going to Uncategorized.
- D11: `Provider::uncategorized_is_real_category` (default false,
  true only for Miniflux) plus the config-level
  `provider::uncategorized_is_real_category` helper. The four app
  guard sites (rename, edit/properties, delete, delete-with-feeds)
  consult `is_protected_uncategorized`, so the local sentinel
  stays protected while Miniflux's real category can be renamed
  and deleted; hosted rename/delete mirroring already routes
  through the Miniflux/GReader remote calls.

Evidence: new `crates/core/tests/audit8_categories.rs` (7 tests:
spaced-path creation/parent links, Python-child membership and
tree queries, rename rewriting overrides/labels/filter targets/
per-feed delete targets with the sibling leaf untouched, refused
rename leaving every reference intact, move collision/self-move
refusals plus a successful reparenting move, delete reparenting
with reference rewrite to Uncategorized, provider capability) —
all pass. Existing suites updated to Python-shaped spaced paths
and Python delete semantics: batch1, batch25, batch27, batch32,
audit7, manage, ops, hosted_gui (plus a capability test there).
Owner screen-reader acceptance of the category dialogs remains
pending as manual acceptance, per the plan.
## Audit 8 Task 3 resolution — ingestion identity, metadata and retention (D03, D04, D05, D06, D15)

Reproduction first: the new offline suite `crates/core/tests/audit8_task3_slice.rs`
failed behaviorally on the pre-fix code in five of its six tests — the second
feed's copy of GUID `1` stored 0 rows, a changed GUID revived a deleted URL,
a publisher URL correction was ignored, old unread non-favorites survived
prune, and a zero-day prune deleted the tombstone itself. The sixth test
(retention insert boundary) is written against the new
`store_parsed_with_retention` API and passes with the fix.

- D03 (feed-scoped identity): `refresh::store_parsed_with_retention` resolves
  identity the way Python's generic store does — an entry whose raw id is
  owned by a different feed is stored under `{feed_id}:{base_id}` with
  independent read/favorite state, media, deletion and change history.
  `Db::insert_article_if_new` no longer records a version against another
  feed's row. The pre-existing YouTube `shape_stable_ids` prefixing is
  unchanged and composes with this (it shapes listing items before store).
- D15 (tombstone identity): new `Db::deleted_tombstones_for_feed` ports
  Python's per-feed (ids, urls) pair; refresh skips an entry when its raw id,
  its scoped id, or its URL matches that feed's tombstones. Other feeds are
  unaffected. Hosted-provider sync paths still consult the global
  `deleted_article_ids` set — they are provider-shaped stores, outside this
  task's cited sites, and are not changed here.
- D06 (metadata sync): existing rows for the refreshing feed (raw or scoped
  id) now go through `Db::sync_existing_article_metadata`, porting Python's
  update: date, description, media URL/type and article URL (with Python's
  stale-enclosure URL healing) are updated when changed; read/favorite state,
  the stored body, tags and labels are never touched, and a changed feed body
  is appended to change history only. One recorded sub-deviation: chapter URL
  is synced by the Db function when a caller supplies one, but the Rust
  ingestion pipeline has no chapter source today (`ParsedEntry` carries no
  chapter URL and no Rust code writes `articles.chapter_url` at ingest), so
  refresh passes None and the stored value is preserved rather than cleared.
- D04 (one retention boundary): `retention::retention_cutoff_date` and
  `retention::article_is_outside_retention` port Python's shared boundary
  (including the undated floor `"0002"`). `store_parsed_with_retention`
  refuses NEW entries outside the window, and the app's refresh drain and
  Add Feed paths pass the cutoff derived from the `article_retention`
  config (`app::article_retention_cutoff`). `store_parsed` keeps the old
  no-boundary behavior for its remaining callers (archive recovery, dialogs,
  provider add-feed). Prune no longer ages out tombstones (see D05), so the
  resurrection guard cannot expire. Core `refresh_feed`/`refresh_all` do not
  receive config in this port's wiring and keep no-boundary storage; the
  production refresh path is the app drain, which is wired.
- D05 (cleanup policy): `Db::prune_old_articles` now mirrors Python
  `cleanup_old_articles`: old read AND unread non-favorites are deleted,
  favorites and undated rows (empty date or the `0001-01-01` sentinel, via
  the `date >= '0002'` guard) are protected, and chapters plus
  `article:<id>` playback_state rows are deleted with their articles.
  Tombstones are not deleted at all. The pre-existing
  `maintenance_slice::prune_keeps_unread_and_favorites` test encoded the old
  divergent policy and was updated to the Python policy (renamed
  `prune_removes_old_unread_keeps_favorites_and_tombstones`).

Verification: `audit8_task3_slice` 6/6, `refresh_slice` 2/2,
`maintenance_slice` 3/3, `articles_slice` 2/2, `parity_core_audit` 6/6 and
the retention unit tests pass; the full gate in this batch's worktree ran
`cargo fmt --all` (check clean), `cargo clippy` at zero warnings for core,
app and xtask, `cargo test --workspace` 450 passed / 0 failed, and
`cargo build -p blindrss` successfully. Category path handling is untouched: this batch builds on
current main's path code and assumes Task 2's canonical `" / "` separator
work lands independently (no category code is modified here).
## Audit 8 Task 4 — delete/restore and complete rule application (D12, D13, D14, D21, G07)

Branch `audit8/task4-delete-rules`, built on main at `d4d69f6`. Python spec:
`core/filters.py` (`parse_delete_behavior`, `resolve_effective_actions`,
`apply_effective_actions`), `core/db.py` (tombstone snapshot/restore/purge),
`providers/local.py` (`_resolve_delete_behavior`, `_apply_rules_to_new_article`,
`apply_filter_rules_to_existing`, `delete_article`), `gui/mainframe.py`
(Deleted Articles view and restore/purge flows).

Failing reproductions first: new core suite `crates/core/tests/audit8_task4_slice.rs`
(12 tests) was written against the pre-fix tree and failed to compile with
exactly the expected missing-API errors (no delete-behavior resolution, no
scoped restore/purge, no paged rule matching, `store_parsed` returning a bare
insertion count). After the fix all 12 pass:

- D14 — `tombstone_preserves_read_and_favorite_through_restore` and
  `python_origin_tombstone_restores_its_flags`: `deleted_articles` snapshots
  now include `is_read`/`is_favorite` (fresh-schema columns plus two migration
  entries; the migration loop remains the only ALTER site). `Db::delete_article`
  copies both flags from the row (and now also clears `article_labels` and
  `chapters` rows, as Python's `_tombstone_and_remove` does). Restore returns
  the stored flags, with NULL (identity-only Python tombstones) restoring as
  unread/non-favorite. The old doc claim that Python restores literal zeroes
  was wrong and is corrected.
- D14/G07 core — `restore_refuses_purged_and_ambiguous_tombstones` and
  `purge_deleted_article_nulls_snapshot_but_keeps_tombstone`: new
  `Db::restore_deleted_article(feed_id, article_id)` restores only that feed's
  non-purged tombstone with `INSERT OR REPLACE`; unscoped `restore_article`
  now requires exactly one non-purged tombstone for the id and leaves rows
  untouched otherwise. New `Db::purge_deleted_article(feed_id, article_id)`
  mirrors Python: snapshot columns nulled, `purged=1`, identity kept so
  refresh never recreates the article; a second purge returns false.
  `deleted_articles()` returns the real flags and orders by
  `deleted_at DESC, article_id DESC` (Python order).
- D12 — `parse_and_resolve_delete_behavior_matches_python`,
  `filter_delete_with_purge_behavior_does_not_soft_delete`,
  `filter_delete_with_category_behavior_moves_instead_of_deleting`,
  `filter_delete_feed_override_beats_global`, and
  `apply_rules_to_existing_honors_delete_behavior`: `rules.rs` now ports
  `parse_delete_behavior` (`DeleteBehavior::{Deleted, Purge, Category}`) and
  `resolve_effective_actions` (`ResolvedActions`), including Python's rule
  that delete-to-category beats an explicit move. One shared path,
  `Db::delete_article_with_behavior` + `Db::effective_delete_behavior`
  (per-feed override, else the configured global), serves the rule pipeline
  for new and existing articles and the interactive Delete command, which
  previously matched the behavior string ad hoc and case-sensitively.
- D13 — `store_outcome_excludes_deleted_and_notification_suppressed`:
  `refresh::store_parsed` now returns `StoreOutcome { inserted, notify_count,
  new_articles }`; rule-removed and `skip_notification` articles are excluded
  from the notification-eligible count and summaries, matching Python's
  `new_items` accounting. `refresh_feed`/`refresh_all` return the
  notification-eligible counts. Archive rescans deliberately keep counting
  stored episodes (`inserted`) for their recovered-episode semantics.
- D21 — `apply_rules_to_existing_reaches_beyond_100k_rows` and
  `smart_folder_matching_reaches_beyond_100k_rows`: the 100,000-row ceiling
  is gone. `apply_rules_to_existing` collects matches over 1,000-row pages
  (`Db::articles_page`) before applying, exactly as Python snapshots all rows
  first, so deletions cannot shift later pages. New
  `Db::articles_matching_rule` pages the same way and is the production path
  for the main window's smart-folder refill, replacing the inline
  100,000-row load. Tests build a 100,001-row library whose only match is
  the oldest row.
- G07 — the Deleted Articles dialog is rebuilt on the standard article
  ListCtrl (same columns/cells as the main list) with term search across
  title/feed/author/URL, Open (double-click/Enter), multi-select Restore,
  and multi-select Delete Permanently behind Python's confirmation prompt
  ("Permanently delete this article? It cannot be restored." / plural).
  Rows leave the list only for items the core call actually restored or
  purged (`retain_failed_deleted_rows`); partial failures are reported and
  the failed rows stay, replacing the old title-only single-restore dialog
  that removed its row even when restore failed. App policy test
  `deleted_rows_leave_only_on_success` covers the retention rule and the
  display-title fallback (title, then URL, then "(deleted article)").

Application caller coverage: refresh drain FeedDone and Add Feed paths pass
the configured `delete_behavior`; `LocalProvider` carries it from config;
the podcast archive queue/manual paths carry it via `ArchiveSettings`;
the rules manager's Apply button passes it to `apply_rules_to_existing`;
interactive Delete resolves through the shared core path; smart-folder
refill calls `articles_matching_rule`.

Check results: new suite 12/12; affected existing core suites 49/49
(rules, articles, refresh, batch4, batch5, parity_core_audit, ops,
archive_parity, manage, hosted_gui); final gate after `cargo fmt --all`:
workspace clippy (`--all-targets -D warnings`) clean, `cargo test
--workspace` 456 passed / 0 failed, `cargo build -p blindrss` OK. The app
gate ran in a private copy of the shared target dir because parallel
worktrees sharing one target dir reuse each other's `blindrss-core`
artifacts (identical package metadata), which produced stale-API compile
errors against this branch's new core signatures; the private copy
eliminated the collision.

Open, honestly: D15 (feed-scoped tombstone URL identity) is Task 3's scope
and is not closed here. The Deleted Articles surface is a full dialog, not
Python's `deleted:all` tree node in the main window. Screen-reader
acceptance of the rebuilt dialog (NVDA pass over columns, search,
multi-restore, confirmed purge, failure messages) remains pending the
owner's hands, as does acceptance with real hosted accounts.
## Audit 8 Task 5 resolution — safe backup and restore lifecycle (O01, O02, O09, O10, O11, O12)

Closed 2026-11-03 on branch `audit8/task5-backup`, built on current main
(`d4d69f6`). Assumption: Task 1 (atomic config) runs in parallel on another
branch; this batch does not change config save/load, only moves and validates
`config.json` as a file, and keeps the existing
`apply_pending_restore(data_dir, db_path)` signature, so integration only
needs to re-check the shared call sites in `app/src/main.rs`.

- **O01** — `apply_pending_restore` removes the old database's `-wal`/`-shm`
  sidecars at the stopped-DB boundary (after the old DB is moved aside,
  before the restored file lands; nothing else in the codebase touches
  sidecars). Regression `a8_restore_removes_stale_wal_shm`: stale WAL/SHM
  beside the target are gone after apply and the restored rows read back
  correctly.
- **O02** — Restore now validates the full staged contents before any swap:
  staging unpacks into a unique `restore-*` directory inside the data dir,
  validates the staged DB with `PRAGMA integrity_check` (not just the
  header) and the staged config as a JSON object, and only then swaps the
  staging directory into `restore_pending/` (the old pending directory is
  renamed aside first and restored if the swap fails). Config-only
  archives stage and apply without touching the database; omitted
  optional files are preserved. The direct `restore_backup` helper got
  the same validate-everything-before-writing-anything treatment with
  per-file atomic replacement. Regressions:
  `a8_stage_rejects_header_valid_corrupt_db`, `a8_stage_rejects_bad_config`,
  `a8_config_only_backup_stages_and_applies`,
  `a8_failed_second_stage_preserves_first_stage`, `a8_foreign_zip_rejected`
  (keeps Python's "This file is not a BlindRSS backup." message),
  `a8_restore_backup_direct_validates_before_writing`.
- **O09** — Backups include Python's four optional files
  (`site_cookies.txt`, `site_cookies_ua.txt`, `site_cookies_ua_hosts.json`,
  `youtube_cookies.txt`) when present, and staged restores apply them into
  the data directory. `create_backup` gained the `data_dir` parameter
  Python's signature has. Regression
  `a8_optional_files_round_trip_and_omission_preserves`.
- **O10** — Backup writes are atomic: DB snapshot and ZIP partial use
  process-unique names (pid + nanoseconds + atomic counter), the ZIP is
  renamed over the destination only when complete, and a failure leaves
  the previous archive and no partial behind. A backup also no longer
  fabricates a `{}` config or an empty database when the source file is
  absent (Python includes each member only when it exists). Regressions:
  `a8_failed_backup_preserves_existing_archive`,
  `a8_concurrent_backups_do_not_collide`.
- **O11** — The manual Back Up Data action now runs `create_backup` on a
  worker thread and reports through the existing `RefreshMsg` drain
  (`RefreshMsg::BackupDone`); completion shows Python's message including
  "It contains your passwords, API keys and site cookies, so keep it
  private.", and failure shows Python's "Backup failed: …" error dialog.
  App-caller wiring is compile-verified; the dialog itself is GUI surface
  and owner screen-reader acceptance remains pending like every GUI item.
- **O12** — New core `run_auto_backup` + `auto_backup_due` port Python's
  rotation: names are `BlindRSS-auto-backup-YYYY-MM-DD-HHMMSS.zip`
  (fixed-width UTC stamp; the format is what lexicographic rotation
  sorts on and it interleaves correctly with Python's local-time names),
  and rotation deletes only prefix + `.zip` matches beyond the keep
  count. The app's startup maintenance now calls it instead of its
  epoch-second names and its prefix-only scan, which could delete every
  new Rust backup in a folder holding dated Python backups. Regression
  `a8_auto_backup_rotation_mixed_names` (dated Python names, an old
  epoch Rust name, a manual backup and a same-prefix non-ZIP file all
  survive correctly).

Checks (this batch): `cargo test -p blindrss-core --test
maintenance_slice --test ops_slice` — 14 + 2 passed, including the 11 new
Audit 8 tests; full workspace clippy/test/build gate results are recorded
in the batch commit message.
### Audit 8 Task 6 resolution — safe, usable updater (O03, O04, O05, O17)

Owner correction during implementation (Brandon, 2026-11-03: "I am to use
shipfitter for the updater. We've gone over this."): the hand-rolled staging
updater shipped in v0.1.51 was the wrong shape. Like BlindRadio
(`crates/blindradio/src/update.rs`) and the Rust IPTV client (its PR #125
deleted its hand-rolled updater the same way), the runtime updater is now
ship-shape 0.5.0; the hand-rolled code is deleted, not patched. Release-side
signing (cargo xtask sign, release.yml, MINISIGN_KEY secret) is unchanged.

What landed:

- `app/src/update.rs` (new): `config()` builds one
  `ship_shape::UpdaterConfig` — repo `serrebidev/BlindRSS-Rust`, app name
  `BlindRSS`, current version from `app/Cargo.toml`, `InstallKind::Portable`,
  and the minisign public key read from the committed
  `crates/core/assets/minisign.pub` (BlindRadio keypair, key ID
  151E848B8939A63F). `check(parent, trigger)` calls
  `ship_shape::ui::run_update_check` on the Stable channel.
- Callers keep their labels and placement: the Tools menu / Settings
  "Check for Updates" action (`tools.check_updates`) runs a Manual check
  (ship-shape reports up-to-date and errors, and offers Download/Install
  with release notes on Yes), and the launch timer still gated by
  `auto_check_updates` (default on) runs an Automatic check (only an
  available update surfaces). After the frame shows,
  `bring_to_front_after_update` runs so a relaunched copy comes forward.
- Deleted: `maintenance::check_update`, `parse_latest_release`,
  `latest_release_assets`/`LatestReleaseAssets`, `download_and_stage_update`,
  `verify_update_signature` and its unit tests (verification now lives in
  ship-shape), core's `minisign-verify` dependency, the app's
  `RefreshMsg::UpdateChecked`/`UpdateStaged` flow, and
  `apply_pending_update` with its `update_pending/` staging directory.
  The `install_updates_automatically` config key is retained on disk for
  compatibility; ship-shape's flow always confirms before downloading, so
  it no longer triggers a silent install (same behaviour as BlindRadio).

Finding dispositions (all four closed by replacement; the findings targeted
the deleted hand-rolled path):

- O03 (default manual update had no installation action): the Manual
  trigger drives ship-shape's full dialog flow — release notes, confirm,
  progress, verified install, restart — under default settings.
- O04 (inequality-only comparison could offer/install a downgrade):
  ship-shape's `check_for_updates` parses and orders versions and only
  reports an update when the release is newer; the Rust inequality check
  is deleted with the rest of the hand-rolled path.
- O05 (supporting-file copy failures discarded; staging deleted under a
  still-running old process): install is ship-shape's platform code — the
  download lands as a temp file, is renamed into place only after
  verification, and the swap/restart runs through its detached platform
  helper after the app exits; a failed verification or download removes
  the temp file and leaves the running install untouched.
- O17 (whole-asset buffering, no cancellation/retry/single-operation
  guard): ship-shape streams the download in chunks with a progress
  dialog and a cancellation flag polled per chunk, and its `ActiveCheck`
  guard makes concurrent check/install calls a no-op.

Evidence: `app/src/update.rs` tests — the config names the release repo,
asset base name, current version and Portable kind, and embeds exactly the
committed public key; two loopback tests drive
`ship_shape::download_update_file` through that production config with
(1) the real release key's signature over different bytes (same key ID, so
the Ed25519 check itself runs) and (2) a garbage signature — both must
error with `UpdateError::Verification` and install nothing.

Open / not covered here:

- Asset-name contract — RESOLVED by owner decision 2026-11-03 ("switch
  fully to fixed ship-shape names like BlindRadio", with direct OK for
  the workflow edit): `release.yml` now publishes exactly the names
  ship-shape looks for — `BlindRSS.zip` (Windows Portable),
  `BlindRSS.tar.gz` (Linux Portable) and `BlindRSS.dmg` (macOS), each
  with a `.minisig` — replacing the versioned
  `BlindRSS-vX.Y.Z-{Windows,macOS,Linux}.zip` names entirely. Archive
  contents sit at the root (no wrapping folder), matching ship-shape's
  Portable apply, which extracts over the app folder without stripping
  a top folder. The publish leg verifies all six package/signature
  files arrived; a release still carries 7 assets (3 packages, 3
  signatures, SHA256SUMS). First live exercise is the next release
  after this batch lands.
- Manual packaged-build upgrades on all three OSes and an NVDA pass over
  ship-shape's own dialogs remain owner acceptance, as the plan requires.
## Audit 8 Task 7 resolution — M02 SponsorBlock privacy and action semantics (2026-11-03)

M02 is closed. The pre-fix Rust client sent the bare video ID to the
wrong host (`sponsor.ajayyy.com/api/skipSegments?videoID=...`), accepted
non-"skip" and sub-0.5 s segments, carried no segment UUIDs, hard-coded
a `BlindRSS-Rust/0.1` User-Agent, and the player remembered skipped
segments by start time. A failing probe against the old parser was
recorded first (a 0.2 s "skip" segment survived parsing; Python keeps
only the one real segment).

`crates/core/src/sponsorblock.rs` is now a faithful port of Python
`core/sponsorblock.py`: lookups go to
`https://sponsor.ajay.app/api/skipSegments/{prefix}` where the prefix
is the first four hex digits of SHA-256(video id), with `categories`
and `actionTypes=["skip"]` as JSON query params; an empty video id or
empty category list issues NO request. `parse_segments` performs
collision matching (only the response entry whose `videoID` equals
the requested id is read), defaults a missing `actionType` to "skip"
and drops any other action, drops segments shorter than 0.5 s, and
preserves each segment's `UUID`. `skip_categories` ports Python's
switch/order/fallback semantics over the CATEGORIES table (stored
non-list falls back to DEFAULT_SKIP; result in CATEGORIES order), and
`segment_at` ports the done-by-UUID lookup with the last-half-second
tail exclusion. Every lookup failure (404, 5xx, connection refused,
unreadable body) returns no segments, so playback carries on. The
User-Agent now comes from core's central resolution
(`crate::http::user_agent()`), never a call-site string.

In `app/src/player.rs`, `spawn_skip_workers` resolves categories
through core `skip_categories` (no worker and no request when empty),
`fetch_segments` no longer returns a Result the caller could drop,
skip announcements use Python's category label and duration text, and
the tick remembers skipped SponsorBlock segments by UUID with
Python's 500 ms tail (silence ranges keep their existing start-time
keying and 250 ms tail; silence semantics are Task 21's scope).

Evidence: `crates/core/tests/sponsorblock_parity.rs` (7 tests) ports
Python's `tests/test_sponsorblock.py` cases and adds a loopback
fixture server proving the recorded request line contains only the
hash prefix (never the full video ID), carries the central
User-Agent, that empty categories produce zero fixture hits, and that
500/404/refused lookups all return empty. `batch3_slice.rs` was
updated to the prefix-response shape. Owner NVDA acceptance of skip
announcements remains pending as platform acceptance, as with all
player work.

### Audit 8 Task 23 resolution — current, validated dependencies and owned subprocesses (O06, O13, O14, O15, F20)

Reproduction first: the new `crates/core/tests/dependency_parity.rs`
suite (16 tests) was written against Python
`core/dependency_check.py`, `core/discovery.py` and
`core/runtime_self_test.py` before the fix; it failed to compile
against the old core (none of the policy APIs existed), and its
behavioral premises were verified against the old code by reading:
`dependency.rs` probed every tool with `--version` and no timeout
(Python: `ffmpeg -version`, 6 s), `update_ytdlp` fetched the STABLE
channel and the app only ever installed missing tools, resolution
stopped at configured/env/managed/PATH, and the `videosearch.rs` /
`sources.rs` yt-dlp runners abandoned timed-out children on
detached threads.

O13 — one validation policy in `dependency.rs`: `run_tool` is the
owned-subprocess runner (stdin closed, stdout/stderr drained on
reader threads and capped at `MAX_TOOL_OUTPUT_BYTES` = 1 MiB per
stream, child killed AND reaped on timeout). `version_arg_for_tool`
ports Python `_version_arg_for_tool` (ffmpeg/ffprobe `-version`,
everything else `--version`); `validate_executable` (6 s) and
`probe_version` (10 s, one retry when the runner itself fails —
never when the tool ran and failed) port `_validate_executable` /
`ytdlp_version`. `check_tools` now counts a present-but-invalid
tool as missing, so the app's existing missing-tool flow repairs a
broken install instead of trusting it (Python
`detect_media_tool_paths(validate=True)` semantics). The internal
`version_of` used by the ffmpeg install check rides the same probe.

O14 — one resolution policy: `dependency::find_tool` (user
override, `BLINDRSS_*` env pointer, managed copy, PATH, the
directory beside the executable and its `bin/`, then
`known_package_dirs`). The package-location set ports Python
`_collect_tool_candidates`' bounded lists: POSIX system/Homebrew/
MacPorts/snap/per-user dirs, and on Windows the Scoop (incl.
ffmpeg-essentials/-shared/-gyan-nightly), WinGet Links,
Chocolatey, Gyan/FFmpeg, Program Files and MSYS2 locations.
`media::find_ytdlp`, `silence::find_ffmpeg` and
`dependency::find_ffprobe` are now thin wrappers over it; the old
pure `resolve_ytdlp` helper and its unit test were replaced by
precedence coverage in the new suite (user override beats the
managed copy). Recorded deviation: Python also expands a handful
of versioned WinGet/Chocolatey glob patterns; the port covers the
stable shim directories (WinGet Links, Scoop `current`, Choco
`bin`) those installs always create, and does not glob.

O06 — nightly yt-dlp maintenance, ported from Python
`_ensure_yt_dlp_cli`/`_update_managed_ytdlp`:
`update_managed_ytdlp` compares the managed copy's version with
the newest yt-dlp-nightly-builds tag, tries `--update-to nightly`
first without trusting its exit code (version re-read), then falls
back to a full download that is verified to run BEFORE it replaces
the working copy — a failed update reports
`KeptExistingAfterFailure` with the old binary byte-identical;
with no working copy a failed download is an error. `update_ytdlp`
(the app's install path) now installs the nightly asset through
the same verified swap; the old 100 KB size heuristic is gone
because verification replaces it. `maintain_ytdlp` adds Python's
24 h throttle (marker `last_ytdlp_update_check.txt` in the data
dir — Python keeps its marker in the temp dir; the data dir is
the durable equivalent), written ONLY after a successful check,
and the app runs it on a background thread at startup: the managed
copy gets full maintenance, a system copy is only offered
`--update-to nightly` and never overwritten. Deviation: Python's
separate embedded-module wheel update has no Rust analogue (there
is no embedded yt-dlp module; the CLI is the only consumer).

O15 — JavaScript runtime + packaged smoke support:
`find_js_runtime` resolves Deno through the same `find_tool`
policy (Python ships Deno in the bundle for yt-dlp's YouTube
signature challenges and finds it via `_find_executable_path`).
`dependency::runtime_smoke_test` is the Rust-appropriate port of
`core/runtime_self_test.py`: yt-dlp, Deno, ffmpeg and ffprobe
must resolve and run, and `smoke_extraction_error` runs Python's
sample-article extraction check (>= 500 chars, identical across
repeated passes). The app exposes it as
`--blindrss-runtime-self-test` (Python's exact flag in main.py),
printing failures and exiting 1 without opening the GUI. Python's
libVLC/updater-helper smoke checks stay with the app/player
layer's own paths and are not duplicated here. Workflow
integration prepared but NOT landed (owner approval required, no
workflow files were edited): release packaging must place a Deno
binary beside the app executable (or in its `bin/`) on all three
platforms — the Windows asset is
`https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip`
(Python `build.bat` uses the same URL), `deno-x86_64-unknown-linux-gnu.zip`
and `deno-aarch64-apple-darwin.zip`/`deno-x86_64-apple-darwin.zip`
for the other legs — and add a packaged smoke step running
`blindrss --blindrss-runtime-self-test` with a minimal PATH before
signing.

F20 — timed-out subprocesses are killed and reaped:
`videosearch.rs`'s search runner and both `sources.rs` yt-dlp
runners (flat listing and info document) now go through
`dependency::run_tool`, as does `media.rs`'s runner (600 s finite
budget, previously unbounded); the detached-thread/recv_timeout
pattern is gone from all three. The new suite proves kill+reap
with a sleeping fake executable that writes its pid (the pid is
fully gone after the timeout returns). Remaining unbounded or
detach-pattern yt-dlp callers in `download.rs`, `play_cache.rs`,
`feedsearch.rs`, `source_feeds.rs` and `ytfull.rs` belong to
Tasks 22, 26 and 16 respectively and are recorded here as their
scope, not silently fixed or claimed by this task.

Evidence: `crates/core/tests/dependency_parity.rs` (16 tests:
version-flag matrix, single-dash ffmpeg fake, broken/hanging
probes, broken-tool-as-missing check_tools + precedence, package
dirs, find_in_dirs, kill/reap pid proof, output cap, throttle
window, four loopback nightly scenarios — current-not-redownloaded
with an asset-hit counter, stale self-update-distrust fallback,
failed-download-keeps-working-copy byte comparison, fresh
install/failure — Deno resolution and the extraction smoke).
Gates on this branch: `cargo fmt --all` clean (check mode),
clippy `--workspace --all-targets -D warnings` zero,
`cargo test --workspace` 515 passed / 0 failed,
`cargo check -p blindrss-core --target x86_64-pc-windows-gnu`
clean (the cfg(windows) package-location table compiles),
`cargo build -p blindrss` OK. One flake note: under heavy
parallel load a version probe of a healthy fake intermittently
returned no result once in several full-suite runs; root-caused
to the runner failing (not the tool) and hardened with the
single runner-error retry in `probe_version` — 4/4 clean
full-suite runs of the new binary followed. Owner platform
acceptance (packaged Deno beside the app, smoke step in the
release workflow) remains pending per the workflow item above.

## Audit 8 Wave 1 integration (2026-11-03)

Wave 1 (Tasks 1-7) integrated onto main in dependency order and
released as v0.1.54. Integration merges worth recording: the store
path now carries both Task 3's retention boundary and Task 4's
StoreOutcome — `store_parsed_with_retention` takes the feed's
effective delete behavior AND the retention cutoff and returns the
outcome, and the app's refresh drain counts notification-eligible
articles (D13) from that call; Task 3's slice tests were re-pointed
at the merged signatures (`.inserted`). In maintenance.rs, Task 5's
backup/restore rewrite and Task 6's updater deletion were merged by
keeping the backup lifecycle and the ship-shape pointer, dropping
the hand-rolled update functions. Task 6's follow-up (owner decision
2026-11-03) switched release packaging to the fixed ship-shape asset
names, so this is the first release whose assets the ship-shape
updater can find.

### Audit 8 Task 8 resolution — unified worker-side refresh and source dispatch (D16, D22, F01)

Reproduction first (recorded before the fix): the regression suite
`crates/core/tests/audit8_task8_slice.rs` was written against APIs
that did not exist (`feeds::feed_body_digest`,
`feeds::fetch_feed_document`, `refresh::refresh_feed_with_options`,
`refresh::subscribe_url`, `discovery::source_listing_url`) and
failed to compile with 7 unresolved-import errors. Behind those
missing APIs, the wired behaviour was: the app's refresh workers
fetched raw documents with `feeds::fetch_with_cache`, and the GUI
drain then ran `store_parsed_with_retention` on the 150 ms timer
thread with the UI connection — including the NPR/Sky media
attachment inside the store, which performs network fetches
(D22). Every refresh reparsed and re-stored the whole feed:
`fetch_with_cache` reparsed the cached body on a 304 and stored
identical 200 bodies again (D16). Subscription callers bypassed
the listing sources entirely: `resolve_subscribe_url` assumed a
native feed document, and even the social transforms that return
a listing page itself (YouTube search, Rumble/Odysee channels)
were fed to `fetch_and_parse`, which fails on those pages (F01).

D16 — unchanged-body fast path, ported from Python
`providers/local.py` (v2.5.5+): `feeds::feed_body_digest` is the
SHA-1 hex of the body with each `<media:statistics…>` /
`<media:starRating…>` opening tag stripped, byte-for-byte
Python's `_feed_body_digest` rule. `feeds::fetch_feed_document`
returns `FeedFetch::NotModified` for a 304 whose cached-body
digest matches the digest recorded for the feed, and for a 200
whose body digest matches; the recorded digest
(`_stored_feed_body_digests` equivalent, keyed by feed id,
in-memory like Python's provider-instance map) is written by
`refresh_feed_with_options` only AFTER `store_parsed_with_retention`
commits, so a failed store is retried by the next refresh even
when the publisher then answers 304. Forced refreshes never take
the fast path. Validators from an unchanged 200 are still
persisted (Python persists the new ETag/Last-Modified so the
next refresh earns a real 304), and the fetch path now parses
before caching, so a body that fails to parse no longer advances
the validators. Where Python's `_store_youtube_items` pops the
digest (listing dates are day-only; the next RSS body must store
again to restore exact times), the port pops it too: the YouTube
search listing branch, the channel RSS-failure fallback, and the
history backfill. The history backfill now also stores through
the caller's retention boundary, as Python computes one cutoff
per `_refresh_single_feed` for all its insert paths.

D22 — one worker-side refresh: `refresh::refresh_feed_with_options`
runs source dispatch, the document fetch, the store and the
NPR/Sky attachment on the caller's thread through the caller's
own `Db` connection, and returns a `WorkerRefresh` carrying only
the committed `StoreOutcome` plus the not-modified flag. The
app's `spawn_refresh` workers call it directly (with the run's
delete behavior and retention cutoff, the existing pool/host-gate/
retry shape unchanged), record or clear the feed error on the
worker connection, and a cancelled refresh records and reports
nothing (the AllDone path announces the stop, as before).
`RefreshMsg::FeedDone` now carries the `StoreOutcome`; the drain
keeps only counting and notification bookkeeping — the
GUI-thread store call is gone. Cancellation is checked before
the fetch and again before the store (`REFRESH_CANCELLED`);
Python checks inside its per-entry loop, the port's store is a
single call, so a cancel landing mid-store takes effect at the
next feed — the same granularity the app already had between
feeds. All subscribe entry points share
`refresh::subscribe_url` on their worker threads (Add Feed,
Feed Search, Video Search subscribe, the archive manager's Find
flow, and `LocalProvider::add_feed`).

F01 — shared source dispatch: `discovery::source_listing_url`
returns the normalized subscription URL for the Rumble, Odysee,
YouTube search, SoundCloud and Mixcloud listing sources (Python
`_resolve_feed_url` normalization), and `resolve_subscribe_url`
routes those URLs — including the social transforms that return
a listing page — through `sources::fetch_listing` instead of
assuming feed XML. A YouTube search subscription resolves with
Python's `YouTube: {query}` title and no fetch at all; its
articles are enumerated by the unified refresh, whose listing
ids are shaped with the real feed id (which is why
`subscribe_url` creates the row first and then refreshes, with
Python's add-flow tolerance: a failed first enumeration leaves
the subscription standing). `discover_feed_urls` returns the
normalized listing URL as the candidate for a source page.
Google Groups / Groups.io dispatch is untouched: those kinds are
explicitly excluded from the new listing path because
`forum_sources` owns them and that dispatch already works.

Evidence: `crates/core/tests/audit8_task8_slice.rs` (11 tests) —
digest counter-invariance; unchanged-200 skip with validator
persistence proven by a third request carrying the new ETag into
a 304; forced refresh bypassing the fast path; a store forced to
fail by an aborting trigger, then retried successfully when the
publisher answers 304; cancellation before the fetch (no request
made) and during the fetch (nothing stored); committed rows
visible to a second connection before the refresh returns; a
loopback NPR story page delayed 1.2 s whose audio is attached by
the time the worker refresh returns (Sky's attach shares the
same store-time call site; its host rules keep it off loopback);
listing-URL normalization; YouTube search resolution without a
feed document; worker-side subscribe for a plain feed and for a
YouTube search row. The pre-existing `refresh_slice.rs`,
`feed_cache_parity.rs`, `source_feeds_parity.rs` and
`source_depth_parity.rs` suites pass unchanged. Gate results are
recorded in the batch commit message. Remaining owner
acceptance: NVDA responsiveness during a Refresh All that
includes slow media feeds (the plan's manual check) — the
loopback test proves the work executes inside the worker call;
the screen-reader verdict stays Brandon's, as with all refresh
work.
### Audit 8 Task 9 resolution — hosted account and authentication lifecycle (D07, D08, D09, D10, D23, S15) (2026-11-03)

All six findings are closed. Reproductions were written first as
failing loopback-fixture tests in `crates/core/tests/audit8_task9_slice.rs`
(19 tests), `crates/core/tests/oauth_slice.rs` (extended) and the
`provider.rs` unit tests; each failed or could not compile against the
pre-fix tree (the refresh helper had no production caller, the clients
sent no AppId/AppKey headers, sync never removed a row, the category
table only ever learned categories from feeds, category Mark All Read
was a per-feed fan-out, and Settings had no Redirect URI field).

D07 — Inoreader token refresh is reachable. `crates/core/src/oauth.rs`
now carries `InoreaderAuth`, a port of Python
`providers/inoreader.py`'s credential lifecycle (`_set_tokens`,
`_token_is_stale`, `_has_required_auth`): staleness is "no token, or
now ≥ stored expiry − 60 s" with no recorded expiry taken at face
value; a successful refresh stores the new access token, a rotated
refresh token when the server sends one, and an expiry of
now + expires_in − 60; an `invalid_grant` response clears all three
(the account then reports "Inoreader is not connected" instead of
looping); any other failure — HTTP 500, connection refused — returns
an error with the stored tokens untouched, and a response without an
access token changes nothing (Python falls through to the old token).
`HostedProvider` builds its Inoreader client through this lifecycle
on every operation and writes rotations back through
`Config::save_to_source` (Config now remembers the file it was loaded
from; a hand-built config skips write-back). A revocation that turns
out to be superseded — the file already carries a different refresh
token, so another flow rotated first — adopts the file's credentials
instead of clearing them. The app's background "sync all providers"
action runs the same lifecycle (and the D08 app credentials) instead
of presenting the stored token raw. Token values are never logged.
Supporting fix: `provider::db_path_for_provider` now keys an
Inoreader account's cache file to its refresh token when one is
stored (Python's `_podcast_archive_account_identity` preference),
because keying it to the access token would orphan the whole cache at
the first rotation once refresh became reachable; sections without a
refresh token keep the previous key, and a stored-token-only account
sees one cache rebuild, the same one-time cost the per-account paths
themselves introduced.

D08 — `GReaderClient::with_app_credentials` carries
`providers.inoreader.app_id` / `app_key`, and every GReader request
construction (reads through `get_json`, the token fetch and POST of
every edit endpoint, rename/disable-tag, mark-all-as-read) passes
through `with_app_headers`, which adds `AppId`/`AppKey` for Inoreader
only, each only when configured — Python `_headers` exactly. The
fixture tests assert the headers on a subscription read and an
edit-tag pair, and their absence for The Old Reader.

D09 — both hosted syncs reconcile, and only on complete success.
`GReaderClient::sync` and `MinifluxClient::sync` previously only
upserted, so a subscription removed on the server stayed in the
cache forever (`provider.rs` then served it from `feeds()`). Now,
after every subscription and every article page has been fetched and
stored without error — any failure returns early, before this point —
feeds in this provider's own id shape (`feed/…`, `miniflux:…`) that
the current inventory no longer carries are removed with their
articles and dependent rows through the new
`Db::remove_hosted_feeds`. Local feeds in the same file are never
eligible. Tests: A+B then A-only removes B (feed and articles) for
both providers; a failed subscription fetch and a mid-sync failed
stream fetch both leave B in place.

D10 — category inventories sync. GReader gained `labels()`
(`/tag/list`, Python `get_categories`), cached process-wide under the
same scope/TTL rules as the Inoreader subscription cache so the small
app quota is not burned per sync; Miniflux reuses
`categories_remote()` (`/v1/categories`). After a successful sync the
new `Db::reconcile_hosted_categories` ensures every inventory title
(and its ancestors) and prunes a stored category the server no longer
carries when nothing local references it (no feed carries it, no
article is labeled or overridden into it); the Uncategorized sentinel
row is never pruned. When the inventory read fails, the category
table is left exactly as it was. Tests: an empty Miniflux category
and an empty GReader label survive sync and appear in
`Db::categories()`; the empty Miniflux category takes a feed
assignment (the edit resolves its server id); a removed,
unreferenced category is pruned while a feed-carrying one is not.

D23 — hosted category Mark All Read uses provider membership. The
trait gained `Provider::mark_category_read`; the gap-2 record above
(the per-feed fan-out and its labeled-article divergence) is
superseded for the hosted path. GReader marks through
`mark_category_read_remote` — `mark-all-as-read` on the
`user/-/label/<path>` stream, Python's `_resolve_stream_id` mapping —
and Miniflux through the new `mark_category_read_remote`
(`PUT /v1/categories/{id}/mark-all-as-read`, Python's Miniflux
mapping); the cache then applies the new `Db::mark_category_read`,
which marks through the same membership clause the category view
displays (feed category minus moved-away, plus `category_override`,
plus `article_labels`). `act_markall`'s hosted category arm calls the
trait method instead of fanning out per feed. Tests assert the
GReader label-stream request shape, and at provider level that the
Miniflux category endpoint is hit (no per-feed marks) and that an
article labeled into the category from an outside feed becomes read
remotely and in the cache.

S15 — the Settings Provider page has an "Inoreader Redirect URI:"
field (appended after the existing provider fields, per the
append-only control rule), prefilled with
`oauth::redirect_uri_from_config` (configured value or the loopback
default) and saved to `providers.inoreader.redirect_uri`. Both
connect flows — the paste-the-URL flow and the browser sign-in
listener flow — take the field's value (trimmed, default when blank)
and use that one URI for the authorization URL and the code exchange,
as Python's dialog worker does. Both flows also store token expiry
through `oauth::token_expires_at_from`, so the value D07's staleness
check reads is the Python `_set_tokens` value in both paths.
Recorded limit: the local listener can only receive redirects aimed
at the default loopback address; a custom URI belongs with the paste
flow (Python's listener variant likewise only serves its own
loopback HTTP shape).

Evidence: `crates/core/tests/audit8_task9_slice.rs` (19 tests),
`crates/core/tests/oauth_slice.rs` (5), `provider.rs` unit tests (3:
refresh-and-persist, revoked-clears, transient-keeps, all against a
loopback token endpoint and a real config file), plus the existing
hosted suites (`hosted_parity`, `providers_full_parity`,
`hosted_gui_slice`, `miniflux_refresh`) unchanged and green. Full
workspace gate on the final tree: `cargo fmt --all` clean,
`cargo clippy --workspace --all-targets -- -D warnings` exit 0,
`cargo test --workspace` exit 0 (69 test binaries ok, 525 tests
passed, 0 failed), `cargo build -p blindrss` exit 0.
Owner acceptance with real hosted accounts (refresh of a genuinely
expired token, remote unsubscribe/category changes appearing after
sync, category mark-all against a live account) remains pending as
platform acceptance, as with all hosted work.
## Audit 8 Task 10 resolution — YouTube merged state and built-in favorites (D17, D18, D19, D20, G06) (2026-11-03)

Reproduction first: the new core suite
`crates/core/tests/audit8_task10_slice.rs` failed to compile against
the pre-fix tree (the merged/favorites/mark APIs did not exist), and
source inspection confirmed the underlying defects — the snapshot
upsert in `Db::save_recommendations` overwrote `date` on conflict,
`Db::recommendations` selected the raw `is_read` flag with a
`current = 1` filter only, and the app's Mark All Read flipped only
the displayed rows' flags. All seven tests pass after the fix; the
app gained two unit tests (`youtube_recommendation_view_scopes`,
`youtube_recommendations_merge_in_date_order`) plus an updated G20
restore assertion.

- **D17 (shared read state) — resolved.** Python's wrapper
  (`providers/youtube_account.py`) computes a recommendation's read
  state as `is_read OR EXISTS (articles.id = 'yt:video:' ||
  video_id AND is_read)`, and its `_set_read` writes both copies.
  Core now uses that exact EXISTS expression in every
  recommendation read shape (`recommendation_articles`,
  `article_by_id`), and the app's `mark_article_read` writes the
  twin through in both directions (recommendation → `yt:video:`
  article and back). The twin rule is gated on the active provider
  being Local in the app, and the SQL clause is naturally inert
  over hosted caches (their article ids are provider-scoped, never
  `yt:video:`), so hosted-account article state is never merged.
- **D18 (snapshot date churn) — resolved.** `save_recommendations`'
  upsert no longer sets `date`: a known video keeps the date it was
  first recommended (Python `core/youtube_account.py`
  `save_recommendations`), while title/author/position/current
  still update. The dedicated view applies Python's wrapper
  re-dating: rows are dated down from `MAX(date)` of the current
  snapshot by position microseconds, so the standalone list's date
  sort reproduces YouTube's current order (regression test pins
  the one-microsecond step). Merged views use the stored dates.
- **D19 (merged views + old favorites) — resolved.** Core gained
  `recommendations_merged` (current rows, stored dates, shared
  read state) and `favorite_recommendations` (favorites WITHOUT
  the current condition — Python's favorites query in the
  wrapper). The app merges current recommendations into the All
  view and into the account category and its ancestors
  (`category_includes_recommendations`, Python's `_view` rule),
  date-sorted into the list via `merge_recommendations`; merged
  views load whole instead of paging, like Python's merged lists.
  The dedicated feed node now also requires a snapshot
  (`has_recommendation_snapshot`, Python's `_has_snapshot`).
  `Db::article_by_id` resolves recommendation ids with no
  current filter, as Python's `get_article_by_id` does.
- **D20 (Mark All Read persistence) — resolved.** Core gained
  `mark_recommendations_read(account, favorites_only)`, which
  persists `is_read = 1` over the wrapper's `_articles(view)`
  scope (current snapshot, or favorites for the Favorites view)
  and marks each video's local feed twin in the same transaction.
  The app's Mark All Read routes through
  `recommendations_mark_scope` in BOTH the local path and the
  hosted path (recommendations are local-only under hosted
  providers, as in Python). Regression test reopens the database
  and verifies the persisted rows and the twin.
- **G06 (built-in Favorites view) — resolved.** The tree gains a
  "Favorites" node with the shared view id `favorites:all`
  (Python adds it whenever the provider supports favorites; every
  backend does). Selecting it lists `Db::favorites` merged with
  `favorite_recommendations`, so favorited recommendations —
  including ones absent from the latest snapshot — appear, and
  unfavoriting removes a row on the refill the toggle already
  triggers. `favorites:all` also restores from the shared
  `last_selected_feed` format again (it previously restored as
  All because there was no node), is excluded from the tree
  context menu's feed variant and from the hosted mark-all
  feed-view routing, and Mark All Read over it marks only the
  favorite recommendations (Python's local provider refuses to
  mass-mark the favorites view itself).

Recorded deviations: none against the plan. The Recommendations
tree node stays at the root (its pre-existing placement); Python
groups the feed under the account category — the view inclusion
semantics above follow Python exactly, and the node placement is
unchanged from earlier batches. Owner NVDA acceptance of the
merged views, the Favorites node and recommendation state flows
remains pending as platform acceptance, as with all GUI work.

Gate in this tree: `cargo test --workspace` 509 passed / 0 failed
(500 at Wave 1 integration + 7 new core tests + 2 new app tests),
clippy `--workspace --all-targets -D warnings` clean,
`cargo fmt --all` run last.

## Audit 8 Task 11 resolution — preview, paging, search and explicit mark scope (G01, G03, G04, G05) (2026-11-03)

Reproduction first: the new core suite
`crates/core/tests/audit8_task11_slice.rs` failed to compile against
the pre-fix tree (the unified view query `ArticleQuery` /
`ViewScope`, `articles_view_page` / `articles_view_count` and
`mark_view_read` did not exist), and source inspection confirmed the
underlying defects — the app's list-selection handler marked the
selected row read and opened, paging existed only for the unfiltered
date-descending view (`view_pages` gate) with hard caps elsewhere
(300 feed/All, 500 category, 1000 favorites), search ran a global
single-phrase query capped at 300 that replaced the current view,
and Mark All Read used the selected node's scope with no
confirmation (and, on the local path, passed the raw view string to
`Db::mark_all_read`, which treats any `Some(x)` as a feed id, so
category/smart/favorites scopes silently marked nothing in the
database). All six core tests pass after the fix; the app gained two
policy tests (`mark_all_prompts_distinguish_global_and_node_scopes`,
`view_query_maps_tree_values_to_scopes`).

- **G01 (preview vs mark triggers) — resolved.** Python's
  `on_article_select` only previews; marking happens on content
  focus (`on_content_focus`) and on activation
  (`on_article_activate` → `_open_article` → `_mark_article_opened`).
  The Rust selection handler is now preview-only (the `mark_opened`
  call and the mark-read-on-select block are removed). A shared
  `mark_selected_read_opened` helper runs from the reader pane's
  focus handler and from the rich WebView's focus handler when
  present, and activation records the opened mark before the
  existing open/play flow. Opened state again backs the Smart
  Folders "opened" criterion.
- **G03 (paging) — resolved.** Core gained a unified view query:
  `ArticleQuery` (scope, read filter, media filter, sort,
  direction, search terms) composed by `Db::view_where` over the
  view's scope — All, one feed, a category subtree under the ported
  membership clause, favorites, or a smart folder's rule matches —
  with `articles_view_page` / `articles_view_count` paging it in
  actual display order at the Python page size (400). Ordering is
  Python's `_sort_articles_for_display`: the primary key in the
  chosen direction with recency (date DESC, id DESC) as the stable
  tiebreak in both directions; the date sort keys on (date, id) in
  the chosen direction. The app's `view_pages` gate and the
  300/500/1000 caps are deleted; every non-merged view pages
  through the unified query with the trailing "Load more items
  (Enter)" placeholder armed from the view count. Regression tests
  tile filtered, category, media-filtered, alphabetical (both
  directions, duplicate titles) and ascending-date views page by
  page and compare against one full page — every matching row
  exactly once, in order.
- **G04 (search) — resolved.** Search is now an in-view filter,
  not a global replacement query. `Db::search_terms` reproduces
  Python's `_filter_articles` term split (lowercase,
  whitespace-split); every term must appear in the article's
  combined search text (title, content, description, author,
  article URL, media URL and feed title — title only in
  `title_only` mode), composed into the view query's WHERE clause
  with LIKE-wildcard escaping, so search preserves the current
  scope, the read/media filters and the display sort, and pages
  like any other view. Whole-load (merged-recommendation) views
  apply the same term rule in memory via `article_matches_terms`.
  The search box also fires on Enter (Python's `on_search_enter`);
  an empty query clears the search. Tests pin terms split across
  fields, scope exclusion, the `title_only` contrast, and
  search + filter + sort paging.
- **G05 (Mark All Read scope + confirmation) — resolved.** The
  File menu / toolbar / shortcut action is now explicitly global
  (Python's `on_mark_all_read`), and the tree context menu carries
  its own action id (`M_MARKALL_CTX`) with the node scope and
  prompt stashed when the menu is built. Both paths confirm first
  with Python's exact prompts (`Mark all items in all feeds as
  read?`, `Mark all items in category "{path}" as read?`,
  `Mark all items in "{feed}" as read?`, falling back to
  `Mark all items in this feed as read?`) under the "Mark All as
  Read" title; Cancel returns before any state change. Execution
  (`run_mark_all`) keeps the hosted routing from Tasks 9/10
  (provider mark-all / `mark_category_read`, recommendations in
  scope marked locally first via `recommendations_mark_scope`) and
  the local path now goes through `Db::mark_view_read`, which
  resolves `cat:` paths through `mark_category_read`, favorites to
  unread favorites only, smart folders through their rule matches,
  the recommendations pseudo-feed to a no-op (routed by callers),
  and anything else as a feed id — fixing the silent no-op above.
  `mark_view_read` is covered per scope by the core suite,
  including a category-labeled article and the recommendations
  pseudo-feed returning 0.

Recorded deviations: (1) Python's local provider pages canonically
(date DESC, id DESC) and the GUI re-sorts only the loaded subset in
memory; per the Task 11 plan, the Rust view query pages in actual
display order directly, so the Rust order is display-true at every
page boundary and full traversal still reaches every matching row
exactly once. (2) Python search filters the view's already-loaded
base, so older matches surface only after paging the unfiltered
base in; the Rust search runs over the whole view scope, a superset
in which older matches are reachable through the search's own
paging. (3) SQL description ordering uses the raw description
column (`LOWER(COALESCE(a.description, ''))`) where Python's key is
a text preview; plain-text descriptions order identically and
HTML-heavy descriptions are an approximation. SQLite `LOWER` /
`LIKE` are ASCII-oriented where Python lowering is Unicode — the
same class of approximation the pre-existing search already had.
(4) Merged-recommendation views remain whole-load by Task 10's D19
design; Task 11 only removed their 300/500 base caps. (5) G01's
event wiring (selection / focus / activation) has no headless
widget test — wxdragon widgets need a display — so its regression
evidence is the code-level separation plus the core suites; owner
NVDA acceptance of the preview/focus behaviour, paging and the
confirmation prompts remains pending as platform acceptance, as
with all GUI work.

Gate in this tree: `cargo test --workspace` 568 passed /
0 failed (6 new core tests + 2 new app tests in this task),
clippy `--workspace --all-targets -D warnings` clean,
`cargo build -p blindrss` green, `cargo fmt --all` run last.
## Audit 8 Task 12 resolution — automatic reader/translation lifecycle (G02, G12, G13, G16, O07, O08, S20, S21) (2026-11-03)

Reproductions first: `crates/core/tests/reader_translation_parity.rs`
(15 tests) failed to compile against the pre-fix tree — the S20
fallback vectors, the S21 response parsers, the runtime-config /
cache-variant identity, the translation request guard and the
reader-cache prefix invalidation did not exist. App policy tests pin
the automatic-load trigger table and the cache-key shape;
article-window unit tests pin Copy Text's source, the failure
stand-ins and the rich caption.

- **S20 (translation compatibility fallbacks) — resolved.**
  `crates/core/src/translate.rs` is now a faithful port of Python
  `core/translation.py`'s provider layer: Qwen walks its three
  regional endpoints (international, China mainland, United States)
  in order per chunk; the default model candidates match Python
  exactly (Grok gains grok-4-0709, grok-3-fast, grok-3, grok-4 and
  grok-beta; Qwen gains qwen-mt-flash, qwen-mt-lite,
  qwen-plus-latest and qwen3.5-plus and drops the nonexistent
  qwen-mt-turbo); a `gsk_` key configured for Grok routes to Groq's
  endpoint and default models, while a user-configured model still
  wins as the single candidate and stored models are never
  rewritten. Fallback advances only on Python's retryable model
  errors (HTTP 400/403/404 whose message names an unavailable
  model); anything else aborts the chunk with Python's
  `HTTP {code}: {message}` detail. Loopback tests drive the
  vectors, the routing and the fallback rule against scripted
  local servers.
- **S21 (translation responses lost or falsely accepted) —
  resolved.** Chat content arrays join their text parts; Gemini
  joins every part of the first candidate; a blank or whitespace
  success is an error ("returned an empty translation response",
  with Gemini's prompt-feedback / finish reason when given),
  never a replacement for the original. Requests now honour
  Python's per-request timeout (`translation_timeout_seconds`,
  clamped 5..=180) and chunk size (`translation_chunk_chars`,
  500..=8000); the chunker is Python's `_iter_text_chunks`
  (contiguous, boundary-preferring, lossless under concatenation;
  results joined with no separator). Gemini sends Python's single
  user prompt with the key as both header and `key` query
  parameter.
- **O07 (automatic translation unreachable) — resolved.**
  `translate::settings_from_config` ports
  `_translation_runtime_config` (enabled AND the selected
  provider's key present; an unknown provider clamps to grok) and
  `TranslationSettings::cache_suffix` ports
  `_translation_fulltext_cache_suffix`. The full-text workers
  (web cascade and YouTube) translate the extracted text on the
  worker before caching — a failure or blank result keeps the
  original — and the reader cache key carries the variant suffix,
  so each provider/language/model variant caches separately.
  Selecting an article while translation is on schedules the
  automatic load (G02), so the pane ends up translated with no
  explicit command; the standalone article window's plain loads
  translate the same way.
- **O08 (late translation replaces another selection) —
  resolved.** Manual translations carry a core
  `TranslationRequest` (article id, reader generation, variant
  suffix). Starting one bumps the reader generation, retiring
  in-flight full-text application like Python's token change; the
  drain applies a result only when the request still matches the
  live generation, the selected article and the *current*
  variant — a delayed A result after selecting B, or after an
  account, provider, language or model change, is dropped, and a
  stale request's error never reaches the status line. On apply,
  the rich document is refetched too, so both reader outputs
  agree after a translation. The guard is unit-tested in core.
- **G02 (plain full text needs an explicit action) — resolved.**
  Selection schedules the automatic full-text load exactly when
  Python does (`auto_fulltext_on_select`: translation ready,
  macOS, forum-thread host, YouTube video), debounced 350 ms and
  tied to the reader generation; reader focus (SET_FOCUS)
  schedules it immediately. Both fire through the existing Read
  Full Text action from the drain timer, so the cache, request
  dedupe and generation guards are shared with the manual
  command, and stale worker results still cannot land (the
  FullText / YoutubeText / RichHtml drain arms were already
  generation- and key-guarded).
- **G13 (Open URL lacks mode-aware translated loading) —
  resolved.** Tools > Open Article opens Python's dialog
  (clipboard URL prefill through `normalize_media_url`, "Open in
  HTML view" defaulting to the rich-view setting, Python's two
  invalid-address messages) and the article window opens
  *immediately* in the chosen starting mode and loads
  asynchronously under its generation token; plain results pass
  through automatic translation, the caption follows the
  extracted page title (plain) or the rendered `<h1>` (rich),
  and a failed load shows Python's `failure_text` /
  `failure_html` stand-ins in the reader. The old
  prefetch-then-open path (`RefreshMsg::OpenedArticle`) is
  deleted.
- **G12 (Copy Text can copy stale hidden text) — resolved.** The
  article window tracks the whole current plain result; Copy
  Text copies the active mode's whole output — in HTML view,
  `collapse_blank_lines(html_to_text(cached document))`, falling
  back to the plain result before the document loads — instead
  of the hidden plain control's stale contents. Ctrl+C in the
  reader keeps its selection semantics.
- **G16 (image-alt changes do not update current output) —
  resolved.** Reader cache keys now begin with the article id,
  and `ReaderCache::remove_prefix` invalidates exactly one
  article's variants. The per-feed image-alt commands and a
  global `show_image_alt` change in Settings both re-render the
  selected article in place (`rerender_current_article`:
  generation bump, per-article cache invalidation, plain body
  rebuilt under the new alt setting, rich document refetched
  when the rich view is on) without moving the selection.

Recorded deviations: Python's automatic full-text path also
raises a "Translation failed" status when its worker-side
translation fails; the Rust worker keeps the original text
silently (no status channel exists inside the extraction
worker; the manual Translate command still reports errors).
Python's Gemini empty-response reason also consults
safetyRatings; the port surfaces promptFeedback and
finishReason. Owner NVDA acceptance of the automatic load feel,
the translation flows, the Open Article window and the alt
rerender remains pending as platform acceptance, as with all
GUI work.

Gate in this tree: `cargo test --workspace` 580 passed / 0 failed
(15 new core tests in `reader_translation_parity.rs`, 2 new app
policy tests, 3 new article-window unit tests), clippy
`--workspace --all-targets -D warnings` clean, `cargo build -p
blindrss` OK, `cargo fmt --all` run last.

**Audit 8 Wave 2 integration (2026-11-04):** Tasks 23, 8, 9, 10,
11 and 12 cherry-picked onto main in dependency order. Two merge
points needed hand work in `app/src/main.rs`: (1) Task 10's
recommendations mark runs beside Task 9's hosted category mark in
`act_markall` (both, not either/or — Python's wrapper marks
recommendations locally beside the remote mark); (2) Task 12 was
branched before Task 11 landed, so its selection handler still
carried the pre-G01 mark-on-select block — dropped at integration:
selection marks nothing, Task 11's reader-focus marking stands,
and Task 12's automatic full-text scheduling was kept in the
selection handler. Two mechanical casualties of the same merge
(a dropped closing brace in the focus handler and Task 11's test
`State` initializer missing Task 12's `auto_load` field) were
fixed in follow-up integration commits. Merged gate: cargo fmt
clean, clippy `--workspace --all-targets -D warnings` clean,
`cargo test --workspace` 588 passed / 0 failed, `cargo build -p
blindrss` OK. Ships as v0.1.56.

**Wave 2 flake root cause and fix (2026-11-04):** The first CI
runs for the Wave 2 tree also failed intermittently in
`dependency_parity` (besides the Windows Clippy failure, fixed
in 65344d3): `nightly_stale_copy_falls_back_to_verified_download`
reported `KeptExistingAfterFailure` instead of `Downloaded` on
Ubuntu CI, and a local run reported `InstalledNew` instead of
`AlreadyCurrent`. Root cause, reproduced locally under CPU load
and captured verbatim in a panic from
`run_tool_captures_output_and_bounds_it`: spawning a
just-written executable fails with ETXTBSY ("Text file busy",
os error 26) for a short window after the file is written and
closed — measured at roughly 1 in 3 immediate execs under load,
worst window ~155 ms, on both tmpfs and disk. The nightly flow
is exactly write-then-exec (download to staging, then probe),
and `probe_version`'s single immediate retry often landed
inside the same busy window, so a healthy copy probed as
broken. Fix: `run_tool`'s spawn (`spawn_tool` in
`crates/core/src/dependency.rs`) retries ETXTBSY with backoff
inside a budget of min(timeout, 2 s); other spawn errors stay
immediate, and a tool that runs and fails is still never
retried. Regression test
`run_tool_executes_freshly_written_files_repeatedly` runs fifty
freshly written scripts back to back. Proof: dependency suite
12/12 full runs under six-way CPU saturation after the fix (it
failed roughly 1 in 10 before); full gate after the fix: cargo
fmt clean, clippy clean (workspace and core
`x86_64-pc-windows-gnu`), `cargo test --workspace` 589 passed /
0 failed, `cargo build -p blindrss` OK.

**Audit 8 Task 13 — lossless metadata enrichment (R01, R13,
R14; 2026-11-04):** `crates/core/src/enrich.rs` rewritten as a
faithful port of Python `core/metadata_enrich.py`, in Python's
source order: JSON-LD restricted to Article-family `@type`s
(first matching node wins; `@graph` flattened breadth-first;
author arrays join up to three names; keyword objects read
their `name`; a list `articleSection` joins through the
keyword list), then Article-family microdata, then OpenGraph
`article:*` (repeated tags kept, profile-URL authors
rejected), then the rust-trafilatura metadata fallback
(author plus tags and categories). Extraction-side cleaning
now dedupes with Unicode `to_lowercase` and caps only the
extracted list (20 tags, 80 code points per tag); the old
code deduped ASCII-only, favoured generic meta tags over
JSON-LD, and capped the merged stored union at 12. R01:
`Db::apply_enrichment` merges through the new
`merge_tag_string` port — every stored tag survives (20- and
45-tag regressions pinned), additions append only below the
40-tag total ceiling, and the write happens only when the
merged string differs from the trimmed stored string,
matching Python's comparison. R13: placeholder authors
(empty, `Unknown`, `(Unknown)`) are filled and real authors
are never replaced; the section is appended to the found
tags at apply time; a missing local row (hosted-provider
article) is a no-op. R14: `render_rich_article` headers
carry resolved metadata with Python's per-path precedence —
ID3 and YouTube structured metadata now override a stale
non-empty fallback whenever non-empty (the author was
previously discarded outright), and on the ordinary
fetched-page path the fallback wins while the new
`extract::title_author_from_meta` (a port of
`_extract_title_author_from_meta`: `<title>` element text
only, author-meta candidates byl, author, article:author,
parsely-author, sailthru.author, `By ` prefix stripped, URL
authors rejected) fills title and author gaps — previously
the author was never filled from the page at all and the
title came from trafilatura's metadata. Enrichment still
issues no network requests: it consumes only the page HTML
the app already fetched; the app caller passes the article
URL (the microdata resolution base) and no longer skips
section-only metadata. Behaviour verified against the
Python implementation running on this box with the spec
libraries (extruct 0.18.0, trafilatura 2.3.1; the Python
app tracks trafilatura from upstream master): for the
common nested-Person microdata shape the extruct stage
yields no author (the name sits under the item's
`properties` and `_person_name` reads only a top-level
`name`) while the trafilatura fallback supplies it from the
`itemprop="author"` element text; the port reproduces both
stages and a regression pins the combined result. Recorded
scope notes: (1) `merge_tags` is unchanged and still serves
feed-ingest categories in `feeds.rs` — Python's ingest path
is not `merge_tag_string`, and the audit did not flag it;
(2) the reconstructed-source render path keeps its existing
title precedence — R14's Python evidence covers the ID3,
YouTube and ordinary page paths; (3) the YouTube header
change has no offline test (it runs a yt-dlp subprocess);
it is the same precedence shape as the ID3 path, which is
regression-tested with local MP3 fixtures. Tests: new
`crates/core/tests/audit8_task13_slice.rs` (14 tests; 12
failed before the fix, all pass after) plus 2 new enrich
unit tests. Gate in this tree: `cargo test --workspace`
604 passed / 0 failed (589 at the Wave 2 base plus the 15
net new tests), clippy `--workspace --all-targets
-D warnings` clean on the Linux target and for
`blindrss-core` on `x86_64-pc-windows-gnu`, `cargo build -p
blindrss` OK, `cargo fmt --all` run last.
### Audit 8 Task 15 resolution — complete Reddit/Lemmy documents in both readers (R02, R03, R04) (2026-11-04)

Python builds ONE complete thread document per Reddit/Lemmy thread and
feeds that same document to both readers. The old Rust code visited
only the initially inlined `t1` replies, so any comment behind a
`morechildren` expansion or a `_` continuation route never reached
either reader (R02); Lemmy fetched a single v3 page of 200 comments
at depth 8, sorted comment paths lexically (so comment 10 printed
before comment 9), and parsed the query/fragment suffix as part of
the post id, rejecting valid URLs such as `…/post/123?x=1` outright
(R03); and `reconstructed_article_html` excluded Reddit and Lemmy,
so the rich reader cleaned the original shell page instead of the
rebuilt document (R04).

Implementation (`crates/core/src/threads.rs`, `extract.rs`): both
builders now mirror `core/article_extractor.py` in the Python tree.
The Reddit collector queues `more` nodes exactly as Python does —
`morechildren` batches of 100 children, at most 100 expansion
requests, `_` continuations fetched only once the id queue drains,
dedupe by fullname across the initial payload and every expansion,
a 10,000-comment cap — then rebuilds display order by grouping
children under their parent and emitting depth-first in arrival
order, with orphans appended at their stored depth. When the JSON
route yields nothing, the RSS fallback document is built instead
(entries deduped by id/link, content preferred over summary, empty
entries skipped, flat depth-0 blocks). The Lemmy fetcher pages v3
(`page`) and then v4 (`page_cursor`) at limit 50 with
`max_depth=999` and `sort=Old`, at most 250 requests and 10,000
comments, deduping by comment id else `ap_id`, stopping on an empty
page, a short v3 page, or an empty/repeated v4 cursor; a first-page
failure fails that API version (so v3 falls through to v4) while a
later-page failure keeps the document built so far. Comments stay
in API order — paths now only set depth (segments excluding `""`
and `"0"`, count minus one) — and the post id is parsed from the
URL path only, so query/fragment suffixes no longer break valid
Lemmy URLs. Both builders emit the same semantic document shape
Python emits (`blindrss-reddit-post` / `blindrss-lemmy-post`
sections; `#N Posted|Comment by author — YYYY-MM-DD HH:MM UTC —
Reply level D` headings; `body_html`/`selftext_html` fragments with
entity-escaped fragments unescaped first, plain bodies split into
paragraphs, the Linked content line under Python's exact permalink
rules, `[removed]`/`[deleted]`/`[no text available]` markers,
Markdown bodies). `reconstructed_article_html` now chains
`reddit_html`/`lemmy_html` after the forum reconstructions, so the
one document feeds the plain reader through `html_to_text` and the
rich reader through `clean_article_html`, as in Python.

Evidence, regression-first: 13 new tests in
`threads::reddit_lemmy_tests` pin the Python behaviour per finding —
morechildren + continuation expansion in parent order with dedupe
and exact request counts, repeated-more termination, the
10,000-comment / 100-request bounds, body-HTML variants, the
linked-content permalink rules, malformed-payload rejection, the
RSS fallback, Lemmy query/fragment URLs, v3 paging at full depth
in API order (comment 9 before comment 10), v4 repeated-cursor
termination, and first-page vs later-page failure handling — plus
two integration tests in `forum_sources_parity.rs`
(`reddit_document_is_identical_in_both_readers`,
`lemmy_document_is_identical_in_both_readers`) asserting the plain
(`html_to_text`) and rich (`clean_article_html`) surfaces yield the
identical complete list of reply identities for the same built
document. Gate on this tree: `cargo fmt --all` clean, `cargo
clippy --workspace --all-targets -- -D warnings` clean,
`cargo test --workspace` 604 passed / 0 failed (589 on main
`6da89d0` before this task; core 570 passed / 0 failed),
`cargo build -p blindrss` OK. No `cfg(windows)` code was touched,
so the Windows-gnu clippy leg did not apply.

Deviations, named honestly:

- The Lemmy origin is rebuilt from the parsed URL's host and port
  rather than Python's raw netloc string; a URL carrying userinfo
  would lose it. No real instance URL carries userinfo, and the
  API requests are unaffected.
- Heading, title and attribute escaping reuses the shipped GitHub
  document escaper, which emits `&#39;` for an apostrophe where
  Python's `html.escape` emits `&#x27;`; both decode identically
  in the plain and rich readers.
- Markdown is rendered by pulldown-cmark 0.13 (tables, footnotes,
  strikethrough and task lists enabled, approximating
  Python-Markdown's `extra` + `sane_lists`) rather than
  Python-Markdown itself. Python's escape-first rule is preserved,
  so raw HTML inside a post stays literal text; pathological
  Markdown edge cases can differ in markup shape, not in the text
  either reader speaks.
- The pre-existing fetch gate is unchanged: in
  `fetch_full_text_opts` a longer feed-supplied text still wins
  over the reconstructed document for Reddit/Lemmy, exactly as for
  forum reconstructions (GitHub alone was already exempted). When
  the document is the selected content, both readers receive the
  same complete document, which is what the identity tests pin.
- Owner screen-reader acceptance is NOT claimed: NVDA/JAWS/
  VoiceOver passes over the rebuilt Reddit/Lemmy documents in both
  readers remain Brandon's manual gate, per the house rules.
## Audit 8 Task 14 resolution — publisher reconstruction, quiz removal, lead fidelity (R05, R06, R07, R08, R09) (2026-11-04)

**R05 (Sky Flourish tables) — decoded, not copied.** The embed
expansion no longer lifts literal `<table>` elements out of the
Flourish embed page; it decodes the published dataset the way
Python does (`_flourish_table_nodes`): a balanced-bracket scanner
extracts `_Flourish_data`, `_Flourish_data_column_names` and
`_Flourish_data_metadata` without executing JavaScript,
`new Date(<ms>)` tokens outside strings become typed date cells,
and tables are built from the data — the "columns" shape and the
binding-keys shape, headers padded with "Column N", datasets
with fewer than two columns or no rows skipped, parts of 100
rows with a "Table, part X of Y" caption when split. Dates are
formatted with epoch arithmetic (portable for pre-1970 values)
using the metadata's d3-style output format, ISO with `Z` when no
format is published. All of Python's guards stand (news.sky.com
only, the "flourish-table" marker, the data-src shape, at most
12 widgets, the article URL as Referer, the 8 MB embed cap); a
widget whose embed decodes to nothing is left unchanged —
Python has no literal-table fallback, and the old Rust fallback
is removed to match. Fixtures are ported from
`tests/test_sky_flourish_tables.py`, including the pre-1970
datetimes (-2208923100000 with `%-I.%M%p` reads "6.15PM") and
the classic linearization ("Table with 3 rows and 3 columns.",
"Row 1: Stage: First contact; Norwich: 6.15PM; ...").

**R06 (rich-reader reconstruction) — ported.** `clean_article_html`
now runs Python's reconstruction order before the generic
main-node pick: publisher body reconstruction (URL-substring
detect for arstechnica.com / theregister. / macrumors.com /
zdnet.com; host map to `div.post-content`, `.bodytext`,
`.js-content`, `.entry-content`; containers must sit inside the
first `<article>`; nested containers dropped; the junk classes
decomposed; `.zd-related-stories-flow` unwrapped because it
wraps real prose beside a widget; empty result falls through),
then socast reconstruction (lead `div.wpb-content-wrapper`
articles that are not inside a mainArticle, merged with every
`article.mainArticle`, nested candidates deduped, junk-token
elements stripped, under-200-chars result rejected), then the
forum rebuild. Any successful reconstruction blanks the
trafilatura keep-text, so a rebuilt body is exempt from the
prune — previously only the forum rebuild was exempt, and the
publisher/socast reconstructions fed only the plain-text
reader. The article_html module doc no longer lists that
divergence. Regression tests in `structure_parity.rs` cover
Ars Technica, ZDNet, MacRumors, The Register and both socast
shapes.

**R07 (CSS-only quizzes) — ported with the guards.** `strip_css_quizzes`
is a direct port of Python's `utils.strip_css_quizzes`: the
regex fast path collects distinct radio group names and returns
the input byte-identical below three groups; radio inputs are
grouped by direct parent; a parent holding three or more groups
is removed unless it is a page wrapper (html/body/main/article/
form) or contains an article/main descendant; when nothing is
removed the original string is returned untouched. It is wired
at both Python call sites — the head of article extraction and
the head of `clean_article_html` — so a quiz can neither reach
the reader nor be vouched for by the prune's ground-truth text.
Fixtures mirror `tests/test_article_quiz_removal.py` (the
How-To Geek vignette quiz is removed end to end; one-group tabs,
polls, and body/form-direct radios are preserved).

**R08 (JSON-LD fidelity) — ported.** `json_ld_text` is rewritten
to Python's `_collect_json_ld_text` / `_extract_json_ld_text`:
list-valued `articleBody`/`text` fields are collected (joined
with a space) alongside string values; entity-escaped markup is
unescaped once when at least three escaped block closers mark it
as real markup; a markup body keeps the standfirst text before
the first tag and its leaf-block paragraph structure
(`_html_fragment_to_text`: script/style/noscript/template
removed, p/li/heading/blockquote leaf blocks as paragraphs,
punctuation spacing fixed); a plain body is fully
entity-decoded. Every candidate passes
`_strip_embedded_script_code` and `_normalize_whitespace`
BEFORE the longest-wins comparison, so leaked script source
cannot win on bulk; the 120-char minimum stands. When the
JSON-LD candidate is the one served, it is patched with
`_patch_json_ld_missing_lead` (Python `_prepend_missing_lead`:
at most four alternative paragraphs scanned, at most two leads,
a 60-char alignment snippet against the base head's 400 chars,
reasonable-paragraph rules) against the cascade's base text and
the DOM article paragraphs (`_extract_article_paragraph_text`),
re-attaching a CMS-dropped lede exactly once. Arbitration
deviation, named: Python compares JSON-LD only against
trafilatura (it wins at >1.1x) inside `_extract_text_any`; the
Rust hybrid cascade keeps its own gate — the candidate must
beat both the base and the second-opinion text — and the patch
applies whenever the jsonld candidate is served. Recorded
engine divergence: on a body-less synthetic page,
rust-trafilatura mines the ld+json body itself, flattens it to
a single line (measured: source=base, no newlines, 547 chars
against the stripped candidate's ~345) and wins on bulk;
Python trafilatura preserves line structure, so Python's final
postprocess strip removes the leaked code run from the winning
text, while the line-based strip cannot fire on a single-line
text. The candidate layer is pinned by the unit test
`json_ld_candidate_strips_leaked_script_code`; the flattening
itself is base-engine behaviour, outside this task's files.

**R09 (lead recovery) — the lean invention replaced by the
faithful port.** The old `recover_lead` prepended up to three
recall paragraphs on EVERY host — behaviour Python never had,
which could prepend unrelated prose — and sliced the first
paragraph's match key at byte 120, which panics when a
multibyte character straddles that byte. It is replaced by
`attempt_lead_recovery`, a port of `_attempt_lead_recovery` and
its helpers: the wirtualnemedia.pl host allowlist
(`_lead_recovery_enabled`); the meta description in
og:/name/twitter order with trailing-ellipsis stripping; the
60-normalized-char minimum; the description-snippet absence
gate against the precision text; the allowlisted
`div.wm-article-header-lead` node path; the recall path, which
requires the description snippet inside the first 8000
normalized recall chars and a description hit among the
recovered intro paragraphs (at most 8 scanned, at most 2 kept,
page-title and precision-paragraph guards); and the
meta-description fallback prepend. All slicing is char-based.
Tests pin the allowlist gate both ways (same page on another
host recovers nothing), the lead-node path, the unrelated-lead
guard, and survival of a multibyte character at byte 120.

**Investigation — ID3 extended/unsynchronized frames:
demonstrated loss, fixed.** Ground truth was established
against mutagen (the parser Python uses, run locally) on
synthetic byte shapes: tags carrying the 0x40 extended-header
flag and unsynchronised tags/frames parse fully there. The
Rust reader started its frame scan at offset 10
unconditionally — landing inside the extended header's size
bytes and losing every frame — and never de-unsynchronised,
mangling text and misaligning frames. `id3_metadata` now
mirrors mutagen: the extended header is skipped with mutagen's
own heuristic (bytes at the frame offset that decode as a
known frame ID mean the flag is bogus and nothing is skipped;
the v2.3 size is big-endian excluding itself, the v2.4 size
synchsafe including itself, non-synchsafe v2.4 sizes rejected);
v2.3 tag-level unsynchronisation is decoded before scanning;
v2.4 per-frame flags are handled in mutagen's `_fromData`
order — the 4-byte data-length prefix for DATALEN/COMPRESS,
per-frame or header unsynchronisation, encrypted frames
skipped as unsupported, COMPRESS frames zlib-inflated (flate2,
already in the workspace lock; output capped at 4 MB where
mutagen is uncapped); an unparseable frame is skipped rather
than ending the scan, and empty frames are dropped, both as
mutagen does. Six fixtures in `batch35b_slice.rs` pin the
shapes (both extended headers, the v2.3 unsync tag, v2.4 frame
unsync + data-length, and compressed USLT in both versions
against a pinned Python-zlib byte stream). Remaining named
gap: mutagen's `determine_bpi` heuristic for iTunes-era v2.4
tags written with plain (non-synchsafe) frame sizes is not
ported — the reader keeps synchsafe v2.4 sizes, as it did
before this task.

**Investigation — relative srcset: demonstrated gap, fixed.**
Ammonia's `UrlRelative::RewriteWithBase` rewrites `src`/`href`
but passes `srcset` through untouched (measured: the `src` was
absolutized, the candidates were not), so relative candidates
reached the WebView unresolved — Python absolutizes them in
its final attribute pass (`_absolutize_srcset`). That pass is
now ported into `normalize_media_srcs` for img/source:
candidates are split on commas, each URL absolutized against
the article URL, descriptors preserved. Pinned by
`rich_srcset_candidates_are_absolutized` in
`structure_parity.rs` (relative, root-relative and absolute
candidates). An earlier draft of this section claimed verified
equivalence from a truncated test log; the full run proved
otherwise and the claim is corrected here.

Gate in this tree: `cargo test --workspace` 616 passed / 0
failed (baseline 589 at 6da89d0; +27 Task 14 tests across
`extract_parity.rs`, `structure_parity.rs`, `batch35b_slice.rs`
and the extract.rs unit tests), clippy `--workspace
--all-targets -D warnings` clean, `cargo build -p blindrss`
OK, `cargo fmt --all` run last. No cfg(windows) code was
touched, so the windows-gnu clippy leg was not required.

**Audit 8 Wave 3 batch 1 integration (2026-11-04):** Tasks 13,
14 and 15 cherry-picked onto main in task order (ee707e6,
b433a0d, 76cbe5a). The only conflicts were append-only
collisions in this file (Tasks 15 and 14 against the Task 13
paragraph), union-resolved so every resolution stands.
`extract.rs` was touched by all three workers in disjoint
regions and auto-merged; the merged tree was gated as a whole
rather than trusting the three per-tree gates: cargo fmt
clean, clippy `--workspace --all-targets -D warnings` clean,
clippy for `blindrss-core` on `x86_64-pc-windows-gnu` clean,
`cargo test --workspace` 646 passed / 0 failed, `cargo build
-p blindrss` OK. Ships with the Wave 3 release.

**Batch 1 Windows CI fix (2026-11-04):** The batch's first CI
run failed only on Windows, in Task 13's
`rich_header_id3_metadata_overrides_stale_fallback`: a local
MP3 path with backslash separators produced an empty path in
`extract::url_path_lower` (it split on '/' only), so
`looks_like_id3_media_url` was false and the ID3 branch of
`render_rich_article` never ran — a real Windows defect for
local enclosures, not just a test artifact. `url_path_lower`
now normalizes '\\' to '/' before splitting; regression test
`media_url_detection_accepts_windows_file_paths` pins Windows
and POSIX path shapes on every platform.
## Audit 8 Task 18 resolution — one immediate cancellable player session (M01, M10, M11) (2026-11-04)

Reproductions first: the policy the findings demand did not
exist, so the new core tests pin Python's behaviour directly —
`player_session.rs` unit tests (identity matching by id then
target, toggle keeping the generation, a switch staling the old
generation, forced loads never toggling, internal loads staling
external results, close forgetting identity while generations
keep moving), `announce.rs` HiddenAnnouncer tests (each load
state spoken once per load while hidden, recorded-but-not-spoken
while shown so a later hide never replays it, queue-advance
suppression of the routine states but never of failure), and
`playback.rs` seek-summary tests (Python's exact
elapsed/remaining/total wording and time formats).

- **M01 (competing players) — resolved.** The `PLAYERS` vec is
  deleted. There is one `SESSION` registration holding one
  `SessionControl`, and `blindrss_core::player_session` owns the
  identity/generation policy (a port of `_ensure_player_window`
  + `is_current_media`). Every activation goes through
  `player::begin_play` / `begin_queue_play` on the UI thread:
  the session already holding the item toggles (Playing pauses;
  Stopped reloads the retained stream through the shared load
  path, as Python's `resume_or_reload_current` does; otherwise
  it resumes); anything else stops the old playback in the same
  window (`begin_load` persists the outgoing position first,
  when media actually loaded) and enters the loading state.
  A queue start always loads (Python's queue path never takes
  the toggle branch). The play queue advancing on media finish
  bumps the generation from inside the session
  (`begin_internal_load`), and closing the window clears the
  session and forgets the policy identity. Main-window command
  dispatch targets the one session.
- **M10 (visibility + hidden announcements) — resolved.**
  `show_player_on_play` now defaults to hidden at both Rust
  read sites (player visibility, Settings checkbox): Python's
  `DEFAULT_CONFIG` carries False and its loader merges defaults
  into every config, so the settings dialog's local True
  fallback never wins there. A fresh load with the preference
  on shows + raises + focuses Play/Pause; with it off the
  window is hidden and focus is not touched (no focus steal).
  `HiddenAnnouncer` (core `announce.rs`) ports
  `_announce_when_hidden`/`_hidden_announced`: "Loading
  media..." at load begin (after visibility is applied, as
  Python's deferred announce observes it), "Playing" at the
  tick's first Playing transition of the load, and "Failed to
  load media" when a delivery fails, a resolution fails, the
  autoplay watchdog gives up, or a queue switch cannot load.
  Queue auto-advance pre-suppresses the routine states and the
  old unconditional "Now playing: {title}" announcement is
  removed (frame title and status still follow the queue).
  Explicit Play presses record the hidden set so the readiness
  announcement never doubles what the press already said.
  Seeks (buttons, arrow keys, slider, Player-menu dispatch)
  (re)arm a 400 ms debounce — Python's `wx.CallLater(400)` with
  Restart — and the tick speaks
  `playback::seek_summary` ("Elapsed …, Remaining …, Total …",
  elapsed-only for unknown length) via `media_navigation`.
  The visible status texts are unchanged ("Could not load
  media", "Could not play this media"); only the spoken text
  follows Python.
- **M11 (creation waits for resolution/metadata) — resolved.**
  All four producers (Play, Open Media URL, Video Search Play,
  play-queue start) call `begin_play`/`begin_queue_play`
  BEFORE spawning their resolver: the window is created in its
  loading state (`create_session` loads nothing) or reused in
  it. `RefreshMsg::MediaReady` now carries the player
  generation, and the drain delivers only the exact pending
  (article, generation) pair; `deliver_media` re-checks the
  generation and the article identity. A stale result — success
  or failure — is dropped whole and never starts audio or
  touches the status line. Cancelling is activating something
  else, starting a queue, changing the data location
  (pending generation zeroed) or closing the window. The yt-dlp
  metadata lookup moved off the critical path: workers send the
  stream first, then `MediaInfoReady`; its chapters are cached
  to the database as before and reach the player through
  `update_chapters`, which refuses a stale generation.
  Cancellation hygiene found on the way: position saves
  (outgoing save, Stop, close) now run only when media
  actually loaded — with an immediate window, closing or
  stopping mid-load would otherwise overwrite the article's
  saved resume position with zero.

Session shape left for Tasks 19–21: `start_loaded_stream` is
the single load path (delivery, queue switch, toggle-reload),
`PlayerState` retains the current stream/headers for reloads,
and chapters are replaceable after delivery.

Adjacent item, state reported, NOT fixed here:
`MediaBackend::load_uri` still passes an empty referer to
`vlc::media_options`, so the `:http-referrer` option is never
emitted. The session rework did not change that signature; the
fix belongs to Task 19, whose plan file list includes
`media_backend.rs`.

Deviations, named: (1) Python's toggle branch also refreshes
chapters when they differ; the Rust toggle does not fetch
chapters — chapter acquisition is Task 20's mechanism, and
this task adds only the post-delivery `update_chapters` it
will use. (2) Python's `resume_or_reload_current` has a
casting branch; the Rust cast flow is a separate main-window
path that never used the player window and is unchanged.
(3) A queue switch now applies the persisted volume AND the
configured playback speed through the shared load path (it
previously re-applied volume only) — that is Python's
per-load behaviour. (4) Seek amounts and the volume step are
re-read from the config at use time; the one long-lived
session would otherwise freeze their creation-time values
(Python reads config per action). (5) Hidden "Playing"
readiness is observed on the 1-second tick's play-state
transition rather than a backend event, so it can land up to
a second after Python's status-driven announcement.
Accessibility pass before the UI edits: no controls were
added, removed or reordered; accessible names are unchanged;
the seek slider's MSAA snapshot contract is untouched (the
tick remains the only writer); the new PlayerState fields are
plain UI-thread state read by no accessibility callback; the
visibility change is the M10 point itself — hidden mode never
calls set_focus or raise. Owner screen-reader acceptance
(NVDA toggle feel, hidden load/failure/readiness speech,
debounced seek summaries) is not claimed.

Gate in this tree: `cargo test --workspace` 661 passed / 0
failed, read from the run's own summary and confirmed by a
full recount (661 / 0 again). The delta is exactly this
task's +15 new tests — 9 player_session, 4 HiddenAnnouncer,
2 playback seek-summary; a diff census counts 15 `#[test]`
added and 0 removed. The parent commit's true total is 646:
the v0.1.56 base of 589 plus batch 1's per-task deltas
(+15 Task 13, +15 Task 15, +27 Task 14, matching each task's
own gate line). The batch-1 merged gate's reported 616
undercounted the same commit by 30 — it coincides with Task
14's single-tree count — though that run also had zero
failures. Clippy `--workspace --all-targets -D warnings`
clean (read from the run's own log after a first attempt's
processes stalled orphaned on the wx check build and were
killed by PID; the re-run finished clean), `cargo build
-p blindrss` OK, `cargo fmt --all` run last and `--check`
clean. No cfg(windows) code was touched, so the windows-gnu
clippy leg was not required.
## Audit 8 Task 17 resolution — shared keyboard normalization + editor semantics (S01, S04, S05, S06, G15, M14) (2026-11-04)

**S01 — shortcut edits round-trip.** The editor wrote
overrides through `Config::set_path("keyboard_shortcuts.<dotted
id>")`, which nested `{"feeds": {"refresh_all": ...}}` where
Python stores one flat `{"feeds.refresh_all": ...}` map; the
dispatch rebuild reads only flat string values, so every edit
was silently ignored after the dialog closed and at relaunch.
Core now owns the shape: `shortcuts::overrides_from_config`
reads the flat map (non-string values ignored, as Python's
resolve ignores them) and `shortcuts::store_overrides`
replaces the whole object flat, in command-table order. The
dialog applies through that store; the main window's
`build_keymap` is core's `accel_map` over the same read.
Pinned by `overrides_round_trip_flat`: flat shape on disk, F6
resolves after reload, an explicit "" unbind survives the
round trip, an empty map restores defaults.

**S04 — one strict normalization + one event bridge.**
`normalize_accelerator` is now a faithful port of Python's
`parse_accel`/`format_accel`: F1–F24 only, an unknown modifier
or multi-char key invalidates the whole string, duplicate
modifiers collapse, the literal '+' key special case
("Ctrl+Shift++") parses, canonical Ctrl/Alt/Shift/Cmd order.
The wx event bridge (Python `gui/shortcut_keys.py`) lives in
core as `event_accel`/`keycode_token`: Control/Alt/Shift/Meta
map 1:1 (Cmd is never merged into Ctrl), named/navigation
keys, numpad digits as digits, numpad Enter as Enter, shifted
punctuation as its unshifted token, F1–F24. Dispatch and
capture both require a modifier, bare function keys excepted
— Python's `event_to_accel(require_modifier=True)`. The main
frame hook no longer blanket-skips Alt and no longer folds
Cmd into Ctrl; bindings resolve through
`resolve_bindings`/`accel_map`, which iterate the command
table in order so a conflict's winner is Python's (the old
inversion iterated a HashMap and picked an arbitrary winner).

**S05 — the editor is Python's dialog.** Columns
Category/Command/Shortcut at Python's widths, "(none)" for
unbound, the list named "Keyboard shortcuts", buttons
"&Change Shortcut...", "&Remove Shortcut", "Reset All to
&Defaults", Close. The free-text Edit path is gone (Python
has none): Change opens the capture dialog — a prompt naming
the command, the candidate in a readout whose accessible name
is "Captured keyboard shortcut", OK disabled until a valid
combination is captured, Escape cancels, Enter confirms.
Changes apply immediately (the whole flat map per edit); a
conflicting accelerator asks YES/NO "… is already assigned
to … Reassign it to …?" and YES unbinds the other command;
Remove writes an explicit "" unbind and is enabled only on a
bound command row; Reset All confirms, then clears all
overrides. The 12 fixed-keys rows (core `fixed_key_rows`,
Python's table verbatim) append as read-only rows with
Change/Remove disabled. The dialog's working state is the
overrides map itself, re-resolved after every change — the
old dialog edited against a stale pre-dialog snapshot of the
effective map. Focus returns to the list after each
operation, as Python restores it.

**S06 — binding-aware F1 everywhere.** Core `help_key_action`
ports `HelpKeyFilter._is_help_key`: the accelerator bound to
`help.user_guide` opens context help; F1 opens context help
while F1 itself is unbound; F1 given to another command
dispatches that command. The main frame's hard F1 intercept
(which ignored rebinding) and every dialog's fixed-topic F1
now route through it: the main frame keeps its
focused-control topics (tree/list/reader/search), dialogs
keep their DIALOG_TOPICS kind, Settings resolves the current
page through NOTEBOOK_PAGE_TOPICS keyed by the English source
page titles (locale-independent), and the shortcuts capture
dialog binds no F1 at all — F1 is capturable input there, as
Python skips its help filter while capturing.

**G15 — Edit Selected is one smart route.** The char hook
special-cased `feeds.edit_selected` into category-rename vs
feed-edit, so smart folders wrongly opened feed editing.
Dispatch now sends the command to M_EDIT_SELECTED, whose
`act_edit_selected` already branches cat:/smart:/feed
correctly.

**M14 — the player takes the whole registry.** The player
hook no longer drops every Ctrl/Cmd/Alt event. Order, as
Python's `player.on_char_hook`: Enter in the chapter list
commits (any modifiers); then the bridge accelerator goes
through the help rule (chapter list → "chapters", else
"player"); then it delegates to the main frame's dispatch
with the text guard off — the main frame installs an Rc
delegate (`player::set_shortcut_delegate`) over its shared
dispatch closure, which reads the live keymap; then media
arrows resolve (Ctrl+Arrow everywhere, Alt+Arrow on macOS
only, Shift/Cmd/Ctrl+Alt disqualify) onto the existing
seek/volume actions. Unclaimed modified keys pass through to
the focused control. The main frame calls the same dispatch
closure with the text guard on: Python's
`_SHORTCUT_TEXT_GUARDED` set (player.play_pause/stop,
queue.next/prev, the six filter.* commands) defers while the
search field is being edited, via `state.search_focused`.

**Deviations, named.** (1) Python's app-wide event filter
(wx.App.AddFilter) also keeps Ctrl+P and the media arrows
live while a modal dialog has focus; wxDragon exposes no
event-filter API, so dialog-focus transport is not
replicated — delegation covers the two top-level windows
(main frame, player) and dialogs keep their own keys.
(2) Python's menu-highlight F1 route (WM_HELP on a
highlighted menu item → that item's topic) has no wxDragon
equivalent; F1 pressed over a menu falls back to the
frame/dialog topic. (3) Player F1 topics cover the chapter
list ("chapters") and the frame default ("player"); Python's
finer per-control topics (player-controls, playback-speed,
casting) need focused-control identities the Rust player does
not tag, so the seek/volume controls, speed combo and cast
button answer with "player".

**Regression evidence.** `crates/core/tests/audit8_task17_slice.rs`,
11 tests (strict normalize, resolve/accel_map order, flat
round-trip, bridge key families, media arrows, the help rule,
the text-guard set, fixed rows), written before the
implementation; the strict-normalize pins were also run
against the pre-task core and failed there as pinned (it
returned "F25" where Python yields unbound). The app-side
routing (G15's menu target, M14's delegate, S05's dialog
flow) is wiring over these pinned decisions and is covered by
the workspace gate, not GUI automation (none exists in this
repo). The pre-existing shortcut suites (i18n_help_slice,
batch9c_slice) stay green. Platform/screen-reader acceptance
of the reworked dialogs and player keys remains Brandon's.

Gate in this tree: `cargo test --workspace` 657 passed / 0
failed (baseline 646 at 76cbe5a on the clean tree; +11, all
of them the Task 17 slice), clippy `--workspace
--all-targets -D warnings` clean, clippy `-p blindrss-core
--all-targets --target x86_64-pc-windows-gnu -D warnings`
clean (this task touches Windows-facing key handling),
`cargo build -p blindrss` OK, `cargo fmt --all` run last.

**Wave 3 batch 2 integration note (2026-11-04):** Tasks 17 and
18 both branched from 76cbe5a and were cherry-picked onto main
in that order. Three merge points needed judgment, resolved as
follows. (1) player.rs: both tasks appended helpers after
`dispatch_command` — kept both blocks (Task 18's session
helpers, then Task 17's shortcut delegate). (2) main.rs:
`MediaResolution` keeps Task 18's shape (URL + headers; yt-dlp
info travels as `MediaInfoReady`) and gains Task 17's
`ShortcutDispatch` type. (3) player.rs media-key volume arms:
Task 17's sites used its local `volume_step` value while Task
18 made it a module function re-reading config at use time —
the two arms now call `volume_step()` like Task 18's other
sites. Merged gate: fmt clean, clippy clean (workspace and
core windows-gnu), 673 passed / 0 failed, build OK. The count
reconciles: 646 (batch 1) + 1 (Windows-path regression test)
+ 15 (Task 18) + 11 (Task 17).

## Audit 8 Task 19 resolution — verified seek/resume and asynchronous persistence (M05, M06, M07, M08, M21) (2026-11-04)

M05 (seeks verify landing): seeks are no longer assumed to
have landed. Core gains `SeekVerifier`
(`crates/core/src/playback.rs`), a pure clock-injected state
machine carrying Python's two mechanisms in one model: the
resume restore (landing tolerance 1500 ms, at most one seek
issued, silent give-up after 8 s, seek support learned false
only when the backend itself reports unseekable and the input
does not look seekable, learned true on landing) and the
user-seek guard (10 attempts, 3 reapplies, tolerance 3000 ms
local / 5000 ms remote, remote gives up below 7 attempts,
progress toward the target within 0.9 s pauses the budget,
failure announced only when the remaining gap is at least
8000 ms). The player tick drives the armed verifier once a
second; delivery and queue advance arm the Resume form,
every user seek (buttons, slider, chapter buttons, chapter
list commit, char-hook arrows, media-arrow shortcuts) routes
through `begin_user_seek`, which cancels a pending resume
(Python `_note_user_seek`), clears the stopped-resume flag,
marks the stream seek-supported and arms the guard on the
backend's computed target. The failure speaks exactly once —
the verifier ends when it reports — as "Could not seek
there. Position …" through the general announce channel.
The tick reads the phase through a new
`MediaBackend::stream_phase` (VLC Opening/Buffering states
1/2, which `get_state` collapses to Stopped and so could not
express "wait") and seekability through a new
`MediaBackend::is_seekable` (new `libvlc_media_player_is_seekable`
FFI entry); learned support persists through the M07 write
queue.

M06 (writes preserve optional shared fields):
`Db::upsert_playback_state` now uses Python's ON CONFLICT
CASE form — stored duration / seek_supported / title survive
a write carrying None, explicit values replace — with
Python's sanitization (position clamps at zero, a
non-positive duration counts as absent). New
`Db::set_seek_supported` is a plain UPDATE (no row, no
insert), and `Db::set_busy_timeout_ms` exposes the timeout
the M07 worker needs.

M07 (persistence off the UI thread): position saves and
seek-support updates no longer open or write the database
from player code. A per-session persistence worker thread
owns its own connection (500 ms busy timeout, Python's
playback-write timeout), receives saves over a channel into
core's `PendingPlaybackWrites` latest-wins queue, and
flushes on a 450 ms cadence; a failed write — a locked
database above all — stays queued and is retried, never
dropped. The worker reports each landed save back, and only
then do the tick's `last_saved_pos` / `last_save` advance:
bookkeeping can no longer run ahead of unwritten state. If
the worker's own open ever fails, `save_position` keeps the
old synchronous path rather than lose persistence. The
locked-database regression test holds BEGIN EXCLUSIVE on a
second connection: the flush reports nothing written, keeps
the save queued, and lands it after the lock clears.

M08 (Stop then Play resumes): Stop sets
`stopped_needs_resume` (only when media actually loaded)
and ends any pending verification; every Play path — the
Play/Pause closure and both `toggle_or_reload` branches —
first runs `resume_after_stop`, which re-reads the saved
row, recomputes the target through `ResumePolicy`
(saved position minus the configured rewind), arms the
verified restore, and clears the flag whether or not a
restore applied (Python's finally-clear).

M21 (soundcard changes reach the live session): new
`MediaBackend::apply_soundcard` calls the engine's
`set_output` on the running session — no reload, position
untouched. The settings action in `main.rs` compares
`preferred_soundcard` before and after the dialog (the same
prior/now pattern as the image-alt dispatch) and routes a
change through `player::apply_preferred_soundcard`.

Referer (the adjacent item Task 18 left, above):
`MediaBackend::load_uri` no longer passes an empty referer.
`load_uri_with_headers` extracts the playback headers'
`referer` and threads it into `vlc::media_options`, so
`:http-referrer` is emitted as Python's `load_vlc_url`
emits it (the range-cache relay also still applies it on
its own fetch); plain `load_uri` falls back to Python's
`referer_for_url` default — the media origin root for
remote http(s) URLs, nothing for loopback or local inputs.

Tests: new `crates/core/tests/audit8_task19_slice.rs`
(6: upsert preserves on None, explicit replaces and bad
values sanitize, policy save preserves learned support,
set_seek_supported updates only, pending writes latest-wins
and report-written, locked-DB survival and recovery), 9 new
playback unit tests (resume waits through Buffering then
seeks once, landing learns seekable, rejected seek gives up
at 8 s and learns unseekable, seekable-looking input never
learned unseekable, user seek cancels a pending resume,
guard reapplies exactly 3 times then speaks once, remote
tolerance landing, small-gap failure stays silent,
Stop→Play target is saved minus configured rewind), 3 new
media_backend referer tests. Gate in this tree:
`cargo test --workspace` 691 passed / 0 failed (baseline
673 on 5d2e0e1; +18, reconciling as 9 + 6 + 3), clippy
`--workspace --all-targets -D warnings` clean, clippy
`-p blindrss-core --all-targets --target
x86_64-pc-windows-gnu -D warnings` clean, `cargo build -p
blindrss` OK, `cargo fmt --all` run last.

Deviations, named: (1) The VLC backend's `time()` clamps
libVLC's -1 to 0, so the verifier sees position 0 rather
than "unknown" on the VLC path; the guard's
unknown-position reapply branch never fires there —
reapplies still happen through the distance rule. Python
reads the raw time. (2) The verifier runs on the player's
existing 1 s tick, not Python's 200–500 ms guard timer:
budgets and attempt counts are Python's, wall-clock pacing
is coarser (a landing is confirmed up to ~1 s late, the
same order as Task 18's tick-paced readiness note).
(3) The wx MediaCtrl fallback has no seekable report (the
verifier gets None, so a resume there can never learn
unseekable), no output-device selection (M21's dispatch
returns false; the saved choice applies at the next load,
which reads the config), and no referer option — all three
are libVLC-path capabilities, and Python has no non-VLC
path. There is no automated test of a real device switch:
the dispatch is a direct engine call with no branch logic
to exercise headlessly. (4) The worker flushes on its own
cadence rather than Python's UI-timer CallLater; the final
save on window close is queued to the worker, so an abrupt
process exit in the same instant can still lose that last
save — the same exposure as Python's pending flush on a
hard exit. (5) The verifier's remote tolerance is judged
from the raw stream URL, where Python judges the URL its
backend actually loaded (possibly a loopback proxy URL,
hence local tolerance). (6) The plan's manual confirmation
of resume and soundcard behavior with real libVLC remains
Brandon's acceptance — unclaimed here.

**Task 19 integration gate (2026-11-04):** cherry-picked onto
main over 5d2e0e1 with no conflicts. Merged gate: fmt clean,
clippy clean (workspace and core windows-gnu), 691 passed /
0 failed, build OK.

## Audit 8 Task 20 resolution — chapters, row annotations and remaining player controls (M12, M13, M15, G11, G19) (2026-11-04)

M12 (chapter upgrades): the chapter pipeline Python runs in
`core/utils.py::fetch_and_store_chapters` is ported to
`crates/core/src/chapters.rs` — existing rows first, then the
Podcasting 2.0 chapter file (source record in the new
`chapter_sources` table keyed like Python's
`_chapter_source_key`, 900 s freshness window, conditional
GET with the stored ETag/Last-Modified, a bare 304 with no
rows retries once unconditionally, a 304 with rows refreshes
only `checked_at`, mime/body/document validation with the
2 MB cap and `version`+`chapters` shape required), then
YouTube structured chapters, then embedded ID3 chapters.
Normalization is Python's exactly (10,000-entry cap,
`toc is False` skipped, start from the first PRESENT of
startTime/start_time/start, `MM:SS`/`H:MM:SS` string starts
with sub-minute parts under 60, title from `text`, href from
the first non-empty of url/link/href, stable sort,
equal-start dedupe merging missing title/href from later
duplicates). ID3 CHAP parsing rides the `extract.rs` frame
walk (Task 14's prologue handling: extended headers,
unsynchronization, per-frame flags) and reads sub-frames in
Python's WXXX → WOAR → WCOM → WPUB priority. Hosted rows go
to the new `chapter_cache` table (Python's split from the
FK-backed local `chapters` table is preserved). Delivery to
the live player is gated twice: a fresh load or queue entry
with no stored chapters stamps a fetch through the session's
new core `ChapterGate` (per-article stamp; a new activation
or queue switch invalidates it), and the result applies only
while the load generation is current AND the gate still
accepts the stamp for the article the session holds — a
stale queue result is dropped whole. The toggle branch of
`begin_play` now also swaps the chapter set when the
re-activated article's stored chapters differ, closing Task
18's recorded deviation (1); the yt-dlp metadata path
(MediaInfoReady) keeps its own generation check.

M13 (player chapter commands/state): selection is now
preview-only, as in Python — arrowing the chapter list
records a pending index, Enter/double-click commits it
through the 350 ms same-index debounce (core
`chapter_commit_allowed`), the commit seeks through Task
19's `begin_user_seek` guard. Previous/Next follow Python's
active-chapter rules (core `chapter_prev_target` /
`chapter_next_target`): Previous selects the PREVIOUS
chapter (the old restart-current-after-two-seconds behavior
is gone), Previous at the first chapter is a no-op, Next
before the first chapter selects it. Chapter links open
through Python's `_validated_chapter_href` (http/https,
host required, no userinfo, no backslash/whitespace/control
characters) with Python's "Chapter link unavailable"
fallback. The player gains a Chapters menu bar (Show
Chapters Ctrl+M, Open Chapter Link Ctrl+Shift+L, Previous
Ctrl+Shift+Left, Next Ctrl+Shift+Right) whose item
availability follows Python's refresh rule (Previous needs
an earlier chapter, Next a later one, Open Link a valid href
on the selection-or-active chapter), refreshed on the
player's tick only when the triple changes, and a Chapters
button opening Python's dynamic popup: an Open Link item
(enabled only with a valid href), a separator, and one jump
item per chapter with the active one prefixed "[Current] "
— no count cap anywhere. Chapter F1 topics are now Python's
finer set where the controls exist: chapter list and
Chapters button "chapters", speed picker "playback-speed",
seek slider / time readout / volume slider
"player-controls", frame default "player".

M15 (remaining player controls/status): the speed ladder is
Python's `build_playback_speeds` — 36 values, 0.5–4.0 in 0.1
steps (core `playback_speeds`/`speed_index`/`speed_step`);
the Up/Down commands snap the CONFIG value to the nearest
rung and step (an off-ladder saved value moves to its
nearest rung first, Python's rule), and a read-only speed
picker is appended to the player's second button row: a
saved on-ladder value selects its rung, an off-ladder value
shows no selection (Python's combo), picking a rung applies
and persists it. The backend rate readback chain
(`MediaBackend::get_playback_rate`, `VlcPlayer::rate`, the
libVLC get-rate FFI entry) was removed — nothing reads the
rate back any more, the persisted value is the only truth,
as in Python. Absolute volume is Python's slider + percent
readout in a new row appended after the chapter list: the
slider applies to the backend only while Playing (a paused
volume change unpauses VLC — Python's user-reported rule),
always persists `volume`, and the tick re-syncs slider and
readout from the persisted value under a guard so the sync
never re-enters the handler. The position readout is now
Python's time-info text (core `seek_summary`: Elapsed /
Remaining / Total, elapsed-only for unknown length), named
"Playback Time", keeping its "No media loaded" initial text
until a stream exists. The main window's status bar is now
Python's four fields (widths [-2, -1, -1, -1]); field 2
carries the live playback status from the player's new
plain-data `PlaybackSnapshot` (core `live_status_text`:
"Playing: 1:05 / 1:00:00 (58:55 left) — Episode One", empty
with no media), polled by the 150 ms drain, written only on
change via set_status_text — focus never moves.

G11 (row annotations): rows compose Python's cells from a
precomputed cache (`RowAnnotations`): ONE `all_playback_states`
scan plus ONE grouped `chapter_counts` query per render,
reused — merged, not recomputed — for every appended Load
More page; row insertion does no database work. The title
cell is Python's `_get_display_title` (core `display_title`:
time suffix — ", {pos} of {total}", ", played, {total}",
", {total}", ", {pos} played" — plus ", Chapters
available"), the Media cell is Python's
`_article_media_label` (playable-media test ported as core
`article_has_playable_media`, the faithful
`_should_play_in_player` without downloads: yt-dlp-supported
article URL with image extensions rejected, canonical media
type audio/video/podcast, audio-extension enclosures; then
the shared `playback_time_annotation`: duration, "played" /
"played {pos}" / "not played"). State lookup follows
Python's order exactly (`article:{id}` → media_url → url).
The live poll also merges the snapshot's position/duration
into the cache (article key and media-URL key, completed
preserved, snapshot duration wins when known) and rewrites
the playing row's Media and title cells — except the focused
row's title, which Python defers until the selection moves
(NVDA would re-read the whole row); the deferred rewrite
flushes in the selection handler. Queue dialog rows carry
the same shared time annotation, fed by one scan at open.

G19 (>16 chapter links): the fixed 16-slot action table is
deleted. The context menu builds its Chapter Links submenu
from core `article_chapter_links` (Python's
`_article_chapter_links`: unusable hrefs skipped, relative
hrefs resolved with urljoin against the article URL, every
result validated as a web URL, no count cap), and the frame
menu handler resolves the reserved id range 2900–3899
dynamically against the stored link list — chapter 17 and
beyond stay keyboard reachable. The range sits above every
fixed menu id (max 2247) and below the dialog ids (5100+),
so the menu_actions uniqueness rule is untouched.

Tests: `crates/core/tests/audit8_task20_slice.rs` (29 tests,
written regression-first — the red run failed on the missing
APIs): speed ladder shape and rungs (1.1/3.5/4.0 present, 36
entries), off-ladder snapping both directions and at the
ends, chapter position/previous/next/link-index targets,
the commit debounce, href validation (userinfo, backslash,
control characters, hostless URLs — including the WHATWG
`https:///nohost` case Python's urlparse rejects and Rust's
parser accepts, now guarded textually), document
validation, normalization (string starts, toc, dedupe
merge), the fetch pipeline against a scripted transport
(fresh 200 stores rows + source, fresh-window reuse makes
no request, 304 keeps rows and bumps only checked_at, bare
304 retries unconditionally, failing document falls back to
embedded ID3), the gate accepting the current stamp and
refusing stale article/stamp pairs, 25 chapter links all
exposed (the >16 regression), relative link resolution,
annotation composition (title suffix per state, media
label, live status text), and the article playability test.
`media_slice.rs` gains the cache/source/scan roundtrip
against its Python-shaped schema; `media_play.rs` gains an
end-to-end annotation composition test over a real database.
Gate: fmt clean (run last), clippy clean (workspace
--all-targets and core x86_64-pc-windows-gnu, -D warnings),
722 passed / 0 failed (691 at the Task 19 base + 31 new),
`cargo build -p blindrss` OK.

Deviations, named: (1) Local MP4/M4A/M4B embedded chapters
are NOT ported — no MP4 chapter-atom reader exists in the
Rust tree and Python's comes from mutagen; the ID3,
Podcasting 2.0 and YouTube paths are ported, and a local
MP4 file simply yields no embedded chapters. (2) ID3 CHAP
sub-frames are read raw: per-sub-frame
compression/encryption flags are not applied (top-level
frames get the full flag handling through the shared walk).
(3) The YouTube chapter branch uses the yt-dlp info path
(`chapters_from_info`: info chapters, else the description
grammar) — Python's watch-next first source is not ported
anywhere in the Rust tree (inherited from the ytfull port).
(4) The chapter-document transport rides the app's ureq
agent with scheme/document validation; Python's
per-redirect-hop public-HTTP/SSRF validator is not
replicated. (5) The player menu bar carries the Chapters
menu only: Python's Playback menu holds only Cast, the Rust
player has no cast control (casting is a main-window
surface), and the "casting" F1 topic therefore remains
unassigned in the player. (6) Chapters-menu availability and
the snapshot refresh on the player's existing 1 s tick;
Python refreshes its controls on a finer UI timer. (7) The
player position readout's accessible name is aligned to
Python's "Playback Time" (was "Position"), its initial text
to "No media loaded", and the chapter list's row labels to
Python's chapter menu-label format ("{timestamp} {title}").
(8) Queue rows gain the shared time annotation but keep the
pre-existing "{title} - {feed}" base format; Python's
"{index}. {title}, from {source}" base was not adopted —
outside G11's letter. (9) The snapshot's state verb is
derived from the backend state each tick (Playing/Paused),
with load/failure/stop texts set at their transitions;
Python passes its status-line text through verbatim.
(10) `replace_chapters`/`replace_cached_chapters` keep the
existing non-transactional DELETE+INSERT (Python wraps in a
SAVEPOINT) — pre-existing Rust behavior, unchanged here.
Owner screen-reader acceptance (chapter flows, speed/volume
controls, live status feel, row annotation wording under
NVDA) remains Brandon's — unclaimed here.

**Task 20 integration gate (2026-11-04):** cherry-picked onto
main over fa81968 with no conflicts. Merged gate: fmt clean,
clippy clean (workspace and core windows-gnu), 722 passed /
0 failed, build OK.

**Miniflux worker-cap flake hardened (2026-11-04):** the
`worker_configuration_has_a_hard_cap_of_thirty_two` test
flaked again on the macOS CI leg (peak 31 of 32) after the
2026-11-02 delay widening. The test server now holds each
targeted request at a barrier until all 32 workers are in
flight before the delay starts, so the peak no longer
depends on the entry ramp; a 10 s deadline keeps a genuinely
lower cap failing the assertion instead of hanging. Five
consecutive local suite runs pass. Test-only change; no
production code touched.
## Audit 8 Task 21 resolution — incremental, cancellable silence skipping (M09, S17) (2026-11-04)

M09 — the silence subsystem is now a port of Python's, not a
substitute. The pre-task state blocked on whole-source
ffmpeg `silencedetect` output, started immediately at load
with no cancellation, took no headers, and SHRANK ranges
with padding where Python EXPANDS them.

- Core scanner (`core/audio_silence.py` →
  `crates/core/src/silence.rs`, rewritten): `scan_silence`
  spawns ffmpeg with Python's argument shape (`-nostdin
  -hide_banner -loglevel error`, `-threads` when
  configured, headers for http(s) sources only — a
  User-Agent header becomes `-user_agent`, the rest one
  CRLF `-headers` block; with no headers the app UA from
  core's user_agents policy is passed, NOT Python's
  hardcoded Chrome/91 string, per Operational Mandate 1)
  and streams mono s16le PCM in 64 KB reads. Pacing: with a
  lead configured (`silence_scan_lead_seconds`, default
  300 s), reads pause while the scanned position is more
  than the lead ahead of the playback-position provider;
  ffmpeg blocks on its full stdout pipe, as in Python.
  Cancellation: an AtomicBool checked before every read and
  during pacing sleeps; on abort the child is killed and
  reaped and the result is `ScanError::Cancelled` — no
  ffmpeg survives (heartbeat-file regression test). stderr
  is drained concurrently into a capped shared buffer
  (Python drains after the loop; concurrent drain cannot
  stall a chatty ffmpeg on a full pipe, and the drain
  thread is never joined on an error path — a killed
  child's own children can hold the pipe open). A nonzero
  exit yields `ScanError::Ffmpeg` with the stderr tail, the
  source URL redacted inside it first. Spawn retries
  ETXTBSY with `dependency::spawn_tool`'s backoff
  discipline; the scanner owns its child because
  `dependency::run_tool` captures capped whole-run output
  and cannot stream PCM.
- Detectors: `VadSilenceDetector` ports
  `_detect_vad_ranges` (frame 10/20/30 with 30 coercion,
  aggressiveness clamped 0..=3, a frame is silent iff the
  VAD says not-speech AND the frame RMS is at or below the
  threshold, runs of at least `min_silence_ms`, gap-merged
  return). `PcmSilenceDetector` ports
  `StreamingSilenceDetector` exactly, including its
  quirks: windows are consumed whole, a trailing partial
  window is dropped at finalize, min windows =
  ceil(min/window), adjacency-only merge. The millisecond
  helpers `merge_ranges_ms` / `merge_ranges_with_gap_ms`
  port `merge_ranges` (+1 ms adjacency) and
  `merge_ranges_with_gap`; `pad_and_merge_ms` ports the
  player's publish transform, and `finalize_ranges` now
  routes through them (padding expands).
- VAD disposition — PORTED, with evidence (quantified
  research protocol, 2026-11-04): Python's primary
  detector is the WebRTC VAD via the `_webrtcvad` C
  extension (`core/webrtcvad_compat.py` over
  webrtcvad-wheels). The Rust `webrtc-vad` crate 0.4.0
  (crates.io; github.com/kaegi/webrtc-vad, MIT) wraps
  libfvad — the WebRTC VAD C library, the same algorithm
  and codebase — compiled with the `cc` build dependency
  (bindings pre-generated, no bindgen at build time). It
  is now a normal core dependency, used exactly where
  Python uses its VAD: primary when the scan rate is one
  of 8000/16000/32000/48000, with the RMS detector as the
  fallback when the VAD cannot serve the rate — the same
  fallback Python takes when `webrtcvad` is unavailable.
  RMS is never labelled equivalent to VAD. Alternatives
  rejected with reasons: Silero-based crates
  (`voice_activity_detector`, `silero-vad-rust`) are a
  different detector, not parity; `wavekat-vad` adds an
  abstraction layer over the same code. GitHub
  most-recent/most-stars searches, Codeberg and GitLab
  were searched for the same claim; the crates.io crate
  above is the direct libfvad binding. Deterministic
  fixtures pin the boundary: the VAD fixture separates a
  voiced harmonic stack from silence, and the RMS
  fixtures pin level-only behaviour (a −50 dBFS tone
  counts as silence under a −38 dB threshold).
- Settings resolution: `SilenceScanSettings::from_config`
  mirrors the player worker's config reads with
  `core/config.py` DEFAULT_CONFIG values as the effective
  defaults (Python's loader merges them into every
  config; the worker's own literal fallbacks fire only
  for missing/falsy keys) and applies Python's remote
  adjustments verbatim: remote = http(s) and not loopback
  → decode rate `silence_scan_remote_sample_rate` (8000),
  aggressiveness forced to 0, threshold capped at
  −52 dB, minimum silence at least 1200 ms, merge gap at
  most 200 ms, threads 1 (2 local), low priority default
  on, lead 300 s.
- Player lifecycle (app/src/player.rs): the scan is
  QUEUED at deliver and at queue advance
  (`queue_silence_scan`), STARTED by the 1 s tick only
  while Playing and after `silence_scan_delay_s`
  (4.0 s default) has passed since the queue, and only
  while its load generation is current. Progress sets
  publish through `pad_and_merge_ms` — padding EXPANDS,
  `(max(0, start − pad), end + pad)` — and mark the scan
  ready incrementally, so a paced scan is usable
  mid-stream; a progress set arriving for a stale
  generation aborts the scan (Python's publish gate). A
  failed scan keeps any progress-published ranges; with
  none, ready stays false (Python's rule).
  `cancel_silence_scan` runs on every new load
  (begin_load), queue advance, Stop and window close:
  abort flag set, channel dropped, ranges, ready flag
  and the whole skip policy cleared.
- Skip policy (core `SilenceSkipPolicy`, clock-injected;
  the tick drives it in Python's order — landing
  verification, floor ratchet, evaluation). Guards in
  Python's order: casting (vacuously false — the Rust
  player has no casting state), seekable `Some(false)`
  blocks while `None` proceeds, an armed resume verifier
  blocks, not-ready/empty blocks, the user-seek pause
  (1.2 s local / 2.5 s remote), slider dragging,
  position + 500 ms behind the floor, and the post-skip
  cooldown (2.5 s local / 5.0 s remote). Hit arithmetic:
  break at pos < start − 1000; containment
  start − 100 ≤ pos ≤ end − 100; target = end +
  resume_backoff (remote-capped at 1000 ms); floor
  refusal; the ±1000 ms same-target loop guard;
  retrigger cleanups. Landing verification re-seeks ONCE
  when playback is still short past 700 ms local /
  1200 ms remote after the 0.35 s / 0.5 s minimum wait,
  inside a 3 s / 5 s window; a user seek cancels it. The
  floor ratchets to max(floor, recent app-seek target
  within 8 s, position), resets to the position after a
  user seek, and follows the position down inside the
  3 s allow-backwards window a seek landing more than
  1200 ms behind opens. Skip seeks go straight to the
  backend and do NOT enter Task 19's user-seek verifier
  (the settled Task 19 split); the policy's own landing
  verification replaces it. Silence no longer rides the
  SponsorBlock skip channel or its hit test; the
  SponsorBlock flow is unchanged.
- Progress delivery on the RMS fallback: Python delivers
  progress only on its VAD path; the Rust scanner
  delivers it on both — a deliberate superset the plan's
  incremental requirement asks for.

S17 — Settings gains Python's `range_cache_debug` toggle:
a "Verbose range-cache proxy debug logs" checkbox on the
Media Player page (appended at the page end — deviation
(1) below), value from config (default false), saved
beside `skip_silence`. The settings action in main.rs
retunes the live proxy when the key changes (Python's
mainframe behaviour; gated loads also retune as before).
The runtime now consumes `RangeCacheConfig.debug`
(previously read from config but never used): the origin
probe emits one diagnostic line — redacted URL, redacted
final URL, status, accept-ranges, content-range,
content-type, content-length — and
`rangecache::redact_url_for_log` ports Python's
`core/utils.py` redactor exactly (credentials and queries
stripped, googlevideo paths collapse to
`/<signed-media-path>`, a `/<token>/proxy|file|transcode`
path collapses to `/<capability>/<kind>`, scheme/host-less
input is `<local-or-invalid-url>`). No headers are ever
logged. Persistence and redaction are pinned by test.

Tests (15 new, regression-first: the red run failed to
compile on the missing APIs, and the two pre-existing
finalize tests pinned the wrong shrink semantics before
being corrected to Python's expansion): audit7_slice
gains merge/pad arithmetic, both PCM detector fixtures
(including the finalize quirks), the VAD fixture, four
fake-ffmpeg scanner tests (incremental progress, headers
only for http sources, pacing hold + abort release,
cancellation leaves no surviving ffmpeg by heartbeat),
five skip-policy tests (guards, hit arithmetic, cooldown
and user-seek pause, floor and loop guard, landing
verification) and the redaction/persistence test;
batch3_slice's finalize expectations updated to
expansion. Gate: fmt clean (run last), clippy clean
(workspace --all-targets and core
x86_64-pc-windows-gnu, -D warnings), 737 passed /
0 failed (722 at the Task 20 base + 15 new),
`cargo build -p blindrss` OK.

Deviations, named: (1) The S17 checkbox is appended at
the END of the Media Player page instead of Python's
mid-page position after the VLC network-cache control —
the append-only settings rule (tab order is screen-
reader muscle memory); parity of function, not position.
(2) Slider dragging has no true drag-start/end events in
wxDragon: user slider events mark dragging and the tick
clears it after 600 ms without one. (3) Low priority:
Windows gets Python's CREATE_NO_WINDOW |
BELOW_NORMAL_PRIORITY_CLASS creation flags; on Unix the
scan does not renice (core carries no libc dependency) —
pacing already throttles the scan's CPU to the lead
window. (4) The remote judgement uses the resolved
stream URL handed to the player (Python uses its
current_url); loopback range-cache URLs read local
under both. (5) The skip policy runs on the player's
1 s tick; Python evaluates on a finer UI timer and arms
250–4000 ms diagnostic seek probes — the probes are
diagnostic-only in Python and are not ported; budgets,
windows and thresholds are Python's, the pacing is
coarser (the same class as Task 19's verifier
deviation). (6) ffmpeg stderr is drained concurrently
rather than after the loop (above): content and use are
Python's, the timing differs. Owner listening checks
(VAD versus RMS feel, skip landing accuracy on real
episodes, remote-stream conservatism) remain Brandon's
— unclaimed here.

**Task 21 integration gate (2026-11-04):** cherry-picked onto
main over 37f8ae9; the only conflict was the append-only
PARITY region (union). Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 737 passed / 0 failed,
build OK.

**Silence progress assertion corrected (2026-11-04):** the
Task 21 test `scan_completes_and_reports_progress_incrementally`
failed on the macOS CI leg: the last progress snapshot held
only the first range. The trailing range ends exactly at
EOF, so it closes only at finalize, and pipe chunking
decides whether a cadence tick lands after the close — the
assertion "last progress equals the final set" encoded
timing as contract. It now asserts the real contract: every
progress set is a prefix of the final set (progress
converges, never contradicts). Production code unchanged;
the player consumes the scan's return value, not progress,
for the final ranges.
## Audit 8 Task 22 resolution — range recovery, playback cache and download parity (M03, M04, M16, M17, M18, M19, M20, S12) (2026-11-04)

**M03 (refused probes poisoned the session) — fixed.**
`rangecache`'s probe now ports Python
`core/range_cache_proxy.py`: a 401/403 probe response is
retried once after 1 s; a refused probe leaves range support
UNKNOWN (never latched `Some(false)`), only 206/200/other
statuses or a transport error settle it; `await_probe`
treats "probe done but support unresolved" as not settled
and probes again inline, so a 403-then-206 origin recovers
and repeated refusals degrade to passthrough for that read,
never for the session. Regression tests (batch36_slice):
`probe_403_then_206_recovers`,
`probe_refusal_never_permanently_disables_cache`.

**M04 (background ranges unbounded) — fixed.** Background
range requests are capped at Python's
`_BG_MAX_REQUEST_BYTES` = 10 MiB per request
(`background_request_target` clamps the bootstrap target
and the steady-state chunk alike); filling continues
incrementally to the configured depth. Tests: the pure
helper test plus the in-module loopback test
`background_download_never_exceeds_ten_mib_requests`
(25 MiB origin, 64 MiB bootstrap / 32 MiB chunk configured;
every observed origin range ≤ 10 MiB, largest exactly
10 MiB).

**M16 (shared bounded yt-dlp recovery) — fixed.** New core
module `ytdlp_recovery.rs` ports Python's policy from
`gui/player.py` + `core/discovery.py` and is shared by
stream resolution (`media::stream_url`), downloads
(`download::download_with_ytdlp`) and the playback cache
(`play_cache` fetch): Python's format selectors
(`bestaudio[ext=m4a]/…` VLC audio, the SoundCloud
progressive-MP3 selector), the primary client pool
(`default,android_vr`) and the wider fallback pool, radio-mix
URL normalization, the music/nocookie recovery URLs tried
with `--impersonate chrome`, cookie attempt order (anonymous
base, existing cookie file, browser sources; cookie file
FIRST for Bloomberg hosts), the uncookieable-error rule
(YouTube page URLs and cookie/login wording excluded;
"please report this issue" / "unsupported url" skip all
browser attempts), and DPAPI handling (a DPAPI failure skips
later Chromium sources, Firefox continues; the final error
carries Python's App-Bound Encryption explanation verbatim).
Timeouts are fatal and stop all phases (900 s download /
1800 s merged-download / 30 s CLI stream shape); the
reported error is the anonymous base attempt's when there
is one. All invocations go through Task 23's
`dependency::run_tool` — the detach-pattern `Command`
callers in download.rs and play_cache.rs are converted, as
assigned by Task 23. The pre-resolution watchdog timing bug
the finding warns about is not present in Rust and was not
reintroduced: recovery here is attempt-sequenced, the
player's stall watchdog (Tasks 18/19) is untouched.
Deviations: (1) Python's pytubefix phase and hidden-browser
visitor-data bootstrap phase are NOT ported — no Rust
analogues exist; phases end at the impersonated
recovery-URL phase. (Half-superseded v0.1.82: the
hidden-browser bootstrap IS ported now —
youtube_session.rs plus the final recovery phase — and
only the pytubefix client phase remains unported. See
the closeout resolution at the end of this file.) (2) Recovery-URL phases also try the
cookie file (Python's play-cache tries base only there; its
download worker tries both — the shared runner follows the
download shape). (3) Stream resolution keeps the Rust CLI
`-g` shape rather than Python's `--dump-single-json` parse;
both ask yt-dlp for the chosen format's URL with the same
selector, client pool, referer and UA. Tests: media_play
policy unit test plus fake-yt-dlp end-to-end tests (fallback
pool reached, cookie-file attempt, uncookieable skip,
DPAPI Chromium skip, impersonated recovery URLs).

**M17 (playback-download cache) — fixed.** `play_cache`
now matches Python `core/play_cache.py`: the default
directory is `ytplay_cache` beside the data dir (was
`cache/play`), identity is the YouTube media ID (file
`%(id)s.%(ext)s`) instead of a whole-URL FNV hash, so every
URL form of a video (watch / youtu.be / music / shorts)
hits one file and a cache copied from the Python app is
reused offline unchanged. `.part`/`.ytdl`/`.tmp` sidecars
are excluded from playback hits, size/count accounting and
pruning; `clear_cache` removes everything and returns
(removed, freed) like Python. Downloads carry the cookie
file, the configured user agent and `--ffmpeg-location`.
Tests: download_parity play-cache layout/accounting tests
(default layout, sidecar accounting, offline reuse across
URL forms, prune evicts oldest completed only, clear
returns (3, 1050)) and a fake-yt-dlp fetch test (media-ID
file created through run_tool; second fetch is a hit with
exactly one invocation). Deviation: fetching a non-YouTube
URL returns an error (no video ID); the only caller path is
YouTube-gated in the app.

**M18 (download retry/format recovery) — fixed.**
`download_with_ytdlp` runs the M16 phase machine; per
attempt, video formats try MP4 merge first and retry ONCE
as MKV only when the error contains "conversion failed";
audio presets never pass a merge format and never retry it
(Python `core/download_formats.py` + the mainframe loop).
The output template is Python's `<safe article
title>.%(ext)s`. Tests: fake-yt-dlp MP4-failure→MKV retry
(exactly 2 invocations), audio preset never merges, cookie
file honored as the second attempt.

**M19 (download layout, legacy lookup, MIME, retention) —
fixed.** Core gains exact Python ports: `safe_download_name`
(invalid-run collapse, trailing-dot strip, 120-char cap,
"untitled"), `download_dir_for` (feed-title folder,
`download_folder` override wins, "Feed" fallback),
`extension_for_download` (URL extension ≤ 5 chars wins,
else the response MIME table — never `.bin`; unknown types
default `.mp3`), `find_downloaded_file` (prefix match,
`.temp.`/sidecar exclusion, newest wins) and retention
helpers (per-folder `apply_download_retention`; root +
first-level-subfolder `sweep_download_retention`).
`download_direct` requests first, then names from
URL/MIME with Python's `_unique_path` numbering
(`name-1.ext`, …) — no more early `.bin` or UUID suffixes.
The app now downloads into per-feed folders (pasted-link
items with no feed go to Python's "Media Links" folder),
applies retention to the folder after each successful
download, sweeps root + subfolders at refresh, and a
completed download on disk (index OR legacy scan) wins over
the resolved stream in the main window's delivery, in the
player's queue advance, and at both activation checks.
Deviations: (1) the core legacy scan covers the download
root + every first-level subfolder — a superset of Python's
single per-feed-folder lookup, because core does not know
the article's feed title; the exact single-folder port
exists as `find_downloaded_file` + `download_dir_for`.
(2) The Rust `Article` has no `download_folder` field, so
the override is approximated by the "Media Links" rule for
feed-less articles. (3) `download_direct` keeps the
partial-file + hard-link publish (Python writes the final
name directly); on a hard-link race it advances the
`_unique_path` counter instead of erroring. (4) The podcast
archive dialog keeps its own chosen download directory
while calling the new signature — Python's archive worker
routes through the standard feed-folder path with a folder
override; recorded as an adjacent find, not changed here
(dialogs.rs is outside this task's file scope beyond the
signature fix).

**M20 (original/resolved URL context) — fixed.** The
player now computes Python's context via the new core
`media::range_cache_context` +
`should_resolve_direct_media_url` (exact ports: the
13-extension seekable list, the 11-host podcast-tracker
list): a resolved direct-media URL registers its ORIGINAL
URL with the range-cache gate, so expired signed redirects
refresh through the original and reuse cached segments;
yt-dlp resolutions and unresolved plays carry no context.
The gate side already supported this (Task 21 era); the
player previously passed `false, None`. The context is
stored on the player state, so range-cache toggle reloads
reuse it, and queue advance goes through the same delivery.
Deviation: the player infers "via yt-dlp" from the input
URL (`is_ytdlp_supported && !is_media_file_url`) because
`MediaResolution` carries no via-yt-dlp flag to the player;
a yt-dlp-supported page URL suppresses context, a direct or
tracker URL carries it. Tests: media_play context/resolve
gate test; batch36 gate test asserting the session id is
the ORIGINAL URL's hash, never the signed URL's.

**S12 (YouTube settings) — fixed.** The Settings YouTube
page gains Python's missing controls, APPENDED after the
existing controls (settings order is never rearranged):
"Automatically import browser cookies.txt exports from
Downloads" (`auto_import_browser_cookies`, default on),
"Play YouTube by downloading first (most reliable; slower
to start)" (`youtube_play_via_download`), the "YouTube
playback cache folder" field, "Max cache size (MB,
0 = unlimited):" (`youtube_play_cache_max_mb`, default 500)
and a "Clear cache now" button. Clearing follows Python's
`_on_clear_play_cache`: scoped to the field's current value
(or the resolved default beside the data dir), counted
first, an already-empty info box when there is nothing, a
Yes/No confirm with count + human size + folder, then a
"Removed N file(s), freed SIZE." report. All four keys save
to config.json in Python's shapes, and their consumers
(startup cookie import, download-first playback, cache
pruning) already read them. Deviations: the max-size value
is a text field, matching this dialog's other numeric
fields, where Python uses a spin control; the cache-folder
row has no Browse button (the field is directly editable);
placement is appended rather than Python's interleaved
order, per the append-only rule. There is no headless GUI
test for the round-trip — the controls use the same
get/set save pattern as the existing YouTube keys; the
NVDA pass over the new controls joins Brandon's acceptance
list.

**Robustness fix found by the new tests (range cache):**
the M03 loopback test flaked ~1 run in 3 with an empty 206
body. Root cause (traced with origin request logs and
temporary fetch-path instrumentation, both removed): the
entry's segment directory is created at entry construction,
but the cache directory is shared with the Python app
(dual-run) — an external cleanup can remove an entry's
directory under the live entry, and the fetch paths'
`File::create(tmp)` then failed ENOENT, aborting the serve
after headers were sent. Both fetch paths
(`fetch_chunk` and `stream_origin_range_to_and_cache`) now
re-ensure the entry directory before creating the temp
file. The batch36 origin test servers also read through
end-of-headers now (a single `read()` could return a split
request and lose the Range line). After the fix the suite
ran 8/8 clean (previously ~1-in-3 failures).

**Task 22 gate (2026-11-04):** fmt clean (run last), clippy
clean (workspace `--all-targets -D warnings` and core
windows-gnu), **761 passed / 0 failed** (baseline 737 on
this tree; +24: batch36 +4, media_play +7, download_parity
+11, play_cache in-module +1, rangecache in-module +1),
`cargo build -p blindrss` OK.

**Task 22 integration gate (2026-11-04):** cherry-picked onto
main over 8192b29; the only conflict was the append-only
PARITY region (union). Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 761 passed / 0 failed,
build OK.

## Audit 8 Task 16 resolution — one YouTube document/subtitle pipeline (R10, R11, R12) (2026-11-04)

**R10 (incomplete comments had no recovery or warning) — fixed.**
There is now ONE info pipeline, `ytfull::extract_video_info`
(Python `extract_video_info` with comments), and every consumer
runs on it: the plain reader (`extract.rs`), the rich reader
(`article_html.rs`), the main-window full-text path
(`download::youtube_full_text_*`), and the chapter fetcher's
info source (`media::youtube_article_info` now delegates to it;
`chapters.rs` reads chapters from the same info as before). One
run is Python's `_extract_info_via_cli` shape — `-J`,
`--write-comments`, the primary client pool plus
`max_comments=all,all,all,all,all`, the radio-mix-normalized
URL, the app's cookie/UA env args — executed through the owned
subprocess policy (`dependency::run_tool_full_output`, the new
uncapped variant: a comment-complete dump legitimately exceeds
the shared 1 MiB cap and Python caps nothing there; timeout,
kill-and-reap and pipe draining are unchanged) with Python's
900 s with-comments budget. A SUCCESSFUL run whose stderr
contains "incomplete data received" is retried once; the retry's
info wins as the document source with its comment list replaced
by the union of both runs' comments (first run's order, deduped
by comment id; an id-less comment keys on its canonical
sorted-keys JSON, Python's `json.dumps(sort_keys=True)`
fallback). If the retry still reports incomplete data — or
fails outright, keeping the first extraction — the info carries
`_blindrss_comments_incomplete`, and `article_fields_from_info`
prints Python's notice verbatim ("YouTube returned an incomplete
comment page after retry; all comments received are shown
below.") directly under the Comments heading. Regression tests
(batch35b_slice, fake yt-dlp via the `extract_article_with`
seam): union across the retry with no notice, notice after two
incomplete runs, retry failure keeps the first extraction with
notice, a complete run never retries; plus a pure test pinning
the notice's position. The main-window path
(`download_parity.rs`): `youtube_full_text_with` produces the
shared document with the retry applied, rejects non-video URLs
without invoking yt-dlp, and surfaces a failed extraction as an
error. The app's full-text worker now gates on
`ytfull::is_youtube_video_url` instead of a substring match.

**R11 (subtitle requests diverged between readers and from
Python) — fixed.** One shared downloader,
`ytfull::transcript_from_info` (Python `transcript_from_info`),
serves every path: candidates from the one
`subtitle_candidates` (whose protocol filter now matches
Python's truthiness — a `"protocol": ""` format is a file, a
`"live_chat"` entry is not), deduplicated by URL, at most two
tracks attempted, the info's extracted `http_headers` forwarded
on every request, and failure handling by HTTP status only (the
error text carries the signed URL and is never surfaced): a 429
on an untranslated track stops the walk, a translated track's
429 falls through. A json3 body that does not parse counts as a
failed attempt, as in Python where the parse lives inside the
try. Tests (batch35b_slice, loopback server recording paths and
headers): header-protected track fetched with the extracted
headers; duplicate URLs requested once; two-attempt cap with
best-first order; untranslated-429 stop after exactly one
request; translated-429 continues to the next track.

**R12 (rich YouTube fallback could show page chrome or replace
rebuilt text) — fixed.** `render_rich_article` computes Python's
`structured_youtube` identity once (non-media YouTube video URL)
and retains it through failure: the generic page fetch is
skipped for such URLs (a failed reconstruction now falls to the
cleaned feed fallback, never the cleaned watch page), and the
feed-length arbitration excludes structured YouTube exactly as
Python's `not structured_youtube` guard does (GitHub's exclusion
unchanged). Tests (batch35b_slice, via the
`render_rich_article_with_ytdlp` seam): a reconstruction
survives a far longer feed body (feed text absent, extraction
title wins); a failed reconstruction renders the feed fallback
and renders nothing at all without one.

**Removed second implementation.** `download.rs`'s divergent
YouTube document builder (12 subtitle attempts with no headers
and no 429 stop, its own candidate ranking, rolling-caption
VTT flattening, its own comment formatter) is deleted, along
with `compose_youtube_text`; `youtube_full_text*` delegates to
the shared pipeline. Its three in-module tests and the two
`media2_slice` tests that pinned the deleted helpers
(`vtt_rolling_dedupe`, `sanitize_and_compose` — the sanitize
assertions survive as `sanitize_filename_shapes`) were removed
with it; the behaviors that remain part of the product are
covered by the Task 16 tests above. Test totals: 761 baseline →
771 (batch35b +12, download_parity +2, removed divergent-helper
tests −4 net).

**Deviations, stated plainly.** (1) Python's extraction has an
in-process yt_dlp fallback for checkouts with no CLI; Rust is
CLI-only by the standing wrapped-executable rule, as before.
(2) Python's `cancel_when` thread-local lets selection changes
kill an in-flight CLI run; the Rust app relies on its existing
generation guards at the call sites (unchanged by this task).
(3) Python's chapter memo (`_remember_chapters`) is not ported;
chapter lookups already cache through `chapters.rs`' own store.
(4) The shared CLI run now carries the app's cookie/UA env
args, which the old `ytfull` path lacked and the old
`media::youtube_article_info` had — a superset of Python's CLI
invocation, kept so cookie-required videos keep working in the
reader paths as they already did in the main-window path.
(5) `media::youtube_article_info`'s former `--no-warnings` flag
is gone with the rest of its private invocation: Python's CLI
path does not pass it, and the incomplete-data signal R10 keys
on travels as a warning.

**Task 16 integration gate (2026-11-04):** cherry-picked
clean onto main. Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 771 passed / 0 failed,
build OK. Wave 3 is code-complete.

**Wave 3 release v0.1.57 (2026-11-04):** version bumped
0.1.56 -> 0.1.57; cargo update applied (cc 1.11.0,
ureq 3.5.1, ureq-proto 0.7.1; 24 packages already latest).
Release-tree gate re-run on the updated tree: fmt clean,
clippy clean both legs, 771 passed / 0 failed, build OK.

## Audit 8 Task 25 resolution — consented cookie identity/import correctness (B03, B04, B05, B06, B07, B08, B12, B17) (2026-11-04)

Regression-first: the four jar reproductions (oversize first
cookie, sibling path leakage, duplicate names, leading-dot
domains) were added to `cookies_slice.rs` and failed against
the shipped code before any fix. New suite
`audit8_task25_slice.rs` (11 tests) covers the rest against
synthetic fixture profiles; no live browser or real cookie
value is involved anywhere.

**B03 (Firefox imports).** New core module `firefox.rs` ports
Python's Firefox-family reading: per-OS roots for Firefox,
LibreWolf, Waterfox, Floorp, Zen, Pale Moon and Thunderbird
(including the Flatpak and Snap Linux layouts), both profile
layouts (`<root>/<profile>` and `<root>/Profiles/<profile>`),
dedupe by profile directory, newest DB first. Reads copy
`cookies.sqlite` plus its WAL sidecars into a unique
per-operation temp directory before querying `moz_cookies`.
`profile_user_agent` derives the profile's UA from
`compatibility.ini` exactly as Python does. The watcher
(`cookies_import::auto_import_browser_profiles`, now factored
over explicit profile lists for tests) runs the Firefox pass
under the SAME consent model as Chromium: the automatic pass
takes clearance-shaped cookies only — across all changed
profiles the longest-lived candidate per (domain, path, name)
wins and its profile's UA is pinned to the cookie's host — and
full session records move only in the consented full mode,
which uses the separate `site_cookies_full_profile_mtimes`
marker map, as Python does. A failed read never advances a
marker. The Import Site Cookies dialog gained an "Import from
Firefox..." action (single profile imports directly; several
get a chooser), so the Firefox support the Settings consent
text has always advertised is now real on both paths.

**B04 (YouTube sessions reach yt-dlp).**
`cookies_import::merge_youtube_cookies` ports Python's
`merge_youtube_cookies`: permitted YouTube/Google records from
a consented full import also land in the managed
`youtube_cookies.txt` beside the data dir. The managed file is
rewritten from YouTube/Google records only (anything else in
it is dropped, as Python drops it), carries the managed-jar
header, and the `ytdlp_cookies_file` setting is pointed at it
only when unset or already pointing there — a file the user
configured is never repointed. Wired into the watcher's full
mode (both browser families) and into both explicit dialog
imports. Nothing merges on the automatic clearance pass; the
consent boundary is unchanged.

**B05 (clearance identity).** `cookies.rs` now carries the
identity half of Python's `site_cookies.py`: the global UA
sidecar (`site_cookies_ua.txt`, removed when cleared) and the
per-host UA map (`site_cookies_ua_hosts.json`) with
longest-suffix host matching. `user_agent_for` follows Python:
the host-pinned UA applies when the jar actually sends cookies
for the URL; the global UA applies only beside clearance;
anything else keeps the caller's default identity.
`forget_clearance_for` removes ONLY the clearance cookies
matching the URL's host (jar rewritten via temp file + rename)
plus the host UAs pinned for matching domains — ordinary
session cookies are never touched. Targeted recovery is
ported with Python's shape and 20-second per-key rate limits:
`refresh_groups_io_cookies` / `refresh_reddit_cookies`
(host-gated, best-login-profile-wins-whole scoring, only that
site's cookies merged, UA pinned to `groups.io` / `reddit.com`)
and `refresh_clearance_from_browsers` (marker-free re-harvest,
returns whether the URL's cookies changed). The feed fetcher
(`feeds.rs::request`) now sends cookies together with
`user_agent_for`'s UA and that identity's client hints.
`browser.rs::save_cookies` now routes through
`cookies::record_browser_session`: a solved challenge stores
ONLY its clearance-shaped cookies for the request host
(previously every cookie the browser returned was merged —
a consent gap against Python), session cookies get Python's
one-hour expiry, and the UA the browser ran with is pinned to
the request host. The #[ignore]d live browser test's fixture
cookie was renamed `clearance` -> `cf_clearance` to match the
corrected semantics.

**B06 (budget).** `cookie_header_for` is now Python's
pipeline: `cookies_for` (insertion-ordered name->value map —
a later duplicate replaces the value, keeps the first
position) then `_trim_cookies_to_budget`: when the rendered
header would exceed the 8,000-byte budget, selection prefers
clearance cookies first, then smaller entries, SKIPPING an
entry that does not fit instead of stopping at it; kept
cookies render in their original relative order. The old
walker stopped at the first oversize cookie and dropped every
cookie after it, clearance included.

**B07 (path matching).** A stored cookie path matches the
request path exactly or at a slash boundary (`/private` and
`/private/child` match, `/private-other` does not), with
Python's trailing-slash handling; root/empty paths still match
everywhere. Jar parsing now also treats a leading dot in the
stored domain as include-subdomains on its own (Python
`_parse_jar`), whatever the flag column says.

**B08 (manual import + UA editing).** The dialog appends
Python's section after the shipped controls (append-only; the
existing tab order is untouched; the two text fields carry the
accessible names "Cookies file path" and "Browser
User-Agent"): a cookies-file field with Browse, the Browser
User-Agent field prefilled with the stored global UA (else
the app's current UA), and an "Import File / Save User-Agent"
action with Python's OK semantics — an empty file field saves
the UA alone; an invalid file is an error and NOTHING is
saved, jar or UA. Core side: `validate_site_jar_file` (any
domains; Python's messages) and `merge_jar_file` in
`cookies_import.rs`. This is also the working fallback for
cookies no direct reader can decrypt (see B17).

**B12 (temp directories).** `chromium::read_cookie_db` uses a
unique per-operation directory
(`blindrss-chromium-cookies-<pid>-<uuid>`), replacing the
PID-only directory whose cleanup let one in-process reader
delete another's copy mid-read; the Firefox reader uses the
same discipline. The database copy itself now retries up to
three times with 150 ms pauses (Python `_copy_with_retries`),
the non-privileged way to read a live profile whose browser
holds the file only briefly. A regression test runs two
same-process reads of distinct fixture DBs concurrently and
asserts no temp directories are left behind.

**B17 (v20 honesty) — where the line fell, exactly.**
Everything in Python's recovery that needs NO privilege is
now ported: the copy-with-retries read of a locked database
(above) and the exported-jar fallback (B08, now solid). The
part that remains unported is only the privileged master-key
resolution — Python's elevated helper path (SYSTEM-level key
access / CNG / snapshot of locked files) that v20 app-bound
cookies require. No privileged or elevated helper was built
in this task, per the plan's reservation of that for a
separate design and security review with explicit consent.
To keep the gap honest rather than silent, `ReadOutcome` now
counts v20-prefixed rows separately (`skipped_v20`), and the
Import dialog's result names them: the message states how
many cookies use Chrome's app-bound (v20) encryption, that
they cannot be read directly, and that the file import below
is the way in. The batch9c round-trip test's skip accounting
was updated to the split count.

**Deviations, stated plainly.** (1) Python's Firefox-button
dialog import does not merge YouTube records into yt-dlp's
jar (only its watcher full harvest and Chromium import do);
the Rust dialog's Firefox import DOES merge them, as its
Chromium import does — a deliberate superset confined to
explicit, user-initiated full imports. (2) Python's clearance
watcher is Firefox-only; Rust's pre-existing Chromium
clearance pass (shipped v0.1.22) is retained unchanged, and
UA pinning is applied where Python pins (Firefox clearance
pass; both families' consented full imports; dialog imports).
(3) The Settings consent label keeps Python's exact wording;
the honest v20 diagnostic lives in the Import dialog's result
text rather than in a divergent settings label. (4) Python's
`cookies_for` ignores the secure flag; the Rust jar's
pre-existing secure-over-http filtering is kept (not a Task
25 finding). (5) The recovery functions are ported and tested
in core, but their call sites inside Python's extraction
fallback chain (`article_extractor` retry / `groups_io`) are
NOT rewired here: `extract.rs` belongs to Task 24's file set,
and its reachable-transport work is where the retry wiring
lands. (6) Python's Chromium fallback UA uses its CHROMIUM_MAJOR
constant; the Rust fallback uses `user_agents`' own detected/
preset identity for the family, since this tree's presets are
the identity source of truth here (Operational Mandate 1).

**Adjacent finds (not fixed, out of scope).** Python's
`_merge_records_into_jar` also prunes YouTube `ST-` per-page
state tokens from the site jar on rewrite (issue #101
hygiene); `chromium::merge_into_jar` does not. Not among Task
25's findings; left for the reconciliation pass.

**Task 25 gate (2026-11-04):** fmt clean, clippy clean
(workspace `--all-targets` and core `x86_64-pc-windows-gnu`,
`-D warnings`), 786 passed / 0 failed (771 baseline + 15 new:
4 jar regressions, 11 in `audit8_task25_slice`), build OK.
Owner screen-reader acceptance of the appended dialog section
joins the standing acceptance list.

**Task 25 integration gate (2026-11-04):** cherry-picked
clean onto main over the v0.1.57 release tree. Merged gate:
fmt clean, clippy clean (workspace and core windows-gnu),
786 passed / 0 failed, build OK.

## Audit 8 Task 24 resolution — reachable browser/challenge transport (B01, B02, B15, B16) (2026-11-04)

**B01 (P1) — the browser fallback has production callers in
all three paths now.** `browser.rs` was rewritten from a
per-fetch cold launch with no callers into Python
`core/browser_feed.py`'s shape (warm session, below), and
each Python call site is wired:

- *Refresh* (`feeds.rs::fetch_feed_document_cancellable`,
  called by `refresh.rs` with the worker's cancel flag and
  the feed's `feed_settings["proxy"]`): the plain fetch's
  failure modes route through the ported qualification
  deciders in `browser.rs` (Python
  `_browser_fallback_error_qualifies` /
  `_browser_fallback_response_qualifies` /
  `_response_looks_cloudflare_challenge`). A terminal fetch
  error qualifies when the status it carries (ureq's
  rendered `http status: NNN`, the counterpart of Python's
  `error.response.status_code`) is in {401, 403, 429, 503}
  or >= 520, or when the error text carries a
  connection-reset marker; timeouts and name-resolution
  failures never qualify. A fetched body that fails feed
  parsing qualifies on its status, a `Cf-Mitigated:
  challenge` header, the Cloudflare challenge body shape,
  or a challenge page. Qualification also requires the
  Settings gate (`browser_feed_fallback_enabled`, default
  True) and an un-cancelled refresh, and fires at most once
  per feed per refresh (the error path and the parse path
  are mutually exclusive in one call). A browser-recovered
  document REPLACES the response with Python's semantics:
  parsed entries flow into the normal store, the HTTP
  cache entry is rewritten with its validators cleared,
  and the returned `body_digest` is empty so the digest is
  never recorded.
- *Discovery* (`discovery.rs::discovery_page_html`, used
  by `discover_feed_urls` and `resolve_subscribe_url`):
  the plain page fetch escalates once to the browser when
  it fails or returns a challenge page, gated on the same
  Settings flag and the configured
  `browser_feed_fallback_timeout_seconds`, with failure
  memory OFF (Python's `detect_page_feeds` passes
  `remember_failures=False`; one-shot detection must
  always retry). A challenge shell the browser could not
  clear is still scanned as a page, as in Python.
- *Extraction* (`extract.rs::fetch_full_text_opts`): the
  chain now runs Python `_try_fallbacks`' full order. The
  direct fetch became status-aware (Python `_fetch_page`
  inspects error-status bodies too), so a gate status
  (402/403/429/451/503 — 401 deliberately absent) or an
  interstitial body sets `gate_seen` even when no page
  came back; previously only an extraction Gate outcome
  could. On a gate: (1) the clearance-refresh retry —
  Task 25 deviation (5) is CLOSED here: `cookies::
  refresh_clearance_from_browsers` runs first, a changed
  clearance refetches the URL once with the fresh stored
  identity (site headers + client hints, Python's HTML
  accept headers), and a recovered body becomes the fetch
  result outright; (2) unrecovered stored clearance is
  forgotten via `cookies::forget_clearance_for` and the
  forget result arms the impersonation stage; (3) Sky,
  impersonation, Jina (gate-only), Smry and Wayback run
  in Python's order (impersonation below); (4) the warm
  browser runs genuinely last — gate seen, no stored
  clearance, per-host failure cooldown (600 s, 256-entry
  clear-all cap, Python `_BROWSER_GATE_FAILURES`) — at the
  extractor's own 90 s floor with failure memory on.
  Every recovered/fallback body is re-checked against the
  gate markers before it is trusted, and a stage's body
  only ever replaces the direct outcome through the same
  `process_fetched_page` path (cascade + multi-page
  following) the direct fetch uses, so browser-recovered
  content can never displace fuller content already in
  hand: the direct Article still returns before any
  fallback runs. The app now calls `browser::configure`
  and `extract::configure_cookies` from
  `apply_http_config`, and `browser::shutdown` on exit.

**B02 (P1) — CAPTCHA solving carries the full challenge
data and delivers through the callback.** `captcha.rs`
was rewritten to Python `core/captcha_solver.py`'s
contract: the submission carries `data` (cData),
`pagedata` (chlPageData), `action` and `userAgent`
whenever the intercepted `turnstile.render` parameters
provide them (new `ChallengeParams`); the deadline floor
is `max(15, timeout_s)`; poll transport errors are logged
and polling CONTINUES to the deadline (the previous port
returned None on the first poll error); an empty solved
token fails closed; cancellation is honoured in 0.25 s
sleep slices. The browser stage's tier-4
(`browser.rs::solve_with_service`) installs Python's
`_TURNSTILE_HOOK_JS` verbatim via
`Page.addScriptToEvaluateOnNewDocument` before reloading
the challenge page, waits up to 25 s for the hook to
capture the parameters, solves with the remaining budget,
and delivers the token by invoking
`window.__blindrssTurnstileCallback` — then removes the
hook script so the warm session is not left with a
stalled widget. Authorization is structural:
`solve_turnstile` returns None before any request is
built when the solver is disabled, has no API key, or
names an unknown provider, and the fixture tests assert
ZERO solver requests in that state (counting loopback
mock). The API key, challenge parameters and token are
never logged.

**B15 — browser session lifecycle is Python's.**
One warm CDP session is kept across fetches and reused
while its options match (the proxy is the only varying
member; the profile directory is the persistent
`feed_browser_profile`, images always blocked); an idle
reaper closes it after 120 s (15 s interval); a warm
session that fails on use is closed and the fetch
retried once from scratch; the per-URL negative-result
cooldown (6 h, 512-entry cap, expired pruned then
clear-all) is recorded and honoured ONLY on the
remembering paths (feed refresh, extraction), never for
one-shot discovery, and never for a cancelled fetch or a
runtime-unavailable failure; runtime failures (no
installed browser, unwritable profile directories) start
the separate 5-minute runtime-unavailable cooldown.
Fetches serialize on one lock acquired in cancel-aware
slices; cancellation is checked before launch, during
launch, and through the settle, and never leaves an
orphan: the session's child is killed on close, on
options-mismatch replacement, on retry, and on
`shutdown()` (now called by the app on exit). Proxy
support reaches the launch as `--proxy-server`. The
settle loop is Python `_settle_page`'s: 0.35 s polls, a
document is usable at >= 2048 chars and not a challenge
page (feed fetches: structural feed validation instead),
and a Turnstile click (the CDP counterpart of
SeleniumBase's `solve_captcha` — locate the challenge
iframe, click its checkbox through trusted
`Input.dispatchMouseEvent`s) is retried on the 2 s / 3 s
cadence while a challenge is on screen. After a settle
that lands on an uncleared challenge, the escalation
runs on the remaining budget (min(max(timeout, 20), 90)
s, or 240 s with a solver configured, its auto-click
phase capped at 45 s so the paid solve fits) and ends in
the tier-4 solver flow above. After any success the
session's cookies for the final URL and the browser's
real `navigator.userAgent` are harvested into
`cookies::record_browser_session` under Task 25's
consent model (clearance-only). This supersedes the
v0.1.23 residual-audit statement that the warm-browser
stage "is a Python library with no Rust counterpart
short of bundling Python": the CDP-driven session is
that counterpart. Tests are fixture-only: a local fake
browser (`crates/core/tests/fixtures/fake_browser.py`,
driven through the `BLINDRSS_BROWSER` pointer) speaks
enough CDP to prove warm reuse (one launch across two
fetches), proxy relaunch, click-to-clear, cooldown
discipline, shutdown killing the process, the tier-4
flow end to end, and all three production wirings
(refresh recovery + disabled gate, discovery escalation,
extraction clearance recovery against a seeded Firefox
profile). The one live-Chromium test stays `#[ignore]`d.

**B16 — TLS impersonation: investigated under the full
research protocol; transport fidelity PENDING, stage
wired.** Evidence (2026-11-04): GitHub by stars —
`0x676e67/wreq` (1094 stars, pushed 2026-11-05,
BoringSSL, Apache-2.0), `apify/impit` (632, pushed
2026-11-04, Apache-2.0), `deedy5/primp` (622, pushed
2026-09-13, a wrapper over the wreq stack). GitHub by
most recent — the same projects plus small forks; no
other maintained candidate. crates.io state is the
blocker: wreq's stable 5.2.0/5.3.0 are YANKED, the
resolvable lines are 6.0.0-rc.31 and a renumbered
0.16.x, and wreq-util is likewise rc-only (3.0.0-rc.14);
impit's crates.io crate is only 0.1.0 (2025-01) while
its current impersonation architecture exists solely as
git-pinned `[patch.crates-io]` forks of rustls, h2,
reqwest, hyper-util and tower-http — adopting either
means pinning a release pipeline to yanked/RC releases
or vendored git forks of the whole HTTP stack, against
this repo's dependency discipline. Codeberg search
found only curl-impersonate and wreq/rnet mirrors;
GitLab search found no Rust impersonation client (one
curl_cffi mirror). A bounded spike (scratch crate,
`wreq = 6.0.0-rc.31` + `wreq-util = 3.0.0-rc.14`,
2026-11-04) FAILED to build on this box in both of its
runs: the first died after ~4.5 minutes inside
`boring-sys2` 5.1.2's build script (bindgen could not be
executed; "Failed to find OpenSSL compatibility
layer"), and a re-run died in the same toolchain area
(`bindgen`'s archive build, `zstd-sys`'s build command)
when the 512 MB tmpfs scratch `/tmp` filled — and a
successful spike would still leave every release leg
compiling BoringSSL plus a tokio runtime island inside
the blocking core. Both
credible transports are also async-only, while the
extraction workers are blocking ureq callers. The stage
itself IS wired with Python's semantics, so a future
transport drops into one function
(`extract.rs::impersonated_fetch`): target selection is
ported (`_IMPERSONATE_TARGETS` = default Chrome target
then `safari184`; a pinned-UA session restricts the list
to its single matching target via the ported
`impersonate_target_for_ua`), the stage is skipped when
the site's session already forced that handshake on the
direct fetch unless stale clearance was just dropped,
and its position (after Sky, before Jina) and
interstitial re-check match `_try_fallbacks`. Targets
are fingerprint profile keys, never User-Agent strings;
the UA on the wire still comes only from
`user_agents`/`http`. Certificate validation is
untouched everywhere. No live-host parity is claimed:
like Python on a host without curl_cffi taking its
degraded path, the stage currently resolves to no body
and the chain proceeds.

**Deviations (stated plainly).** (1) Python's tier-3
escalation runs in a SECOND engine (pydoll, a
WebDriver-free CDP client with its own profile); the
port has one CDP engine, so the escalation's clicks and
the solver tier run on the warm session itself, and
pydoll's separate anti-detection fingerprint is not
reproduced. (2) Refresh error qualification: Python's
sub-case where a non-qualifying status's attached
response itself qualifies has no counterpart — ureq
does not expose error bodies. (3) Python refresh call
site (d) (a parsed document with 0 entries that is
bozo or not feed-like) has no feed-rs signal to key on;
the port escalates on parse failures and challenge
shapes, which cover the observed cases. (4) Python
downloads Chrome-for-Testing when no Chrome is
installed (SeleniumBase); the port launches only an
installed browser (now also selectable via the
`BLINDRSS_BROWSER` pointer) and takes the
runtime-unavailable cooldown otherwise — no browser
downloading. (5) The harvest reads cookies for the
final URL (`Network.getCookies` with `urls`) rather
than Python's all-cookies read; `record_browser_session`'s
host filtering makes the stored result the same under
the consent model. (6) Bloomberg pages: Python fetches
them impersonation-first; that ordering awaits the same
pending B16 transport, and the cascade's existing
Bloomberg handling is unchanged.

**Adjacent finds (not fixed, out of scope).** Python
refresh also has an HTTP-level retry layer the port
does not mirror (`_response_looks_blocked` driving
Cloudflare/WordPress retries in `providers/local.py`);
it is refresh retry policy, not the browser/challenge
transport of this task, and is left for the
reconciliation pass. Task 25's adjacent find (YouTube
`ST-` token pruning in `chromium::merge_into_jar`)
remains open, likewise for reconciliation.

**Task 24 gate (2026-11-04):** fmt clean, clippy clean
(workspace `--all-targets` and core `x86_64-pc-windows-gnu`,
`-D warnings`), 803 passed / 0 failed / 2 ignored (786
baseline + 17 new: 6 captcha unit tests, 1 browser tier-4
unit test, 10 browser_parity additions incl. the
qualification deciders and the three production wirings),
`cargo build -p blindrss` OK. Owner screen-reader
acceptance of the browser-recovery behaviour joins the
standing acceptance list.

**Task 24 integration gate (2026-11-04):** cherry-picked
clean onto main. Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 803 passed / 0 failed,
build OK.

## Audit 8 Task 26 resolution — subscription discovery/search end to end (F05, F06, F07, F08, F09, F10, F14, F17, F18, F19, G09, G18) (2026-11-04)

**F05 — Video Search retains subscription targets.** `VideoResult`
now carries `native_feed_url` and `source_subscribe_url`, derived
at normalization time by a faithful port of Python's
`_derive_subscribe_targets_for_search_result` (with
`_native_youtube_feed_from_search_entry` and
`_choose_entry_subscribe_source_url`): feed results target
themselves, user/playlist results resolve the native YouTube feed
(playlist_id / channel_id) or `get_ytdlp_feed_url`, media results
subscribe to the owning channel/source. The dialog keeps its
existing buttons in their existing order and appends "Subscribe
(Native Feed)" and "Subscribe (Source)" after Close; Subscribe
(best) uses native else source. Every subscribe action goes
through the one provider-aware subscribe action (G09) and never
the bare video URL. Regression tests in `videosearch.rs` cover
video, channel, playlist and Mixcloud targets. Deviation: Python's
dialog stays open with a status label after subscribing and asks
the category through `add_feed_from_url_prompt`; the port's modal
dialog returns the chosen action and subscribes without a
category, as its subscribe flow already did.

**F06 — the site registry derives from the installed yt-dlp.**
`yt-dlp --list-extractors` is enumerated on a background warm
thread at startup (Python preloads its extractor classes the same
way), and `sites_from_extractor_names` applies Python's rules:
the label-override map, the redundant gvsearch/yvsearch drop, the
Mixcloud extra, dedupe by site id with losing keys recorded as
`search_key_aliases`, sort by (label, id), and the Pornhub adult
template appended after the sort only when adult sites are
enabled. The previous hard-coded seven sites (which offered Vimeo
and TikTok, neither of which exists in yt-dlp 2026.08.19) are
gone. Deviation, stated plainly: the CLI bridge exposes extractor
NAMES only — no `_SEARCH_KEY`, `IE_DESC` or `_WORKING` — so search
keys come from a table verified empirically against yt-dlp
2026.08.19 (ytsearch/ytsearchdate, scsearch, bilisearch,
nicosearch/nicosearchdate, rkfnsearch, prxseries, prxstories,
gvsearch, yvsearch; each key exercised against the CLI), and a
site is offered only when its key is in that table. Search
extractors with no CLI-derivable key (dailymotion:search,
CiscoLiveSearch, GameJoltSearch, RedGifsSearch on 2026.08.19) are
enumerated but not offered, matching the effect of Python's
non-empty-`_SEARCH_KEY` filter; `_WORKING` is assumed true; labels
for non-overridden colon sites derive from the IE_NAME base
(IE_DESC is not exposed). Until the enumeration lands, derivation
runs over the observed 2026.08.19 search-name set. Tests cover
derivation, extra/missing extractor tracking, alias recording,
adult gating and adult ordering.

**F07 — YouTube handle/custom-channel metadata recovery.**
`ytdlp_feed_url` ports `get_ytdlp_feed_url` in full: direct feed
URLs, playlist ids, `/channel/` and `/user/` shapes resolve
locally; `/@handle`, `/c/` and other channel pages go through the
metadata recovery — `yt-dlp --dump-single-json --flat-playlist
--playlist-items 0` over `ytdlp_recovery::cookie_attempts(None)`
(the anonymous attempt, then the configured cookie file, then
every consented browser source, the same ladder Python's
`_build_cookie_sources` builds) — accepting a `channel_id` only
from a playlist/channel document, so a consent or challenge shell
falls through to the next discovery stage instead of inventing a
feed. `detect_page_feeds` tries this native channel feed before
fetching the page, like Python. Deviation: the port passes
`--ignore-config` on this dump for consistency with its other
yt-dlp calls; Python's discovery call omits it.

**F08 — full discovery chain.** `discover_feed_urls` now mirrors
`discover_feeds`: a feed-shaped URL returns itself; the native
yt-dlp feed; social/forum/listing transforms; a page fetch (with
Task 24's browser recovery behind it on gate statuses and
challenge pages); a feed-shaped body returns the effective URL;
then RANKED alternate candidates (Python's scoring — page-token
overlap ×8, title-token overlap ×10, slug bonuses, −1/−3 path
penalties, comment-feed −12 — with document order breaking ties),
then anchor candidates, then the probed common paths, and only
when all of those are empty the validated OpenRSS fallback.
`looks_like_feed_url` and `body_looks_like_feed` (JSON Feed, RSS,
Atom, RDF) are ported, as are the vBulletin candidates used by
Detect Feeds on Page. One pre-existing test's expected order
changed: ranked enumeration now outranks document order when a
candidate's path shares the page's tokens, which is Python's
behaviour; the test's attribute-boundary and URL-resolution
assertions are unchanged. Tests cover comment-first ranking,
anchor-only pages, JSON/RDF detection and OpenRSS success/failure
shapes (offline).

**F09 — effective-URL resolution.** Page transport carries the
effective response URL end to end (`PageResponse.effective_url`
from the response URI after redirects); alternate links and
anchors resolve against it, and detection returns it for
feed-shaped bodies. A loopback test redirects /old/page to
/new/dir/page and resolves a relative feed link against /new/.

**F10 — Detect Feeds on Page has its own handler.** New menu id
M_DETECT_PAGE (the Feeds-menu item and the `feeds.detect_page`
shortcut action; the Tools-menu directory search keeps
M_FEED_SEARCH), a prompt → worker `detect_page_feeds` → drain
presentation matching Python's `_present_detected_page_feeds`:
none → "No feeds detected on this page.", one → Add Feed
prefilled with it, many → a "Detected Feeds" picker feeding the
same prefilled Add Feed flow. Errors follow Python's two
messages verbatim: a challenge → "BlindRSS could not complete
this site's browser verification page automatically." plus the
Tools > Import Site Cookies guidance; any other failure →
"Unable to retrieve the page." `detect_page_feeds` ports
Python's function: native YouTube channel feed first, plain
fetch, Task 24 browser recovery for gate statuses
(401/403/429/503/≥520) and challenge-looking 200 bodies,
`PageFetchError { is_challenge }` decided by
`looks_like_challenge_response`, wide-type alternates in document
order, vBulletin candidates appended. The B16 impersonated retry
remains a seam here as it is in extraction; it is not re-wired.

**F14 — authenticated source retries.** Odysee listing fetches
retry after any anonymous failure with
`--cookies-from-browser` for each consented source from
`ytdlp_recovery::cookie_sources()` — the same sources Python's
odysee.py cycles — first success wins, the last error surfaces;
yt-dlp reads the browser cookies itself, nothing is stored or
logged. Deviation: Rumble's login-page retry uses the app's
managed cookie jar (`cookies::site_headers_for` — cookies the
user imported or the app harvested under the Task 25 consent
model) instead of Python's live read of browser cookies through
yt_dlp's library; no live browser profile is read outside the
consented paths, and Chromium v20 app-bound live reading stays
the documented non-port. `rumble_looks_like_login_page` ports
Python's three login signals, with regression tests.

**F17 — Awesome RSS catalogue cache and tolerant OPML.** The
catalogue ports Python exactly: the same 16 country and 11 topic
files, parsed entries in Python's dict shape, dedupe by
lowercased URL, cache at `<data dir>/awesome_rss_catalogue.json`
with a 7-day TTL — a fresh cache serves search fully offline —
and the cache is written after a successful load. OPML parsing
uses the strict parser first with Python's tolerant tokenizer as
the recovery path, so malformed markup (e.g. an unescaped `&` in
an xmlUrl) recovers its entries instead of failing the
catalogue; the Feed Search `parse_catalogue` path shares the
recovery. Tests cover cached offline search against a
Python-shaped cache file and malformed-OPML recovery. The
stale-cache refresh and cache-write paths are not covered by
offline tests (they need the network catalogue).

**F18 — case-faithful video dedupe.** `canonical_key` is
Python's `canonical_search_result_key`: host lowercased with
`www.` stripped, path lowercased after the trailing-slash trim,
query preserved verbatim; YouTube video ids keep their case in
every form (watch `v`, youtu.be, /shorts/, /embed/). Case-variant
watch URLs collapse to one key; ids differing only in case stay
distinct. Regression tests cover both, plus host/path folding
and query-case preservation. Minor difference: the `v` parameter
is read through the URL parser's percent-decoding pairs where
Python uses `parse_qs` — equivalent for video ids.

**F19 — Python's result normalization and merging.**
Normalization ports `_normalize_ytdlp_search_entries` fully:
exact-URL dedupe, junk titles (title equal to the URL or the
entry id) repaired with Python's friendly per-host fallbacks,
the owner label (name / @handle, with the UC-prefix suppression
and the YouTube handle-from-URL rule), play counts (view_count
then play_count, non-negative only), the detail line
(`Kind | owner | H:MM:SS`), and the display site label taken
from the result URL's host. Not ported: Python's quick-title
network prefetch for junk titles (one page fetch per result
inside normalization) — the friendly fallback covers the same
rows offline. Merging ports the dialog's rule as
`videosearch::merge_result`: an existing row is replaced in
place — keeping its arrival slot — only when the new row's
quality tuple (real title, has a subscribe target, play count)
is strictly greater; the app previously dropped every later
duplicate first-wins. Duration and owner are also carried as
result fields for the dialog columns, as before; the detail
string itself is Python's.

**G09 — one provider-aware subscribe action.** `spawn_subscribe`
in app/main.rs is now the single subscribe path for Add Feed,
Feed Search directory results, Video Search and Detect Feeds on
Page: with a hosted provider active it calls the provider's
`add_feed` (a server-side subscribe; the ProviderAction drain
reloads the cache from the DB and runs the post-sync side
effects, so directory subscriptions persist remotely), and
locally it runs the worker subscribe path. Feed Search
previously subscribed locally even when a hosted provider was
active.

**G18 — Add Feed hints and category suggestion.** Add Feed is
Python's AddFeedDialog shape: URL and category in one dialog
(the combo lists the existing categories plus Uncategorized,
preselects YouTube when present, and accepts a typed new path),
with the asynchronous compatibility hint — YouTube hosts are
recognized immediately ("OK: Recognized as YouTube source", and
the YouTube category is auto-selected), other URLs are checked
on a worker after a 500 ms debounce ("Checking
compatibility..." → "OK: Supported by yt-dlp") without ever
blocking the dialog. Deviation: the support check is the offline
port `ytdlp_supported_offline` (scheme/host allowlist plus
media/article path heuristics) because the CLI bridge cannot
consult yt-dlp's extractor URL regexes in-process as Python
does; a non-allowlisted URL on an unrecognized host reports
unsupported where Python's regex pass might match one. Hosted
adds show the same dialog; the provider `add_feed` trait takes
no category, so the chosen category applies to local adds only
(pre-existing trait shape). Accessibility pass before the UI
edits: new controls were appended, never inserted (the two new
Video Search buttons follow the existing row; dialogs use
labelled controls with accessible names only and no
instructional descriptions; initial focus lands on the primary
field; results keep arriving through the worker + RefreshMsg
drain, so no dialog blocks on the network).

Adjacent finds left for reconciliation: Python's
`_merge_records_into_jar` prunes YouTube `ST-` per-page state
tokens on jar rewrite (Task 25's find — untouched here); Python's
refresh has an HTTP-level retry layer (`_response_looks_blocked`)
that Task 24 also noted and this task did not add; the port's
Add Feed category combo shows identity paths where Python shows
display names over identities (the port's established picker
convention); the vBulletin forum-id regex lacks Python's
case-insensitive flag (paths are case-sensitive in practice).

**Task 26 gate (2026-11-04):** worker gate on branch
audit8/task26-discovery: fmt clean, clippy clean (workspace
--all-targets and core x86_64-pc-windows-gnu, -D warnings),
825 passed / 0 failed (baseline 803; +22 regression tests
across videosearch unit tests, discovery_slice,
feedsearch_parity and source_depth_parity), `cargo build -p
blindrss` OK. Two VM replacements interrupted gate runs
mid-flight; deps were restored via the azure-only apt sidestep
and every gate was re-run to completion afterwards. Owner
screen-reader acceptance of the Video Search subscribe
buttons, Detect Feeds on Page and the Add Feed hints joins
the standing acceptance list.

**Task 26 integration gate (2026-11-05):** cherry-picked
clean onto main. Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 825 passed / 0 failed,
build OK.
**Task 24 macOS fix addendum (2026-11-05):** CI run
41978897138 on the Task 24 tree passed Ubuntu and Windows
but failed five `browser_parity` tests on macOS (warm
session reuse, click-clears-challenge, failure cooldown,
proxy relaunch, extract clearance refresh). Root cause:
`attempt()` in `browser.rs` bounded browser LAUNCH by the
caller's fetch timeout (`timeout_s.min(60)`), so on a slow
or loaded machine the browser start alone consumed the
whole budget and the fetch died with "Browser retrieval
timed out or cancelled" before ever navigating, while the
post-start page work takes under a second. Python never
charges startup against `timeout_s`: SeleniumBase starts
the driver on its own terms, and `timeout_s` budgets the
settle poll and the escalation. The 90 s production paths
survived a slow start; the 15/30 s callers did not. A
second, test-only defect: the extract test hard-coded the
Linux Firefox profile path (`.mozilla/firefox/...`),
which never exists on macOS
(`~/Library/Application Support/Firefox`), so clearance
refresh found no profile and the article stayed gated.
Fix: launch now runs against its own startup budget
(`LAUNCH_STARTUP_MAX_SECONDS = 60`, a wedge guard only,
not drawn from the caller's timeout); the launch-deadline
failure reports "Browser startup timed out", distinct
from fetch-lock starvation and cancellation; the DevTools
discovery poll bypasses any environment proxy
(`.proxy(None)` — the endpoint is the loopback port of
the browser just spawned); `settle_page` tolerates
page-state read errors and keeps polling to the settle
deadline, matching Python `_settle_page`'s swallowed
`get_page_source` exceptions. The fixture gained a
`start_delay_seconds` behaviour knob, and a new
regression test (`fixture_slow_browser_start_still_fetches`:
20 s start delay against a 15 s fetch timeout) failed
with the exact CI error before the fix and passes after;
the extract test's Firefox path is platform-correct.
Proof: `browser_parity` 15 passed / 0 failed / 1 ignored
across repeated runs, including single-CPU saturation
runs and forced-35 s-start-delay runs of the four
formerly failing fixture tests; full gate fmt clean,
clippy clean (workspace `--all-targets` and core
`x86_64-pc-windows-gnu`, `-D warnings`), 804 passed /
0 failed (803 baseline + 1 new regression test),
`cargo build -p blindrss` OK.

**Task 24 macOS fix integration gate (2026-11-05):**
cherry-picked onto main (PARITY union, 1 region). Merged
gate: fmt clean, clippy clean (workspace and core
windows-gnu), 826 passed / 0 failed, build OK. One
transient single-test failure in the first gate attempt
under peak three-way build contention did not reproduce
in two subsequent full runs; CI is the arbiter.

## Audit 8 Task 27 resolution — imported subscriptions and complete archive integration (F02, F03, F04, F11, F12, F13, F15, F16) (2026-11-05)

**F02 — recovered hosted episodes merge into ordinary reads.**
`archive_store::merge_articles` (the port of the hosted
mixin's `_merge_hosted_archive_articles`, with its virtual-
view exclusions: empty scope, `all`, `category:`, `unread:`,
`favorites:`/`fav:`/`starred:` views stay unmerged, `read:`
resolves to the real feed) is now called from
`HostedProvider::articles` for plain feed views and from the
main window's `refill_inner`: a plain subscribed-feed view
under a hosted provider whose (scope, feed) has stored
episodes (new cheap `archive_store::has_entries` check)
loads whole, merges, and runs the in-memory pipeline, like
Python's provider returns; every other view keeps the paged
path. Live/archive dedupe rides Python's identity keys, so
an episode the live feed also carries appears once, as the
live row.

**F03 — OPML/Takeout import routes through the active
provider.** The `Provider` trait gains `import_opml(path,
target)` and `categories()`. Local: the ported local
importer (F13) against the on-disk database. Miniflux: the
document is validated as strictly well-formed XML, its body
children are wrapped in one folder outline named by the
target category when one is given, and the document is
POSTed to `/v1/import` as `application/xml`; HTTP 404/405
falls back to the base per-feed import (category matched
case-insensitively or created, HTTP 400 for an already-
subscribed URL counts as success, as Python's Miniflux
`add_feed` rules). GReader family: the base per-feed import
over `parse_opml` triples, subscribing with the category in
the `subscription/edit` form's `t` field — Python's exact
carrier, quirks included. All three app handlers (Import
OPML, Import OPML Here with the tree category as target,
YouTube Takeout) run the import on a worker and land the
result as a `ProviderAction`, whose drain reloads the tree
and list; fixture tests assert the remote requests and that
a following sync retains the subscriptions with their
categories.

**F04 — archive Play is normal activation.** The archive
dialog takes a playback callback from the main window;
`act_play`/`act_activate` were split into parameterized
`play_article`/`activate_article` cores (the list actions
keep their selection and placeholder-row behavior), and
the dialog's Play button and Enter both route the picked
episode (selection, else first visible row — Python's pick)
through `activate_article`. Downloaded and recovered
episodes play in the player; the article-window opening is
gone from the Play path. The dialog now receives its own
`Db` connection opened outside the state borrow, since the
callback re-enters the activation path.

**F11 — Takeout keeps (source, URL) provenance end to
end.** `takeout.rs` is rewritten on Python's model:
`TakeoutImport` carries the feeds plus the four source
counts; dedupe is per (source, URL), so one URL keeps one
entry per source it appeared in; `selected(sources)`
tallies counts over the kept feeds BEFORE URL
deduplication while the feeds themselves are URL-unique,
first occurrence winning; member matching uses Python's
exact paths under the "YouTube and YouTube Music" section
and its exact DictReader field names (subscriptions/owner
read only the Channel ID column); the error strings are
Python's. The app flow mirrors `mainframe`: a worker
parses the ZIP and reads the provider's categories, the
drain prompts for source groups and the destination
category (Uncategorized first when unlisted, deduped), a
second worker snapshots feed ids, writes the selected
import as temporary OPML, imports through the provider,
and counts the new subscriptions; the completion message
is Python's, verbatim figures included.

**F12 — Takeout checklist native checkbox state.**
Python's dialog uses a native report list with real
checkbox state because wx.CheckListBox exposes none
(nvaccess/nvda#7325). wxDragon's ListCtrl has no checkbox
binding at all (no EnableCheckBoxes in its FFI surface),
so the port uses the one wxDragon control with native
checkbox state — a TreeListCtrl in Checkbox style with
flat root items, one column, the source-group labels with
counts, all checked initially, first item selected, focus
on the list, named "YouTube Takeout items to import" as in
Python. Deviation, stated plainly: the control's role is a
tree, not a list. NVDA checked-state announcement and
Space toggling are the owner's manual acceptance item;
they are recorded, not claimed.

**F13 — OPML compatibility.** `opml.rs`'s import half is
rewritten on Python's local importer: the decode chain is
strict UTF-8, then BOM-stripped UTF-8, then latin-1 —
Python's cp1252 step is unreachable behind latin-1, and
the port mirrors the chain exactly (a cp1252-encoded file
decodes as latin-1, smart-quote bytes becoming the same
control characters Python produces; the regression test
asserts that faithful result). Parsing is tolerant in
BeautifulSoup's shape: a streaming tokenizer that never
requires balanced tags (an end tag closes everything
opened inside it, EOF closes the rest), attributes read
case-insensitively, outline titles from `text` then
`title`, feed titles falling back to "Unknown Feed",
`title_is_custom` set exactly when an imported title was
present, feed-outline children filed under the SAME
category, folder outlines appending their title to the
category path (split on " / ", leaves sanitized, category
rows ensured), and feed URLs resolved OFFLINE through
Python's `_resolve_feed_url(allow_network=False)` chain
with dedupe against both the original and resolved URL.
A missing `<body>` is the failure, as in Python; a file
with a body and zero new feeds still imports successfully.
`parse_opml` (the utils triples) backs the base provider
import. For Miniflux's bulk import the document is
spliced, not re-serialized: same document, different bytes
than ElementTree's — stated as a deviation of form, not
of content.

**F15 — busy-close covers every native route.** The
archive dialog's close logic is one shared handler used
by the Close button and by a `CLOSE_WINDOW` binding that
vetoes the native close (title-bar X, Alt+F4, Escape) and
re-drives it through the same path: busy → Python's
"Stop the active work and close this window when it is
safe?" confirmation, Yes sets both cancel flags and
`close_when_idle` (the existing drain completes the close
when the work finishes), No stays open; idle → the same
end-modal code the Close button always produced.

**F16 — duration flows into archive identity.**
`ParsedEntry` gains `duration`: feed-rs loses the raw
`itunes:duration` text (it parses to a `Duration`), and
feedparser's `itunes_duration` — the value Python's
episode keys embed — is the raw element text, whitespace-
trimmed, NOT normalized to seconds ("1:02:03" and "3723"
are different keys). `parse_feed` therefore overlays a
raw-text scan (quick-xml; entry boundaries plus xmlns
scope tracking; the element counts when its prefix is
`itunes`, empty, or resolves to the iTunes DTD URI) over
feed-rs's parsed value, which remains the fallback.
`archive::entry_identity_keys` now receives the duration,
activating the title-duration branch that sat inert under
Gap 6; `article_identity_keys` still passes none, exactly
like Python's `_podcast_article_identity_keys`. Regression
tests: an undated episode whose GUID, page and media URLs
all changed still merges by `title-duration:<title>|<raw
duration>` (whitespace variant included), and a different
duration keeps the episodes apart.

**Task 22 deviation resolved.** The archive dialog's
downloads now route through the standard feed-folder path
with its folder override, like Python's archive worker:
the shared `download_root`, core `download_dir_for` with
the feed title as override ("Podcast Archive" when the
feed has no title), the legacy-aware `downloaded_path_for`
for both the worker's already-downloaded check and the
list's Downloaded column, and `apply_download_retention`
after each successful download.

Pre-existing tests adapted to the corrected shapes:
`media2_slice` and `parity_core_audit` read the new
`TakeoutImport` (one media2 fixture header was corrected
to the real Takeout spelling "Channel title" — Python's
DictReader lookup is exact-case, and the old fixture's
"Channel Title" only ever passed against the old, more
tolerant parser); `manage_slice` and `batch1_slice` pass
bytes to the OPML import API.

Adjacent finds left for reconciliation: the offline OPML
resolver inherits `sources::google_group_subscription_
url`'s gap — Python's `normalize_google_groups_url` also
accepts the legacy `#!forum` link form, the port handles
the email and `/g/name` forms only; the GReader family's
`add_feed` (and therefore its OPML import) does not do
Python's per-feed network discovery before subscribing —
a pre-existing gap, inherited, not widened; `Db::ensure_
category_path` does not UPDATE parent_id on existing rows
where Python's local-import ensure does — kept per this
repo's ensure-helper rule (INSERT OR IGNORE + re-read).

**Task 27 gate (2026-11-05):** worker gate on branch
audit8/task27-imports: fmt clean (run last), clippy clean
(workspace --all-targets and core x86_64-pc-windows-gnu,
-D warnings, both re-run after fmt), 851 passed / 0 failed
(baseline 825; +26 regression tests across opml_parity 11,
takeout_parity 7, audit8_task27_slice 5, archive_parity +3),
`cargo build -p blindrss` OK. One VM replacement mid-gate
wiped the apt build deps; they were restored via the
azure-only apt sidestep (shared with the parallel worker)
and every gate was re-run to completion afterwards. Owner
screen-reader acceptance items added by this task: the
Takeout source-selection list's checked-state announcement
and Space behavior under NVDA (F12), and archive Play
focus behavior after activation from the dialog (F04).

**Task 27 integration gate (2026-11-05):** cherry-picked
onto main (PARITY union, 1 region). Merged gate: fmt
clean, clippy clean (workspace and core windows-gnu),
852 passed / 0 failed, build OK.

## Audit 8 Task 28 resolution — Caster discovery/protocol fidelity (C01, C02, C03, C04, C05, C06) (2026-11-05)

The Caster engine is the port unit throughout: every behavior below is a
port of `caster_engine.py` / `caster_devices.py` / `caster_extras.py` as
adapted by Python `core/casting.py`, verified against fake SSDP / mDNS /
TXT / ECP / UPnP / YXC fixtures on loopback. No live receiver test ran:
this sandbox refuses multicast, so "RB Room" is unreachable from here —
the physical cast acceptance remains the owner's manual item, as it has
been since v0.1.42.

**C01 — media probing, preparation and live recovery connected.** New
GUI-free module `crates/core/src/castmedia.rs`. `probe_media` ports
`caster_engine.probe_media`: up to 5 Range attempts inside the 8 s
budget keeping the longest body, early stop once enough bytes prove
TS; magic bytes beat the Content-Type (ID3 / frame sync / OggS / fLaC
/ RIFF-WAVE / ftyp), the TS stride test needs the sync byte at byte 0
plus one stride (aligned) or four consecutive strides (mid-packet); a
playlist Content-Type is live HLS at once; a probe with no bytes and
no Content-Type at all is treated as LIVE (Caster: calling a stream
VOD on no evidence is the expensive mistake); local files probe by
magic + extension and are never live. `native_hls_url` ports
`_native_hls_url` (segment-shaped sibling `.m3u8`, validated by
fetching it: `#EXTM3U`, TARGETDURATION + EXTINF + a segment line, no
ENDLIST, longest listed segment <= 8.0 s). The Chromecast load path in
`app/src/casting.rs` no longer loads the caller-claimed MIME once as
BUFFERED: it probes, then climbs `ChromecastLoadChain` — a state
machine port of `_play_chromecast`'s ladder (native playlist when TS +
live, else the HLS relay for TS, else the probed direct load;
streamType LIVE/BUFFERED from the probe; on a non-settle: native ->
live relay, then the stream-type flip, then — direct on-demand loads
only — the conversion relay; at most 4 loads, then
"{device} did not start playing ({idle reason|no answer})").
Settling is `_await_playing`'s rule, implemented in `castsession.rs`
(`settle_verdict` / `await_playing_from`, status source injected for
tests): PLAYING/PAUSED on a session other than the previous one
settles; IDLE with a non-stale reason rejects; stale-session reports
and INTERRUPTED/CANCELLED idles are not verdicts; LOAD_TIMEOUT 12 s.
The LOAD payload itself is `_load`'s (`load_payload`): currentTime is
0 for a live relay, the resume position for a BUFFERED load, absent
for a live load; the post-settle corrective seek (start > 5 s,
receiver more than 5 s short) is `correct_resume_seek`. The HLS relay
is now a managed process in `proxy.rs`: `start_hls_relay` spawns
ffmpeg in the background (live mode: `-hls_time 2 -hls_list_size 8
-hls_flags delete_segments`, Caster's balanced-preset shape; VOD mode
keeps the established transcode), waits for playlist + first segment,
and registers the child; `stop_hls_relay` kills it and removes the
segment directory. The cast worker records the relay source per item
and stops it on Stop, on Next, on queue advance and before every new
load — stop/cancel tears relays down, nothing transcodes for a cast
that moved on. The `/hls` route serves a running relay's playlist
instead of starting a second transcode, and starts the live relay on
demand. Live recovery is `LiveWatchdog` (clock-injected port of
`_recover_live_cast`): armed only for loads recorded live; a stall is
20 s (`CAST_STALL_TIMEOUT`) with neither a new session nor position
movement; an idle live session recovers; a pause is the user's and
clears progress; recoveries are at most one per 30 s
(`CAST_RECOVERY_INTERVAL`). UPnP and Roku loads are prepared from the
probe too (`prepare_upnp_target`: TS or non-HTTP -> relay, else the
probed MIME; `prepare_roku_target`: TS -> relay, else `roku_mime`);
Sonos keeps `_play_sonos`'s rule (probed MIME when audio, else
audio/mpeg).

**C02 — Chromecast mDNS discovery.** `cast.rs` gained a record-level
mDNS parser (`parse_mdns_records`: PTR -> SRV/TXT/A joined, TXT
key/value pairs preserved; the triple parser remains as a wrapper) and
an explicit `_googlecast._tcp` browse (`discover_chromecasts_mdns`).
Names come from the TXT `fn` friendly name (instance label otherwise),
the model from TXT `md`, the port from SRV (8009 default); a name two
hosts claim is disambiguated `"{fn} ({host})"`, as Caster keys its
found dict. `discover_all` merges SSDP DIAL and mDNS results per host
(`merge_chromecast_discovery`): the mDNS name replaces an SSDP entry
that only had its host to go by, and the mDNS model fills an empty
one — mDNS-only TVs (Caster: pychromecast's browser misses them) are
now found with their friendly names intact.

**C03 — Roku ECP parameters.** `play_roku` now sends Caster's exact
launch (`caster_devices.roku_play`): channel `2213`
(`ROKU_MEDIA_PLAYER`), parameters `t=a|v`, `u=<url>`, then
`audioName`/`audioFormat` (mp3 default; wav/aac/flac mapped) or
`videoName`/`videoFormat` (mp4 default; the HLS MIME maps to `hls`),
form-encoded the way Python's `urlencode` encodes (quote_plus: space
is `+`). The old `url=/title=/contentType=mp4` query on channel 15985
is gone. `roku_launch_path` is pure and the fixtures assert the query
byte-for-byte for audio, video and HLS — HTTP success is never treated
as evidence media loaded. Roku discovery also tries Caster's
device-info name order (friendly-, user-, default-, model-name).

**C04 — Sonos coordinator routing and duplicate exclusion.** Discovery
excludes Sonos from the generic MediaRenderer list by name OR
manufacturer (`looks_like_sonos`, ported; `parse_upnp_description_full`
now reads the manufacturer and the ZoneGroupTopology control URL), and
a final merge pass drops any generic entry carrying a Sonos player's
name (Sonos merges last and wins). Play and stop for a Sonos device
resolve the group coordinator first (`sonos_coordinator_endpoint`:
description -> ZoneGroupTopology `GetZoneGroupState` ->
`parse_zone_group_coordinator`, members matched by exact description
Location with a host fallback, 192.168.1.5 never matching
192.168.1.50) and address the coordinator's AVTransport — Sonos
rejects play and stop sent to a non-coordinator member. Any failure
keeps the device's own endpoint.

**C05 — AirPlay filtering.** `discover_airplay` runs on the record
parser and keeps an announcement only when `raop_supported` passes:
not Sonos (name or TXT `am`), and the TXT `pw` flag is not true
(password/pairing required). Pairing itself stays unsupported, exactly
as in Python — the parity gap was the filtering. Fixtures cover a
classic RAOP receiver (kept), a pairing-required receiver (excluded)
and a Sonos RAOP announcement (excluded).

**C06 — MusicCast input preparation.** `castupnp.rs` ports the YXC
family (`yxc_available` via `main/getStatus` with a 2 s budget,
`yxc_set_input` with `prepareInputChange` first when the zone's
feature list advertises `prepare_input_change`, all with
`yxc_try`'s failure-is-an-empty-answer semantics). `play_upnp_on`
prepares before the push for DLNA renderers: host from the control
URL (Python `upnp_host`), available -> set input "server",
best-effort. A mocked-YXC fixture asserts the order
getStatus -> getFeatures -> prepareInputChange -> setInput ->
SetAVTransportURI -> Play, and a second fixture binds port 80 to prove
`play_upnp` itself performs the preparation.

**Tests (+30).** `crates/core/tests/castmedia_slice.rs` (new, 18):
TS stride shapes and the lone-0x47 false positive, probe fixtures
(lying Content-Type, playlist type, retry/longest-body, no-evidence
live, local files), native-HLS validation (VOD playlist, over-long
segments, non-segment URL never fetched), all four load-ladder shapes
with bounds, UPnP/Roku preparation, LOAD payload rules, the settle
policy (stale sessions, stale reasons, rejection reasons, timeout),
the watchdog timeline (stall bound, recovery interval, pause clears,
movement is progress), and a real-ffmpeg relay lifecycle test
(start -> playlist served through the proxy -> stop removes process
and directory). `cast_slice.rs` (+6): mDNS-only Chromecast with its
TXT name, SSDP/mDNS merge preserving friendly names, name-collision
disambiguation, the three RAOP fixture classes, `looks_like_sonos`,
and the full description parse (manufacturer, service URLs).
`cast_depth.rs` (+6): byte-exact Roku launch queries (the pre-existing
toggle test's channel assertion updated from the wrong 15985 query to
Caster's), MusicCast preparation order + port-80 wiring + the
unavailable case, ZoneGroupState parsing, and coordinator routing of
play and stop against a two-player household fixture. One pre-existing
test (`upnp_resume_play_seeks_once_playing`) now drives a DLNA device
instead of a Sonos one: its subject is `play_upnp_at`'s
seek-after-playing mechanics, and a Sonos device would now (correctly)
take the coordinator path its own fixtures cover.

**Deviations, stated plainly.** (1) Caster's `HlsRelay` internals —
the piped TsSource reader, replay dedupe, under-feed rotation and the
`_probe_codecs`-driven re-encode decisions — are NOT ported; the Rust
relay is the proxy's ffmpeg `-c copy` HLS route made managed and
live-capable. The ladder's final "conversion" step therefore remuxes
rather than re-encodes: a receiver rejecting the codec itself (not
the TS container) still will not play, where Caster's relay would
transcode. (2) The watchdog observes status on the controls window's
existing 2 s poll rather than Python's dedicated 5 s thread;
thresholds and intervals are identical. (3) The RAOP pairing filter
reads the TXT `pw` signal and Sonos identity; pyatv's full
pairing-requirement derivation from device info is not reproduced.
(4) Sonos seek/volume/status still address the member directly;
Caster documents coordinator routing for play and stop, which is what
is routed here. Soco's seed-based household discovery is not ported;
discovery remains the SSDP ZonePlayer sweep. (5) The native-HLS check
also runs against the proxy-decoded target URL — a deliberate
superset: Python checks only the post-proxy source, and because the
Rust app flow always proxies, the native path would otherwise never
fire; the sibling playlist is fetched and validated before use, so a
protected origin simply falls back to the relay. (6) Probing reads
through ureq with a Range header; Python's probe reads the response
stream — the byte windows can differ by a few bytes per attempt, the
classification is unchanged.

**Adjacent finds (recorded, not fixed — out of this task's scope).**
The shared `crate::http::agent()` honors the process proxy
environment: in this sandbox, requests to a LAN address outside
`NO_PROXY` (observed with 127.0.0.2) detour to the egress proxy and
hang — raw-UDP discovery (SSDP/mDNS) is unaffected, but description
and SOAP fetches in a proxied environment inherit it; this is
pre-existing shared-agent behavior used by every provider, not a
casting regression. Python's discovery merge dedupes by NAME across
all protocols (a later same-name device with sinks + video replaces
an earlier one); the Rust merge dedupes per protocol plus the Sonos
name rule — the full cross-protocol name merge is not ported. The
SSDP DIAL sweep still sends one M-SEARCH where Caster repeats
searches across the window.

**Task 28 gate:** the worker gate on branch audit8/task28-caster
ran fmt (last, clean), clippy (workspace --all-targets and core
x86_64-pc-windows-gnu, both -D warnings clean), the full workspace
test suite (882 passed / 0 failed: baseline 852 on main 942c800
plus 30 new) and `cargo build -p blindrss` (OK). No UI code
changed shape: the casting window's controls, names and order are
untouched (the accessibility pass consisted of confirming no dialog
edit was needed). No workflow files touched.

**Task 28 integration gate (2026-11-05):** cherry-picked
clean onto main. Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 882 passed / 0 failed,
build OK.

## Audit 8 Task 29 resolution — settings, help, and smaller main-window controls (S03, S07, S08, S09, S10, S13, S14, S16, S18, S19, S22, G08, G10, G14, G17) (2026-11-05)

Every behavior below was ported from the Python source named in
Audit 8 and the Task 29 plan section, regression-first: the new
suite `crates/core/tests/audit8_task29_slice.rs` plus inline tests in
`textfind.rs`, `diagnostics.rs`, `feed_settings.rs` and the app-crate
tests in `dialogs.rs`. The accessibility pass ran before any UI edit:
all new controls are appended (never inserted mid-list), carry
Python's accessible names, and put no instructional text in control
descriptions.

**S03 — per-feed article columns resolve and apply.** Core
`article_columns::resolve_for_view(global, Option<feed layout>)`
returns the feed's stored layout when the view is a single plain
feed, the global layout otherwise. The main window computes the
effective layout per refill: the column-rebuild signature now
includes the feed id and that feed's stored `article_columns` JSON,
so switching feeds rebuilds columns exactly when the effective
layout changes; category, smart-folder and favorites views keep the
global layout. Feed Properties gains the List Headers page (below,
G08) with an inherit checkbox and the shared column editor.

**S07 — Help Find and continuous reading.** Core `help.rs` now
parses the guide into the Python document model (sections/blocks,
`help_docs.py` port) and `render_plain_text` produces one continuous
document. The viewer shows the full rendered guide in one read-only
text control ("User guide text"), a contents list ("Guide contents")
that moves the caret to the section's line without announcing, and a
Find row: Ctrl+F focuses the find box, F3 / Shift+F3 step forward /
backward, Esc closes. Search is core `textfind` (see G14) over the
whole guide: a wrapped search announces "Wrapped to the start/end of
the guide.", a miss announces "Not found: {term}", a match is
selected with the caret at its end and focus returned to the guide.

**S08 — help locale fallback, announced.** The loader tries the
regional file, the base language, then English (fr_CA →
`fr-CA`, `fr_CA`, `fr-ca`, `fr`, `en`). When the document that loads
is not in the requested language, the viewer shows Python's fallback
notice verbatim above the guide, in a control named "Guide
language". Slice tests cover the candidate chain and the real-guide
fallback.

**S09 — interface-language picker.** Settings' General page appends
the picker: "auto" plus `Catalog::available_languages` over the
translation-override root and the bundled locale roots, displayed
sorted as "Name (code)". The choice is stored under the existing
`language` key, which `ui_language()` already honors at startup; no
hot-swap was invented — the app's existing restart behavior applies.

**S10 — display strings translated, stored keys stable.** Settings
field/choice labels, the shortcut list (keys.rs categories, command
labels, capture prompt, conflict message), article-column headers
and the reader's Read/Unread/Favorite/Media strings pass through
`crate::tr()`; stored ids, keys and bindings are unchanged. The
slice suite verifies against the shipped German catalog (Title →
Titel, Author → Autor, Date → Datum, Description → Beschreibung,
Media → Medien, and the sound/language labels).

**S13 — custom sound browse and preview.** The Sounds group gains
the refresh-complete and refresh-error rows the runtime already
reads (`sounds/refresh_complete.wav`, `sounds/refresh_error.wav`
defaults), each with a path field (named without the colon), a
"Browse..." button named "Browse for {name}" opening a WAV file
dialog, and a "Play" preview button resolving the path against the
exe dir, then the data dir, then as-is. Paths save raw, as Python
stores them.

**S14 — source credentials.** The Provider page appends Groups.io
and GitHub groups with Python's note texts verbatim and masked
fields named "Groups.io API key" and "GitHub personal access
token". Values are trimmed on save into the existing config keys
and consumed only by the existing consumers; they join the
diagnostics redaction set (S18) and are never logged.

**S16 — adult opt-in and User-Agent preview.** The Advanced page
appends a "Video Search" group: the "Enable adult sites in Video
Search" checkbox writes `enable_adult_search` — the key Task 26's
F06 gate consumes, so discovery stays off until explicit opt-in —
with Python's explanatory note. The User-Agent group gains a
"Current User-Agent" preview line recomputed when the identity
choice changes or the custom field is edited, resolved through core
`user_agents::resolve` (custom mode shows the typed string verbatim,
"(not set)" when empty).

**S18 — debug mode, rotating diagnostics log, redaction.** Core
`diagnostics.rs` ports Python's shape: a rotating log (5 MiB, three
backups) plus secret redaction — configured secret values
(provider keys, cookies, Authorization/solver material collected by
`secrets_from_config`) and key-shaped patterns are masked by
`redact_line`. Settings' General page appends the "Debug mode (show
console on startup)" checkbox writing `debug_mode`. At startup, and
only when the flag is set, `diag_init` opens the rotating log at
`blindrss.log` in the data dir, installs the config-derived secret
set, and on Windows allocates the console; `log_line` then routes
through the redacting rotating writer, and refresh failures are
logged beside their recorded feed errors. With the flag off,
startup writes nothing extra — `log_line` keeps its previous plain
append. Tests prove the redaction on the diagnostics path itself:
lines written through the rotating log to disk contain no secret.
Per the audit's hard rule, no Max Cached Views control was added —
the Rust implementation has no per-view cache for it to set.

**S19 — translation configuration.** Core `translate.rs` carries
`LANGUAGE_PRESETS`: the 189 name/code pairs extracted mechanically
from Python's `translation.py`, with display ↔ code round-trips
(`language_display_value`, `language_code_from_ui`). The Translate
page's target language becomes an editable combo seeded from the
presets as "{name} ({code})"; the stored value stays the code.
Per-provider model fields become editable combos seeded with the
provider's `default_models` suggestions, and the entered text saves
verbatim — custom models are never rewritten. The OpenRouter row
gains "Load OpenRouter Models": the fetch runs on a worker thread
behind a loading guard; success rebuilds the model combo from the
sorted, deduped ID list and restores the prior text; failure leaves
the combo untouched and reports "OpenRouter model load failed:
{e}" in the status line.

**S22 — legacy column normalization.** `resolve_layout` accepts
Python's legacy shapes: bare string lists, trimmed case-insensitive
keys, unknown keys dropped, duplicates deduped keeping the first.
Title is pinned: it resolves first, `move_key` clamps movement to
[1, len−1], and `set_visible` refuses to hide it. The editor saves
Python's `{"key", "visible"}` shape via `layout_to_json`. S03 and
S22 share the slice-suite vectors.

**G08 — Feed Properties behavioral overrides.** The dialog is now a
Notebook (General / List Headers, Python's shape). General keeps
its existing controls in place and appends, in Python's relative
order: refresh interval (the 15 `REFRESH_INTERVAL_CHOICES`, "Use
global setting" first), custom request headers (multiline), request
timeout, browser impersonation, proxy URL, full-text extraction
encoding, and the delete-behavior choice with its target-category
field enabled only for the category behavior. OK validates both
encoding fields with Python's error text and writes exactly one
merged `set_feed_settings(build_patch(...))`, so feed_settings keys
the dialog does not show are preserved. Consumers are wired and
verified: refresh builds `FeedFetchOverrides` from the stored
settings (headers, Python's referer rule, per-feed proxy agent,
timeout, body decode through the feed encoding); the scheduler
honors a per-feed `refresh_interval_seconds` with session attempt
times like Python (tick shortened to 30 s so the 30-second override
is real) and falls back to the untouched global path when no feed
carries an override; full-text extraction prefers a non-empty
`fulltext_encoding` over the feed `encoding`. A live loopback test
proves the headers, referer and windows-1251 decode on the wire.

**G10 — functional Feed Errors dialog.** `show_feed_errors` replaces
the status-text stub: Python's heading (including the zero-error
text), a list (Feed / Last attempt / Failures / Error) named "Feeds
with errors", and a detail pane ("Error details") with Python's
layout — feed, URL, category, last attempt, last success, the
consecutive-failures line, then the full multiline error. Copy
Details writes the detail text to the clipboard; Refresh Selected
(button or list activation) re-fetches on a worker with the feed's
own settings and clears the recorded error on success or records
the new failure; Feed Properties... and Remove Feed are wired; a
changed result reloads the main window. The dialog opens with zero
errors and explains itself. The pure text builders are unit-tested
in the app crate.

**G14 — reader Find reverse and terminal semantics.** The reader's
search is core `textfind::find_in_text`, an exact port of Python's
`_find_in_text` (direction, wrap, caret rules). Find starts at the
caret and wraps; Find Next (F3 / Ctrl+G) starts at the selection
end without wrapping and fails with `No more occurrences of
"{query}".`; new Find Previous (Shift+F3) starts at the selection
start, searches backward, and fails with `No previous occurrences
of "{query}".`. A match is selected with the caret at its end and
reported as "Found: {line}".

**G17 — staged saved searches.** The persistent-search manager
edits a working copy: Add and Remove mutate only the copy,
duplicates are detected case-insensitively on trimmed text, OK and
Run Search apply the staged list, Cancel discards it. The
normalize/dedupe helpers are pure and unit-tested in the app crate.

**Deviations, stated plainly.** (1) The impersonation override is
persisted verbatim (auto/always/never) but not wired to a transport:
the Rust impersonation stage is the recorded B16 residual, and the
browser fallback is not gated by it — Python keeps the two separate
as well. (2) The language picker and debug toggle are appended at
the end of the General page (Python has them mid-page) under the
append-only rule; Feed Properties' pre-existing Encoding and Reset
Title controls keep their positions, with the new controls after
them in Python's relative order. (3) Full-text encoding precedence
is `fulltext_encoding` (non-empty) over the shipped `encoding`
field. (4) The sound rows carry a Play button per the plan, though
Python's field builder shows Browse only. (5) Feed Errors
timestamps use the app's existing `fmt_ts` (UTC, minute precision)
rather than Python's localtime format. (6) The global column editor
now saves Python's `{"key","visible"}` shape without the Rust-only
width field; widths re-derive from defaults on load, as in Python.

**Adjacent finds.** None new beyond the B16 dependency above:
`close_to_tray` defaults were not touched (owner-disposition item,
out of scope), and no workflow files changed.

**Task 29 gate:** the worker gate on branch audit8/task29-settings
ran fmt (last, clean), clippy (workspace --all-targets and core
x86_64-pc-windows-gnu, both -D warnings clean), the full workspace
test suite (911 passed / 0 failed: baseline 882 on main 3923f83
plus 29 new) and `cargo build -p blindrss` (OK). The clippy pass
caught a real defect in this task's own new code — the column
editor's move/visibility calls dropped the functional core API's
returned layout — fixed by assigning the result before the gate
closed. Accessibility pass: performed before the UI edits above;
new pages/controls appended only, Python's accessible names used
throughout ("Find in guide", "Guide contents", "User guide text",
"Guide language", "Feeds with errors", "Error details", "Saved
searches", "Current User-Agent", "Groups.io API key", "GitHub
personal access token"), no instructional descriptions on controls.

**Task 29 integration gate (2026-11-05):** cherry-picked
clean onto main. Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 911 passed / 0 failed,
build OK.

**Flake note (2026-11-05):** CI run 42068872325 (the Task 28
tree) failed once on Ubuntu in
`captcha::tests::cancellation_stops_the_solve` — the test's
10 s wall-clock bound on a pre-cancelled solve was exceeded
under runner load (the cancellation itself worked: no poll
request was asserted sent, and the re-run passed; run
finished fully green on all three platforms after
rerun-failed-jobs, macOS included). First occurrence; the
assertion is load-sensitive by construction. Disposition:
rerun once per the standing rule; if the same assertion
fails again, harden the test to bound by poll-slice count
rather than wall time.

**CI fixes on the Task 29 tree (2026-11-05):** CI run
42082869004 failed on Windows (MSVC clippy) and Ubuntu.
(1) Windows: Task 29's S18 debug console declared
`GetProcAddress` with `*mut c_void` signatures while
hotkey.rs declares the same symbol `(usize, *const u8) ->
usize` — a second declaration with a different signature
is a hard error on Windows, invisible locally (the block
is cfg(windows) and the app crate's windows-gnu check is
blocked by the known build.rs issue; MSVC CI is the
arbiter and caught it). main.rs now matches the house
signature exactly and resolves through a transmute_copy
helper like hotkey.rs. (2) Ubuntu: the captcha
cancellation test failed a SECOND time (first on run
42068872325, passed on its re-run; again here), which by
the standing rule means root-cause, not another rerun.
Root cause: `solve_at`'s ureq agent reads a proxy from
the environment by default; whether NO_PROXY covers
loopback depends on the machine — this VM's does, the
Ubuntu runner's evidently does not reliably — so the
loopback submit intermittently detoured through a proxy
and blew the test's 10 s bound. Fix: loopback solver
endpoints (both in/res) bypass the proxy explicitly
(`is_loopback_url` + `.proxy(None)`, the browser.rs
DevTools idiom); remote solver endpoints keep the
environment proxy, as Python's requests calls do.
Classification unit-tested; captcha suite green locally.
This supersedes the earlier "one-off load flake"
disposition for run 42068872325.

**Captcha test, third failure and final disposition
(2026-11-05):** run 42086353799 (which CONTAINED the
loopback proxy bypass) failed the same elapsed assertion
on Ubuntu, so the proxy-detour theory is disproven as the
(sole) cause: the structural facts held every time (token
None, submit-only hits) while suite wall times on the
failing runs were 16-30 s against ~6 s locally — the 10 s
wall-clock bound around a socket round-trip is the
defect, not the cancellation path (locally the test runs
in 0.01-0.03 s, even pinned to one core; six consecutive
full-suite local runs all passed). The test now asserts
the structural proof only (exactly one hit, no token),
with the reasoning recorded in its comment: had
cancellation been ignored, the first poll would fire
after the 0.05 s test cadence and hits would be 2. The
loopback proxy bypass stays — it is independently
correct (loopback traffic should not traverse an
environment proxy; the browser.rs DevTools idiom) — but
it is no longer credited as this fix.
## Audit 8 Task 30 resolution — notifications and OS app identity (B09, B10, B11, B13, B14) (2026-11-05)

**B09 (P2) — notifications carry article payloads again.** New
GUI-free `crates/core/src/notify.rs` owns the payload policy,
ported from Python `gui/mainframe.py`'s assembly
(`_queue_new_article_notifications_from_state`,
`_notification_preview_text`, `_build_notification_body`,
`_build_notification_activation_payload`,
`_resolve_notification_article`): one notification per
eligible article with the article's title, a plain-text
preview (tags stripped, entities decoded, whitespace
collapsed, 180-character cap — the shared shape of
`_notification_preview_text` and `providers/local.py`'s
`_plain_text_preview`), body `"{feed} - {preview}"` under
`windows_notifications_include_feed_name`, a shared
per-refresh budget from `windows_notifications_max_per_refresh`
(0/absent = unlimited), cross-feed dedupe by article id,
per-feed exclusion via `windows_notifications_excluded_feeds`,
and Python's generic branch (count-only states resolve
per-article first, generic bodies for the remainder). The
capped summary now states the true suppressed count
("{n} new article notification(s) were suppressed by your
cap.", gated by `windows_notifications_show_summary_when_capped`);
the old port's summary reported the TOTAL new count across
all feeds as if it were the suppressed figure, and its
per-feed notifications were bare counts with no article
content and no click action at all. Rule exclusions hold by
construction: the drain feeds the planner
`StoreOutcome::new_articles`, which since D13 contains
exactly the stored, not-rule-removed, not-notification-
suppressed articles. The drain caps each feed's batch at
500 articles, mirroring Python's per-feed summary cap in
`_record_new_article` (the new-items count itself stays
uncapped, as Python's does). Click activation is wired on
Windows: each shown notification's payload is stored
(latest wins — the tray balloon replaces the previous
balloon, so only the most recent is clickable), and
`TaskBarIcon::on_balloon_click` runs an action that resolves
the payload through core `resolve_payload_article` (current
article list by id/url, then `Db::article_by_id`, then an
article constructed from the payload) and opens it through
the same `activate_article` core as Enter in the article
list — mark read, play or open per the normal activation
decision; an unresolvable payload falls back to opening its
URL, as Python's `_handle_notification_activation` does.
Deviations, stated plainly: (1) Python keys payloads per
notification object (wx NotificationMessage stack in the
Action Center); the wxDragon balloon has no per-notification
identity, so only the latest shown payload is clickable —
earlier notifications in a burst are display-only on
Windows. Linux (notify-send) and macOS (osascript) surfaces
have no click routing, as before this task. (2) Python's
two refresh drivers word the summary differently (the
single-feed driver uses the "(s)" form, the full-refresh
driver ngettext); the port has one drain and uses the
grammatical singular/plural pair. (3) Hosted-provider
refreshes complete through `RefreshMsg::ProviderAction`,
which carries no per-article states in this port, so hosted
notifications remain silent (sounds still fire) — unchanged
from before this task, recorded here rather than papered
over; Python's hosted providers emit `new_articles` states.

**B10 (P2) — foreground-right transfer.** `signal_existing`
now calls `AllowSetForegroundWindow(pid)` with the pid from
the instance state BEFORE sending the authenticated
activation, exactly Python `activate_existing`'s order, via
a `cfg(windows)` FFI declaration in
`crates/core/src/single_instance.rs`. The non-Windows path
is byte-identical to before. Manual verification stays the
owner's: second-instance activation from the tray/minimized
state while another application owns the foreground (plan's
platform-acceptance item) — not claimed here.

**B11 (P2) — exclusive private state creation.**
`write_state_file` unlinks whatever sits at the state path
(never writes through a planted symlink), then creates with
`create_new` (O_EXCL) and mode 0600 on Unix — Python's
`os.open(path, O_WRONLY | O_CREAT | O_EXCL, 0o600)` shape.
A racer's file appearing between unlink and create fails
the create instead of being overwritten, and `serve` then
reports the listener unstarted, as Python does. Tests
(module + `tests/single_instance_contract.rs`): created
file is a regular 0600 file; a planted symlink's target is
untouched and the path becomes a real file; exclusive
create refuses an existing file; a stale state file (dead
port/token) is replaced by serve; valid-token signal
activates exactly once; wrong-token and garbage state are
rejected without activation. The internals were
path-parameterized (`serve_at`/`signal_existing_at`) so the
contract runs against scratch paths; the public API is
unchanged.

**B13 (P2) — launchctl bounded at 20 s.** `app/src/startup.rs`'s
macOS `launchctl` wrapper no longer blocks on unbounded
`.output()`; it runs through
`blindrss_core::dependency::run_tool` with Python
`core/macos_integration.py`'s `_LAUNCHCTL_TIMEOUT_S` = 20 s
deadline (`run_startup_command`, returning Python
`_run_launchctl`'s (ok, stdout | stderr) shape). `run_tool`
kills and reaps on timeout and reports "timed out after
20.0s" as the failure message, which propagates into the
Settings toggle's error text the way Python's
TimeoutExpired string does. Tests: `run_tool`'s own
kill-and-reap timeout test (`dependency_parity.rs`), plus
new app tests driving `run_startup_command` against a fake
hanging script (deadline fires at 1 s, timeout in the
message, call returns promptly) and quick success/failure
scripts (stdout/stderr selection). The macOS call sites
(load/unload, "already loaded" acceptance) are unchanged
beyond the wrapper.

**B14 (P2) — macOS application bundle builder.**
`cargo xtask bundle-app <staging> <out_dir> <version>`
(new in `crates/xtask`) assembles
`<out_dir>/BlindRSS.app` from the release staging folder
with the metadata of Python `portable.spec`'s BUNDLE step:
bundle identifier `com.serrebi.BlindRSS`, name/display name
`BlindRSS`, category `public.app-category.news`,
`NSPrincipalClass` `NSApplication`, the passed version as
both `CFBundleVersion` and `CFBundleShortVersionString`.
Layout: the executable at `Contents/MacOS/blindrss`
(mode 0755), every other staging entry (locale/, sounds/,
docs/, vlc/, README.md, PARITY.md, minisign.pub) under
`Contents/Resources/`, plus `Contents/PkgInfo`. The dmg
contract holds: the workflow's macOS leg would stage the
bundle so `BlindRSS.app` sits at the dmg root, keeping the
fixed `BlindRSS.dmg` asset name and ship-shape's
contents-at-root Portable apply. App-side, resource
resolution learned the bundle: `resource_dirs()` (exe
folder, then `Contents/Resources` when the exe runs from
`Contents/MacOS`) now feeds `locale_roots()`,
`resolve_sound` and the help viewer's `docs/help` lookup,
so a bundled build finds its catalogs, sounds and guide.
xtask tests inspect the built bundle: layout, executable
bit, Resources placement (no duplicate executable), and
every Info.plist key above; staging without the executable
is refused with a named error. **Pending owner approval
(not done here): the release.yml integration** — in the
macOS leg only, after the Package step's staging is
complete, run `cargo xtask bundle-app dist/BlindRSS
dist/bundle "$VERSION"` and point the existing `hdiutil
create -srcfolder` at `dist/bundle` instead of
`dist/BlindRSS`; no other leg changes. Finder/VoiceOver/
start-at-login behaviour of the bundled app is the owner's
manual acceptance item (plan's platform-acceptance list),
not claimed here. `app/build.rs` needed no change: the
Windows identity it embeds (manifest + version resource)
is the Windows counterpart and already present.

**Accessibility pass (before the UI-visible changes):**
notifications are system surfaces, not controls; the pass
checked that (a) notification titles/bodies are plain text a
screen reader speaks as-is (no markup — previews are
tag-stripped in core), (b) balloon-click activation lands
through the standard article activation, so focus and read
marking behave exactly like pressing Enter on the article,
with no focus manipulation of its own, and (c) the launchctl
deadline removes a path where the Settings toggle could hang
the UI thread silently. No controls were added, moved or
renamed in this task.

**Task 30 gate:** the worker gate on branch
audit8/task30-notifications ran fmt (last, clean), clippy
(workspace --all-targets and core x86_64-pc-windows-gnu,
both -D warnings clean), the full workspace test suite
(935 passed / 0 failed: baseline 911 on main 50e6339 plus
24 new — notify 3 unit + 13 slice, single_instance 3 unit +
1 contract, startup 2, xtask bundle 2) and
`cargo build -p blindrss` (OK). Two defects in this task's
own new code were caught and fixed during the gate: the
preview entity decoder never ran for text without literal
angle brackets (Python's queue-side preview always decodes),
and an unterminated `<` in preview text swallowed the rest
of the string instead of staying literal.

**Task 30 integration gate (2026-11-05):** cherry-picked
onto the CI-fix tree; PARITY append conflict resolved by
union. Merged gate: fmt clean, clippy clean (workspace
and core windows-gnu), 936 passed / 0 failed, build OK.

## Audit 8 Task 31 resolution — translation override lifecycle (O16) (2026-11-05)

O16's three defects were all in
`crates/core/src/translation_updates.rs`, each a place where
the port recorded or trusted state Python treats as
unproven. All three were reproduced red before the fix
(a temporary repro file, since deleted: the version-reset
repro showed prepare reporting the transition handled with
the new version recorded despite a failed removal, and the
ETag repro showed the stale validator going out for a
catalog that was not on disk).

**Fail closed on version-reset cleanup.**
`prepare_overrides_for_app_version` ignored the result of
`remove_dir_all` and recorded the new app version
unconditionally, so a failed cleanup stranded the stale
override tree permanently: state claimed the reset had
happened while old catalogs kept shadowing the bundled
ones. The function now returns `PrepareOutcome`
(`Current` / `Reset` / `CleanupFailed`); on a removal
error it writes NO state at all — recorded version, ETags
and last_check all survive, exactly as Python leaves them
when `shutil.rmtree` raises before `save_state` — so the
reset retries on the next launch. The app side completes
the fail-closed shape Python gets from `core/i18n.py`:
when prepare reports `CleanupFailed`, main() sets
`OVERRIDES_STANDDOWN` and `tr()` builds its root list
without the override tree for that run, so bundled
catalogs are authoritative (Python's
`override_locale_dir` answers "" when prepare raises).
There is no process-level "prepared" memo like Python's
`_prepared_app_version`; the recorded-version check makes
repeat calls cheap no-ops, and prepare is called once per
launch, so the retry semantics match.

**Absent catalog clears its ETag.** `check_and_update`
sent the stored ETag even when the language's catalog file
was missing from disk, so a server 304 could "confirm" a
file that does not exist and strand the language with no
catalog. A `catalog_path` helper (Python's function of the
same name; `install_catalog` now uses it too) lets the
check drop the validator whenever the file is absent, so
the re-fetch is unconditional and a 304 cannot strand the
client — Python's exact mechanism, applied per language
before the fetch.

**Forced checks.** `check_and_update` takes `force: bool`;
forced checks send no validator for any language (Python
`check_and_update(..., force=True)`). The Settings "Check
for Updates" button is the forced path — Python's
`on_check_translations_now` passes `force=True` — and the
startup/scheduled check in main() passes `force: false`
and stays conditional, as do the frequency settings.

**Tests.** New `crates/core/tests/i18n_slice.rs` (4 tests):
failed cleanup records nothing and the retry resets once
the obstruction clears (removal failure is induced
deterministically by parking a regular file at the
override root, which `remove_dir_all` refuses on every
platform); absent catalog → the recording fetcher sees an
empty validator, the catalog is re-installed, and the
follow-up check with the file present sends the new stored
validator and stays conditional; forced check bypasses the
validator while the unforced check against the same state
does not; and bundled fallback plus same-version override
precedence — a downloaded catalog shadows the bundled
string, a version Reset removes it leaving the bundled
catalog intact, and a failed reset leaves the bundled tree
untouched. `batch22_slice`'s round-trip test was updated
to the new signatures (force argument, `PrepareOutcome`
comparisons); its damaged-catalog sanity refusal and
path-to-bundled stand-down assertions are unchanged and
still pass.

**Accessibility pass (before the Settings change):** this
task adds, moves and renames no controls. The only
Settings change is the argument the existing "Check for
Updates" button passes to core; its name, position and tab
order are unchanged, and its result text in the adjacent
status label is unchanged. The main-window change is
invisible unless a reset cleanup fails, in which case the
app simply speaks the bundled language for one run.

**Deviations.** None from Python's lifecycle semantics.
Two pre-existing port shapes were deliberately left alone
as outside O16: the reset replaces the `etags` object with
an empty one where Python pops the key (readers treat them
identically), and the Settings manual check gathers its
language list from the locale roots with the UI language
first where Python uses the picker selection plus
`installed_languages()` (a language-selection difference,
not a validator-lifecycle one).

**Task 31 gate:** the worker gate on branch
audit8/task31-i18n ran fmt (last, clean), clippy
(workspace --all-targets and core x86_64-pc-windows-gnu,
both -D warnings clean), the full workspace test suite
(940 passed / 0 failed: baseline 936 on main a56f8bf plus
the 4 new i18n_slice tests) and `cargo build -p blindrss`
(OK). One clippy catch in this task's own new test code
(&PathBuf where &Path belongs) was fixed before the gate
closed. Mid-gate the VM was replaced and apt packages
vanished; restored via the azure-only sidestep in
~/TOOLS.md and the gate re-run from the clippy leg.

**Task 31 integration gate (2026-11-05):** cherry-picked
clean onto main. Merged gate: fmt clean, clippy clean
(workspace and core windows-gnu), 940 passed / 0 failed,
build OK. All 31 Audit 8 tasks are now integrated; the
Wave 4 release (v0.1.58) and the closing reconciliation
follow.

**CI fix on the Task 30 tree (2026-11-05):** run
42095130853 failed Windows MSVC clippy in
app/src/startup.rs: Task 30's B13 items
(`STARTUP_COMMAND_TIMEOUT_S`, `run_startup_command`) were
gated `cfg(any(macos, test))` and their tests are
unix-only (shell-script fakes), so in the Windows test
binary the items were dead code and the test module's
`use super::*` was unused — four -D warnings errors,
invisible locally (Linux runs the tests; the app crate's
windows-gnu check is blocked by the known build.rs
issue). Fix: items gated `cfg(any(macos, all(test,
unix)))`, test module gated `cfg(all(test, unix))`.
Behavior unchanged everywhere the code runs.

**CI fixes on the combined Wave 4 tree (2026-11-05):**
run 42104773420: macOS success; Windows failed the new
single_instance_contract test; Ubuntu was cancelled after
~90 minutes stuck in the "Install wxWidgets build
dependencies" apt step (runner infrastructure — the same
apt trap recorded for BlindFB the same day; no code
involved; the job never reached compile). Windows root
cause: the contract test assumed the state path follows
XDG_RUNTIME_DIR on every platform, but Python
`_state_path` (and the port's state_path, faithfully)
uses %TEMP% unconditionally on Windows — serve() wrote
the real temp path while the test read its scratch dir
and found its own stale plant. Production code matches
Python exactly; the test now computes the expected path
by Python's platform rule. The earlier B13 cfg fix is
confirmed by this run's clean Windows clippy.

**Wave 4 release (2026-11-05): v0.1.59.** The first
Wave 4 tag, v0.1.58 (86549b0), FAILED its Release run at
the Validate step on both Unix legs: the version-bump
commit carried app/Cargo.toml only, and the release
build's --locked rejected the stale Cargo.lock member
version (CI does not use --locked and passed). The tag
is burned with no release, per the standing
never-reuse rule. Lesson, now part of the release flow:
after any version bump, run `cargo update -p blindrss`
so the lock sync lands in the SAME commit, and include a
`cargo check --locked` in the release gate. v0.1.59
(9294956: bump + lock sync) shipped verified: CI fully
green, Release run 42132756400 success (3 Package +
Publish), exactly 7 fixed-name assets, contents at
archive root, Linux signature VALID with tamper
rejected. Combined Wave 4 tree state: 940 tests.

## 10. Audit 8 reconciliation (2026-11-05, post Wave 4)

Every Audit 8 finding ID (160) carries a resolution or a
recorded deviation in the sections above; a mechanical
cross-check confirms all 162 ID tokens in the audit doc
are addressed in this file. Final state of the items that
are not plain "fixed":

- B16 (TLS impersonation transport): LANDED v0.1.65 —
  wreq 0.16.3 / BoringSSL shipped after all (the yanked /
  pre-release state this entry cited had resolved by the
  2026-11-06 spike); see the "## B16 landed" section at
  the end of this file. Python refresh's
  `_response_looks_blocked` escalation is the same item
  and landed with it as refresh's per-feed impersonation
  stage.
- B17: the privileged Chromium v20 master-key resolution
  (SYSTEM/CNG/UAC helper, VSS) — LANDED v0.1.66; see the
  "## B17 landed" section at the end of this file. The
  earlier "deliberately not built" disposition is
  superseded by Brandon's 2026-11-06 finish-everything
  order.
- Task 20 deviation: SUPERSEDED — local MP4/M4A/M4B
  embedded chapters were ported in Audit 9 and shipped
  v0.1.63 (see the Audit 9 section). ID3, Podcasting 2.0
  and YouTube chapters were already ported.
- O17 (update download cancellation/retry): superseded —
  the hand-rolled updater it cited was deleted by Task 6;
  updates are ship-shape's, whose behavior owns this now.
- Task 23's Deno packaging and Task 30's B14 bundle-app
  workflow step: LANDED v0.1.67 under the owner's
  2026-11-06 finish-everything order (which is the
  workflow decision these proposals awaited); see the
  final-sweep section at the end of this file.
- `close_to_tray` default: DECIDED by Python parity —
  Python's defaults table seeds it True; the Rust read
  sites fell back to False. Flipped to True at both read
  sites in v0.1.67; stored user values are unaffected.
- Remaining per-task deviations (wxDragon limits, relay
  shapes, probe byte windows, and the rest): given final
  verdicts in the final-sweep section at the end of this
  file (2026-11-06).

Two adjacent finds were closed during reconciliation:
the YouTube `ST-` token pruning (Task 25's open find)
is now ported in `chromium::merge_into_jar` with
Python's exact rule (skip on merge, prune on rewrite,
youtube.com domains only) plus a regression test; and
the Python re-comparison below produced one port.

### Python source re-comparison, v2.5.8 -> v2.5.9

Python moved three fix commits after the Audit 8
baseline (2a7eeb3), shipped as Python v2.5.9 (#148, #150,
#151 via #149/#150/#151):

- #148 (NVDA re-reading the focused row when chapters
  arrive): ALREADY PORTED — Task 20's live poll defers
  the focused row's title rewrite until the selection
  moves, exactly Python's fix; recorded in the Task 20
  resolution.
- #150 (cancelled YouTube loads leaving ~30 MB folders
  in %TEMP%): NOT APPLICABLE — the folders are the
  Python app's own PyInstaller one-file unpack dirs, left
  when its worker children outlived a cancel. The Rust
  binary has no unpack folder, creates no temp dirs in
  ytfull, and its yt-dlp runs are owned subprocesses
  (Task 23) killed on cancel.
- #151 (yt-dlp launched for every YouTube row paused
  on): PORTED in this reconciliation. Python's
  `_fulltext_debounce_ms_for` gives Windows YouTube
  selections a 3000 ms settle delay (the packaged
  yt-dlp's per-run unpack cost); macOS and all other
  cases keep 350 ms; reader focus stays immediate.
  Rust: `auto_fulltext_delay_ms` behind
  `schedule_auto_fulltext`, regression-tested.

Reconciliation tree state: 942 tests, all gates green.
The classic-branch swap remains the final step, on the
owner's explicit word only.

**Final Audit 8 release (2026-11-05): v0.1.60** —
reconciliation fixes (ST- pruning, #151 debounce) with
the lock sync in the same commit; CI fully green;
Release run 42144380037 success (3 Package + Publish);
verified Latest, exactly 7 fixed-name assets, contents
at root, Linux signature VALID with tamper rejected.
942 tests. Audit 8's plan completion criteria are met
on the code side; owner screen-reader/platform/hardware
acceptance remains the owner's separate record, and the
classic-branch swap awaits the owner's explicit word.

**Accessibility fix (2026-11-06, Brandon's report): v0.1.62.**
Escape did not close the app's custom dialogs. Root
cause: every custom dialog wires its OK/Cancel buttons by
hand — plain buttons whose clicks call
end_modal(5100/5101) — and never sets an escape id, so wx
never learns which code Escape should end with; Python's
dialogs get Esc from wx's standard handling. Fix: all 33
custom dialogs now call set_escape_id(5101) right after
building. The cast queue window (a modeless frame) also
gained the CHAR_HOOK Escape binding the article, player
and help windows already had (verified present, matching
Python). Two source-scan regression tests
(every_custom_dialog_sets_an_escape_id,
modeless_windows_bind_escape_to_close) keep any future
dialog or window from silently losing Escape again.

**v0.1.62 VERIFIED (2026-11-06):** Latest, exactly 7
fixed-name assets, contents at root, Linux signature
VALID with tamper rejected; 944 tests; CI fully green
on c445db3 (the tag tree also carries the discovery
test-server Connection: close fix described in that
commit). Brandon's NVDA pass on Escape across dialogs
is the acceptance step.

## Audit 9 — missing-feature completion pass (2026-11-06, Brandon's order)

A fresh sweep of every recorded non-port against current
Python main (v2.5.9, unchanged since the reconciliation).
Three real gaps were completed; several recorded "gaps"
turned out to be stale records, corrected below; the rest
are dispositioned with fresh evidence.

**Completed.**

1. **Local MP4/M4A/M4B embedded chapters** (was: Task 20
   deviation). `chapters::mp4_chapters` walks the box tree
   (moov/udta/trak/mdia/minf/stbl) for the `chpl` atom —
   the Nero chapter list mutagen's `MP4.chapters` reads,
   the only MP4 chapter source Python exposes — and parses
   its entries (u64 100-ns starts, length-prefixed titles).
   `embedded_media_chapters` mirrors Python's branch order
   exactly (MP3/ID3 first, then the MP4 suffix/type set;
   remote MP4 still not downloaded, Python's rule).
   Tests: crates/core/tests/mp4_chapters.rs (4).
2. **YouTube watch-next chapters + the chapter memo**
   (was: Task 20 deviation (3) + Task 23 adjacent find (3)).
   `ytfull::extract_chapters` now ports Python's
   `youtube_fulltext.extract_chapters` in full: one
   InnerTube `next` call, sources in Python's order
   (player-bar markers incl. markersMap
   DESCRIPTION/AUTO chapters, the key-moments panel, then
   description timestamps validated against the `player`
   call's lengthSeconds), `None` (unsettled) falling back
   to the yt-dlp info dict while a settled empty list does
   not, the "<Untitled Chapter 1>" gap fill, and the
   30-minute per-video memo (successes only, empties
   included). The chapters transport calls it instead of
   going straight to the info dict. Python's cancellation
   check stays unported per the Task 23 deviation (call
   sites guard with generations). Tests:
   crates/core/tests/watch_next_chapters.rs (5, fixture
   InnerTube responses through the injected fetch).
3. **Cast discovery: cross-protocol name merge, sinks,
   repeated searches** (was: Task 28 adjacent finds).
   `CastDevice` gains `sinks` (Caster's rule: published =
   believed, empty = unknown); UPnP descriptions now also
   resolve the ConnectionManager URL and discovery fetches
   each renderer's GetProtocolInfo sinks
   (`upnp_sink_mimes`, Caster's parse). `discover_all`
   merges by NAME in Caster's fixed order (chromecast,
   airplay, dlna, roku, kodi) via
   `merge_discovery_by_name`: a later same-name device
   replaces the incumbent only when its published sinks
   prove video the incumbent cannot show; Sonos still
   merges last and wins outright (the shipped C04 pass is
   unchanged). Both SSDP sweeps now repeat each M-SEARCH
   3 times across the window (Caster SSDP_SEARCHES,
   spaced timeout/4), deduped downstream as before.
   Tests: cast_depth merge/support rules (3 new).

**Stale records corrected (already ported; not gaps).**

- Forum HTML layouts (Invision/vBulletin/FluxBB/Drupal/
  XenForo): ported in `forum_sources.rs` with its own
  selector matcher and covered by source_depth_parity
  tests. The stale claim survived in threads.rs's module
  doc and §7-era notes; the doc now points at
  forum_sources.
- The range-cache proxy for ordinary playback: ported
  by Task 22 (core/rangecache.rs + the player wiring,
  probe retry, context and toggle reloads). §1's table
  row predates it and is superseded by §9 as usual.
- Sonos seed-based discovery: Caster-app surface only —
  Python BlindRSS itself calls `sonos_discover` with
  `seed_ips=[]` (core/casting.py). Nothing to port.
- Settings surface items from the batch-9 era list
  (interface-language picker, Debug mode): shipped by
  Task 29 (S09/S18). `max_cached_views` remains an
  Audit 7 "not a gap" (Python's control is inert there).

**Still open, with verdicts.**

- **B16 (TLS impersonation transport): research refreshed
  2026-11-06.** The landscape moved: `wreq` (the rquest
  rename) now has a stable 0.16.1 line in production use
  elsewhere (fastcrw/crw PR #527, Sept 2026, verified
  live against a JA3 wall with Chrome profiles;
  independent test 2026-10-11: builds and 280+ tests pass
  on Rust 1.99/Ubuntu with BoringSSL via btls + libclang —
  this box's toolchain). The 6.0.0-rc line also continues
  (rc.31, Aug 2026). impit remains git-patch-only for
  Rust use. Remaining blocker is now only the shape: wreq
  is async (tokio) and the core is blocking — the port
  needs a bounded spike proving a per-call blocking
  bridge + the BoringSSL build inside this workspace's
  CI, then the stage wiring behind
  `extract::impersonated_fetch`. That spike is the next
  B16 step; it was not rushed into this release.
- **B17 (privileged v20 helper): unchanged.** A new
  elevated SYSTEM-impersonation binary is a security
  surface of its own; it ships only with a design pass
  and a Windows test pass, neither of which this audit
  substitutes for. v20 rows remain counted-and-named
  with the file-import route working.
- **Caster HlsRelay internals** (piped TsSource reader,
  replay dedupe, under-feed rotation, `_probe_codecs`
  re-encode decisions): the recorded shape deviation
  stands. This pass landed its prerequisite — per-device
  published sinks now reach the cast layer — but the
  relay stays the managed ffmpeg `-c copy` HLS route
  until a receiver-driven transcode decision is designed
  against it; physical receiver acceptance remains
  Brandon's (RB Room unreachable from the sandbox).
- **pytubefix client phase** (Task 22 deviation,
  narrowed v0.1.82): still no Rust analogue; standing
  verdict. The hidden-browser visitor-data bootstrap
  that shared this verdict IS ported now
  (youtube_session.rs plus the final recovery phase in
  media/play-cache/download) — see the closeout
  resolution at the end of this file.
- Small recorded niceties left as-is: installed-browser
  version detection for the "auto" UA preset, GSMArena's
  battery-table sort, the diagnostic seek probes
  (diagnostic-only in Python by design). None change
  what a user can do; they are polish, listed here so
  the record stays complete.
- Owner-decision items unchanged from §10: Task 23's
  Deno packaging and Task 30's B14 workflow integration
  (both need Brandon's workflow OK), and the
  `close_to_tray` default disposition.

**v0.1.63 VERIFIED** (2026-11-06): release commit
`57f4dac`, CI run 42328945643 fully green (all three jobs
read directly), Release run 42330251144 success (3 Package
+ Publish). API verification: draft=false,
`/releases/latest` = v0.1.63, exactly 7 assets with the
fixed ship-shape names. Linux tarball contents sit at the
archive root; signature verified VALID with the committed
public key and the tamper control (flipped byte) rejected.
Local gate on the release tree: 956 tests passed, 0 failed.

## User reports batch (2026-11-06, Brandon's hands-on reports against v0.1.63)

Brandon filed ten reports from his own NVDA use and ordered
the whole batch brainstormed, fixed, committed and released
("I want everything brainstorm fix and commit and release").
Method per his stop-and-think directive: Python is the spec;
every root cause below was read from both sources before any
edit. Shipped as v0.1.64.

1. **Ctrl+Up/Down did nothing in the article text area.**
   Python's `on_content_key_down` never special-cases the
   keys; its readers are all TE_RICH2 (mainframe.py:6032,
   article_window.py:67, help_viewer.py:123), so the native
   RichEdit paragraph movement is the behavior. The port
   built plain MultiLine|ReadOnly controls at all three
   sites. Fix: `TextCtrlStyle::Rich2` added at the main
   reader, the article window reader and the help reader.
   Guarded by `readers_use_rich2_so_ctrl_arrows_move_by_paragraph`.

2. **No author/date/publisher header; title ran straight
   into the body.** Python's selection pane composes
   title + "Date:" (humanized) + "Author:" + "Link:" + a
   40-dash rule + blank line, and its full-text render
   composes "Title:" + "Author:" + blank line over the
   whitespace-normalized body (article_extractor.py
   `_render`). There is NO "Source:" line in either Python
   composition — none was invented. The port composed
   `"{title}\n\n{body}"` everywhere. Fix: core ports of
   `humanize_article_date` (utils.py:1501) and
   `collapse_blank_lines` (see 3), app helpers
   `selection_reader_text` / `fulltext_reader_text`, applied
   at every reader fill site (selection, cached full text,
   translation result, extractor result, YouTube full text,
   article-window load completion). The article VERSIONS
   dialog composition was left unchanged (not a reported
   surface). Deviation: the local UTC offset used for the
   absolute Date line is today's offset, not the offset in
   effect on the article's own date; across a DST boundary
   the rendered absolute time can be off by one hour.

3. **Blank lines between paragraphs.** Python's
   `collapse_blank_lines` (utils.py:549 — "A screen reader
   stops on every one of those blank lines") drops empty
   lines at its render points. The port never collapsed.
   Fix: exact core port in postprocess.rs, applied inside
   both composition helpers, matching Python's application
   points.

4. **Focus trapped on the Add Feed button after Alt+Tab;
   Tab/Shift+Tab could not leave; restart only.** Mechanism
   NOT proven from source: the frame CHAR_HOOK passes Tab
   through, and no code path was found that swallows it.
   Structural divergence fixed: Python has no button row at
   all (Add Feed is menu-only); the port's ~23 buttons and
   the search box were direct children of the frame, and a
   wxFrame is not a tab-navigation container the way a
   wxPanel is. The buttons and search controls now live on
   a dedicated `btn_panel` (same controls, order, labels,
   position). Whether this cures the trap on Brandon's
   machine is decided by his retest, not by this record.

5. **Menu shortcuts not announced.** Proven: Python routes
   registry items through `_append_shortcut_menu_item`,
   whose label is the binding rendered as TEXT —
   `"Add Feed (Ctrl+N)"` — deliberately not a `\t`
   accelerator (dispatch is the global char hook; a native
   accelerator would double-fire), re-rendered by
   `_refresh_shortcut_menu_labels` after binding edits.
   The port had the binding and the dispatch but never the
   label rendering. Fix: core `shortcuts::menu_item_label`
   + app `refresh_shortcut_menu_labels`, a post-build pass
   over `shortcut_menu_id` mappings via
   `MenuBar::find_item`; base labels captured once in
   `State.menu_label_bases`; called after the menu bar is
   set and again when the shortcuts dialog closes.
   Simplification vs Python: mnemonic re-derivation is
   skipped — base labels are never regenerated, so the
   captured base keeps its original mnemonic.

6. **PowerShell window flashed on failed refresh, landing
   in NVDA's speech history.** Proven chain:
   `refresh_side_effects` → `play_sound_file` spawned
   `powershell` (SoundPlayer script) with no
   CREATE_NO_WINDOW; the app is windows_subsystem, so the
   console app flashed a console window and stole focus.
   Python plays in-process (`wx.adv.Sound`), and every
   Python PowerShell spawn carries CREATE_NO_WINDOW with
   the comment that a console window "can take focus from
   a screen reader". Fix: `play_sound_file` now plays
   in-process via wxdragon `Sound` on all platforms (the
   afplay/paplay branches are gone too), and
   CREATE_NO_WINDOW was added at the three remaining spawn
   sites that lacked it: core `windows_shortcuts.rs`
   run_powershell, app settings.rs test-notification
   balloon, core `dependency.rs` spawn_tool (yt-dlp /
   ffmpeg / ffprobe launches).

7. **Uncategorized sat right after Favorites.** Proven:
   the port appended it before the sorted categories.
   Python treats it as an ordinary member of the sorted
   category set (no pin); Brandon directed the literal
   reading — bottom of the tree. Fix: the Uncategorized
   node is appended last, after categories,
   Recommendations and Smart Folders; its unread label and
   feed filing are unchanged. Adjacent observations NOT
   acted on: the port has no "All Articles" / "Deleted
   Articles" root nodes (Python has both; All Articles is
   exposed via the View filter).

8. **Tabbing into a folder's list: first article not
   selected/read; NVDA said only "Articles list".** Proven:
   Python has an exact function for this exact symptom,
   `_focus_first_row_if_unvisited` (mainframe.py:10843,
   "With no focused row, Tab into the list made NVDA say
   only 'list'"). It FOCUSES row 0 (never selects —
   selecting loads the article), and skips when a selection
   is remembered, the list is empty, a row is already
   selected, or the list itself has focus (double-read).
   The port's `render_refill_rows` set no focused item and
   the list had no focus handling. Fix: the port
   `focus_first_row_if_unvisited`, called at the end of
   every refill. One deliberate guard difference: the
   `selected_article_id` check is covered by the widget
   selection check (restores set widget selection; a stale
   remembered index from a previous view must not block
   the placeholder in a new view).

9. **NPR feeds arrive with no audio (Morning Edition, All
   Things Considered; feeds.npr.org/3/rss.xml carries zero
   enclosures — detection must come from the article
   side).** Two findings. (a) The refresh-time attach:
   story-id extraction, embed-player URL, the ondemand MP3
   regex and unescaping were all verified correct against
   the live embed page, and the compiled path fails in
   this sandbox only at TLS (`UnknownIssuer` — the egress
   proxy re-signs certificates; the same artifact as the
   B16 spike's first failure). Its behavior on Brandon's
   machine is unproven either way; no refresh-path change
   was made on unverifiable evidence. (b) PROVEN
   divergence, the manual path Brandon named: the Detect
   Audio command fetched the article page and ran only the
   in-page scan — no NPR embed-player step, which cannot
   work (the story page is TollBit-gated; the MP3 lives in
   the embed player). Fix: core `media::detect_media`
   ports Python's `discovery.detect_media` order — Sky,
   then NPR embed player, then yt-dlp `--dump-json` with
   the first cookie source — and the command's worker now
   calls it. Simplification: Python's optional
   cookie-profile argument is not reproduced
   (`cookie_sources()` returns plain source names).
   Untouched: `resolve_for_play` also lacks an NPR embed
   step, but Python's player path was not proven to call
   detect_media, so no change was made there.

10. **During playback, Ctrl+Left/Right (seek) and
    Ctrl+Up/Down (volume) did nothing.** Proven: Python's
    main frame routes these in its key handler
    (mainframe.py ~3010): Ctrl-only arrows, skipped when a
    text control or the rich view has focus, seek only
    when media is loaded, volume whenever a player window
    exists — deliberately NOT menu accelerators ("causes
    double-seeks"). The player window's own handler exists
    in the port, but the main frame's hook had no media
    block at all. Fix: the block added to the main frame
    CHAR_HOOK using core `media_arrow_action` with the
    macOS-Alt variant off (Python's main-frame block
    requires Ctrl; Alt lives in the player window),
    routing to the one player session through
    `player::dispatch_command`
    (player.rewind/forward/volume_up/volume_down), with
    the seek guard read from `player::progress_snapshot`.
    Simplification: Python's hold-to-repeat gate tames
    Windows key-repeat randomness; the port relies on
    wx autorepeat re-firing CHAR_HOOK, one step per event,
    like the player window's existing handler.

B16 status unchanged by this batch (wiring still the next
step; the spike's local proof stands). Owner-decision
items unchanged.

**v0.1.64 VERIFIED** (2026-11-06): release commit
`6c9522f`, CI run 42337290468 fully green (all three jobs
read directly), Release run 42338640517 success (3 Package
+ Publish). API verification: draft=false,
`/releases/latest` = v0.1.64, exactly 7 assets with the
fixed ship-shape names. Linux tarball contents sit at the
archive root; signature verified VALID with the committed
public key and the tamper control (flipped byte) rejected.
Local gate on the release tree: 969 tests passed, 0 failed
(956 + 13 new: core date/collapse/detect-media/menu-label
unit tests and 7 app source-scan guards). Acceptance of
the ten fixes is Brandon's NVDA pass on v0.1.64.

## B16 landed — TLS impersonation transport (2026-11-06, shipped v0.1.65)

The last Audit 8 transport gap is closed. The local spike
recorded earlier (wreq 0.16.3 + wreq-util 0.2.0 + BoringSSL
building clean, the per-call blocking bridge driving the
async client, the emulated Chrome 131 user-agent crossing
a real socket, HTTPS completing through a proxy with a
caller-supplied CA store) is now the production path:

- New core module `impersonate.rs`: `fetch(url, target,
  cookie_header, timeout)` spawns one OS thread per call
  running a current-thread tokio runtime (core is
  synchronous throughout), builds a wreq client with the
  target's emulation profile, and returns the body only
  on a 200..400 status — Python's success rule in
  `_download_via_impersonation`; the caller
  (`fetch_via_impersonation`) still rejects empty and
  interstitial bodies and cycles the targets. Certificate
  validation is never weakened (default root store), and
  the system proxy configuration is honored the way
  requests honors it.
- Profile map (`profile_for_target`): Python's targets
  are curl_cffi keys. `""`/`"chrome"` (Python's default,
  `utils.IMPERSONATE_TARGET = "chrome"`) → Chrome149;
  `"safari184"` → Safari18_5 (the nearest Safari 18
  profile wreq-util ships to curl_cffi's 18.4 — same
  fingerprint family); `"firefox"` → Firefox151. Unknown
  keys fall back to Chrome, mirroring Python's
  `impersonate_target or IMPERSONATE_TARGET`.
- Headers follow Python's `_impersonated_headers`
  division of labor: the profile owns the identity set
  (User-Agent / Sec-CH-UA / Sec-Fetch, self-consistent
  with the handshake — the loopback test captures the
  emulated Chrome 149 set on the wire); the request
  carries only the functional headers, Python's
  `_HTML_ACCEPT_HEADERS` (Accept, Accept-Language)
  plus the caller's cookie header when supplied.
- Timeout: the configured feed timeout
  (`http::timeout_secs()`), the same value the shared
  ureq agent carries; Python threads its extraction
  timeout through the equivalent call.
- Tests: profile-map unit test + two loopback tests
  (profile identity/cookie/Accept headers cross a real
  socket; a 403 yields no body). Live JA3-wall acceptance
  remains a manual check on Brandon's machine — this
  sandbox's egress is refused by the endpoints this stage
  exists for, so no live verdict is claimed from here.
- §10's B16 entry is superseded by this section. Still
  open: B17 (unchanged, needs design + Windows test
  pass) and the owner-decision items (Task 23 Deno
  packaging, Task 30 B14 workflow step, close_to_tray
  default).

## Launch reports against v0.1.64 (2026-11-06, shipped v0.1.65)

Two reports from Brandon's first v0.1.64 launch, root-caused
as one chain, plus B16's refresh half landed alongside.

**The status-line error is real, and it is the YouTube
root cause.** At launch the managed yt-dlp maintenance
tried to download the nightly binary and the status line
showed `yt-dlp download: yt-dlp download: the response
body is larger than request limit: 10485760`. Three
defects composed it: (1) `dependency::download_to_file`
read the body with ureq's `read_to_vec`, whose default
read limit is 10 MiB — every yt-dlp nightly binary is
larger, so the managed download could NEVER succeed;
(2) the error was prefixed twice, once in core
(`download_to_file`) and once in the app's status
formatter, which now shows the core string as-is (the
core strings are complete: "yt-dlp download: ...",
"install: ..."); (3) `dependency::download_bytes` (the
ffmpeg archive path) carried the same capped read and
got the same raised cap. Fix: downloads stream to disk
through the body reader under an explicit 1 GiB sanity
cap (`TOOL_DOWNLOAD_LIMIT`), a failed copy removes the
partial file (the maintenance flow verifies before
swapping, and a truncated file must never be what it
verifies), and the ffmpeg path reads under the same cap.
Regression test: `tool_download_streams_past_the_ureq_default_read_limit`
serves 11 MiB over loopback and lands it whole.

**YouTube feeds: add never completes, refresh errors.**
Verified from serrebiradio.com at Brandon's direction:
the channel page answers 200 while
`feeds/videos.xml?channel_id=UCxXMMQl_aakstul0tzu41Fg`
answers 404 with Google's generic error page for EVERY
anonymous client tried — curl (plain, browser UA, full
browser header set, CONSENT cookie, HTTP/1.1 and /2),
python requests, and curl_cffi with chrome/safari184/
firefox fingerprints; a control channel feed 404s
identically while NPR answers 200. The endpoint refuses
anonymous fetches from these IPs outright — the exact
condition Python's issue #107 comment records ("answers
404/500 at random for some channels"), whose designed
recovery is the yt-dlp uploads-playlist listing. That
listing works from the same server (real entries
returned). The Rust refresh already carried the #107
fallback, but it shells to yt-dlp — and on Brandon's
machine yt-dlp was the managed copy whose download the
10 MiB cap had just killed. One root cause, both
reports. The subscribe path had a second, independent
gap: `resolve_subscribe_url` ran a failed videos.xml
parse into the generic page-discovery chain (probe
paths, OpenRSS), which grinds and fails even though the
listing would resolve the channel at once. It now tries,
after the direct parse fails and before page discovery,
the same uploads-playlist listing refresh uses, and
subscribes with the listing's title and entries under
the original videos.xml URL — refresh then keeps its
RSS-first + fallback shape. Tests:
`crates/core/tests/impersonation_refresh.rs` covers the
stage below; the listing machinery itself was already
covered by the sources tests.

**B16 refresh half — per-feed impersonation stage.**
Section 10 recorded Python refresh's `_response_looks_blocked`
escalation as part of B16 ("both land or stay pending
together"); with the transport landed, the stage is now
wired into `feeds::fetch_feed_document_cancellable`
behind the per-feed `impersonate` setting
(`FeedFetchOverrides.impersonate`, from
`feed_settings::impersonate_value`; providers/local.py
lines 2196 and 3369-3460 are the spec). Modes: "always"
tries the impersonated fetch first (a plain-pipeline
success remains possible when impersonation fails —
Python's always attempts impersonate exclusively; the
divergence only ever turns a failure into a success and
is recorded here); "auto" makes ONE reserved
impersonated attempt when the plain fetch errors, or
when the response fails to parse and looks blocked
(challenge qualification, or status 200/403/429 — the
status half of `_response_looks_blocked`); "never"
disables the stage. The attempt is
`impersonate::fetch_bytes`: same blocking bridge as the
extractor's fetch, feed Accept header, the site's pinned
session target when one exists else "chrome", the site's
stored cookies attached exactly as the plain builder
attaches them. A recovered body follows the browser
stage's contract: cached with validators cleared, digest
never recorded. Deviations from Python's choreography,
stated plainly: Python interleaves the reserved attempt
inside its retry budget (conditional headers and all);
the Rust stage runs as a recovery stage beside the
browser stage, so an impersonation-recovered refresh
always transfers the full body and never answers 304.
Tests: `refresh_recovers_a_blocked_feed_through_impersonation`
(a loopback WAF that serves the feed only to requests
carrying the impersonated client's Sec-Fetch headers)
and `refresh_never_mode_skips_the_impersonation_stage`
(exactly one request reaches the server), plus
`impersonated_fetch_bytes_returns_the_raw_body`.


## B17 landed — privileged Chromium v20 helper (2026-11-06, shipped v0.1.66)

Brandon's 2026-11-06 order ("Finish everything d16 d17, all the d's
that are left") lifted the design-review reservation on B17. The
full Python `core/chromium_cookies.py` design is ported in
`crates/core/src/chromium_v20.rs`, wired into both import paths
(`cookies_import.rs` automatic import and the explicit Import
button in `app/src/dialogs.rs`), with the helper entry points
dispatched early in `app/src/main.rs` before the GUI starts.

What landed, against the Python spec:

- Pure crypto, platform-free: the app-bound key blob parser
  (`[u32 path_len][path][u32 content_len][content]`), all four
  derivation shapes — raw 32-byte key (flag 0), flag 1 AES-256-GCM
  with the static Chrome 127–132 key, flag 2 ChaCha20-Poly1305
  with the static Chrome 133+ key, flag 3 domain-joined (CNG
  decrypt of the wrapped AES key as SYSTEM, XOR with the static
  key, AES-GCM unwrap) — and v20 cookie decryption with the
  DB-version ≥24 SHA-256(domain) prefix strip. Decryption runs on
  ring; the derivation is covered by `crates/core/tests/
  chromium_v20.rs` (14 tests), including Python's real Chrome 130
  flag-1 test vector end-to-end (blob → derived key) and
  flag-2/flag-3 roundtrips sealed with the fixture crates.
- Key cache at Python's location and shape:
  `<data_dir>/chromium_v20_keys.json` maps sha256(b64) to the key
  re-encrypted under user DPAPI with Python's entropy string;
  writes are atomic (temp + rename). Resolution is cache-first
  and batches every missing browser key into ONE elevation.
- Windows elevation, Python's transport order: the silent
  scheduled task (`BlindRSS Elevated Helper <sha256(exe)[:10]>`,
  RunLevel Highest, registered once through a runas PowerShell
  script that also unregisters stale sibling tasks) is tried
  first; a response-file `error` is a hard failure with no
  fallback (Python parity); a transport failure falls back to
  the runas key helper; cancellation (error 1223) propagates as
  cancelled and never falls back. The elevated body impersonates
  SYSTEM (SeDebugPrivilege, candidate-process token search,
  S-1-5-18 SID check), performs the double-DPAPI unwrap, and runs
  the flag-3 CNG decrypt under that token.
- VSS staging for locked cookie databases: the elevated helper
  creates ClientAccessible shadow copies through WMI COM
  (SWbemLocator, driven through a hand-rolled IDispatch layer —
  deliberately not PowerShell, as in Python), copies the database
  plus -wal/-shm, grants Users full control with icacls, and
  deletes each shadow copy. Both import paths stage every locked
  candidate profile in one elevation and read from the staged
  copies; the staging directory is removed after the import.
- The `skipped_v20` outcome now means the key could not be
  resolved this time (elevation cancelled or failed); the import
  dialog says so and points at retrying with the prompt accepted,
  or the cookies.txt route.

Shape notes (recorded, not gaps): the Rust helper is always the
shipped-binary shape — Python's dev form (interpreter + script
path) has no analogue and none is needed. Task staleness is
checked by the current executable path appearing in the task's
query XML, matching Python's comparison. The WMI/COM and
elevation paths are compile-proven here (windows-gnu clippy zero
with -D warnings; the Windows MSVC CI leg is the build arbiter)
and unit-proven only in their pure parts; first-run acceptance —
import from a real v20 Chrome profile on Brandon's machine, one
registration prompt, silent task runs after — is Brandon's
hands-on pass, as with every Windows-only surface in this port.

**v0.1.65 VERIFIED (2026-11-06).** Release run 42346548518
completed success (all three Package jobs). Verified by
direct API reads: /releases/latest is v0.1.65, draft=false,
exactly 7 assets (BlindRSS.zip / .tar.gz / .dmg, each with
.minisig, plus SHA256SUMS); the tarball's contents sit at
the archive root; the tarball signature verifies VALID
with the tree's verify-sig harness and a flipped-byte copy
is rejected. Main CI for the tagged commit (46b3c65) was
fully green on all three OSes (run 42344699180) — the
Windows MSVC leg is B16's BoringSSL-in-CI proof. One
blemish, not in the release path: the duplicate CI run on
the tag ref failed its macOS leg on
`end_to_end_range_cache_on_loopback` (origin hits 2 -> 3,
bytes correct) — root-caused the same night to parallel
tests retuning the process-wide range-cache singleton and
fixed in the two test commits that follow v0.1.66's code
(3a0ce2d, 7908d41; see the final-sweep section).

## Final deviations sweep — Task 23, Task 30 B14, close_to_tray, and the recorded remainder (2026-11-06, shipped v0.1.67)

Brandon's 2026-11-06 order ("Finish everything d16 d17,
all the d's that are left") closed every open item in
§10. This section is the completion record; §10's list
lines were updated to point here.

**Task 23 Deno packaging — LANDED.** release.yml gained a
"Bundle tools (bin/)" step and a "Packaged smoke test"
step. All three legs stage `bin/` beside the executable
with the yt-dlp nightly binary and Deno from the same
URLs Python's build.bat/build.sh use (yt-dlp nightly
builds repo per-platform assets; denoland/deno latest,
macOS asset picked by runner arch). ffmpeg follows
Python's ensure_ffmpeg where Python has one: the Linux
leg copies the apt ffmpeg/ffprobe, the macOS leg copies
the brew ffmpeg/ffprobe. Python's Windows build bundles
no ffmpeg (it is found on the system at runtime), but the
smoke contract below requires all four tools resolvable
from the package under a minimal PATH, so the Windows leg
stages the gyan.dev static essentials ffmpeg/ffprobe
instead — a recorded, deliberate superset of Python's
Windows layout in the direction of a self-contained
package. The app's own precedence is unchanged: the
managed yt-dlp/ffmpeg copies in the data dir still win
over the packaged ones once provisioned, so the nightly
self-update behavior is preserved. The smoke step runs
the staged binary with `--blindrss-runtime-self-test`
under a minimal PATH (Unix: `env -i PATH=/usr/bin:/bin`;
Windows: system directories only) after packaging and
before signing, so a package whose tools do not resolve
from its own layout never gets signed. One caveat shared
with Python: the Linux/macOS ffmpeg is the build machine's
dynamically linked copy, exactly as Python's build ships;
a machine lacking its libraries falls back to the app's
managed ffmpeg provisioning.

**Task 30 B14 bundle-app workflow step — LANDED.** The
macOS leg's archive step now runs
`cargo xtask bundle-app BlindRSS bundle "$VERSION"`
(staging dir, bundle out dir, tag version without the
leading v) and points `hdiutil create -srcfolder` at
`dist/bundle`, so BlindRSS.app sits at the dmg root under
the fixed BlindRSS.dmg asset name. No other leg changed
shape; the Package step was split into staging and
archive steps on all legs to give the bundle and smoke
steps their place, with identical archive contents.

**close_to_tray default — DECIDED AND FLIPPED.** Python's
defaults table (core/config.py) seeds
`"close_to_tray": True`; the `.get(..., False)` fallbacks
in Python's GUI are overridden by that merged table, so
Python's effective default is True. Rust had no seeded
default and both read sites fell back to False. Both —
`app/src/main.rs`'s close veto and
`app/src/settings.rs`'s Settings checkbox — now fall back
to True, matching `minimize_to_tray`, which already did.
Stored user values are unaffected.

**Stale records corrected (already ported, verified in
the current tree).** (1) The GSMArena battery-table sort:
`normalize_gsmarena_battery_table_html` exists in
`crates/core/src/extract.rs` behind its
battery-test-v2.php3 URL gate and is called from
`article_html.rs` — the §1 table's "not ported" is a
stale snapshot; the Audit-era resolution in this file
already recorded the port. (2) Installed-browser
true-version UA detection: `user_agents::resolve` routes
"auto" through `resolve_with_installed`, which prefers a
detected installed browser at its real major version
(Chromium channel version directories and Firefox's
application.ini on Windows, with the other platforms'
detectors below them in the same module), and
`installed:<family>` modes plus `installed_choices` back
the Settings picker — the §1/§2-era "not ported" lines
are stale snapshots of a state before that port landed.

**Final verdicts on the recorded per-task deviations.**
These stand as the port's shipped shapes; each is bound
to a named platform or architecture limit, and none has
a user-facing behavior gap left to close:

- wxDragon FFI limits: the Takeout checklist uses a
  Checkbox-style TreeListCtrl (the one wxDragon control
  with native checked state) instead of a report ListCtrl,
  which has no checkbox binding in wxDragon's FFI; slider
  dragging is inferred from user events plus a 600 ms tick
  (no true drag-start/end events); per-notification
  balloon data, WebView navigation introspection
  (IsTargetMainFrame / GetNavigationAction), and
  menu-highlight help topics have no wxDragon surface.
  Revisit only if wxDragon grows these APIs.
- Silence-skip pacing: the skip policy runs on the
  player's 1 s tick and Python's 250–4000 ms diagnostic
  seek probes are not ported — they are diagnostic-only
  in Python (they emit log lines, they do not move the
  playhead policy); budgets, windows and thresholds are
  Python's.
- Caster HlsRelay: the shipped relay is the range-cache
  proxy's managed ffmpeg `-c copy` HLS route; Caster's
  piped TsSource reader internals are not reproduced.
  Playback, resume-seek correction and discovery merge
  behavior are ported and tested; physical-device
  acceptance remains Brandon's.
- Casting probe reads: probing goes through ureq with a
  Range header where Python reads the response stream;
  the byte windows can differ by a few bytes per attempt
  and the classification is unchanged.
- Range-cache test isolation (the v0.1.65 tag-CI blemish
  above): the proxy is a process-wide singleton and its
  tests now hold one static guard per test, after the
  failure was traced to a parallel test's retune landing
  between another test's retune and its entry creation.

With this sweep, every Audit 8 finding and every §10
item is fixed, superseded, or closed with a final
verdict above. The only remaining step in the migration
is the classic-branch swap, which happens only on
Brandon's explicit word.

**v0.1.66 VERIFIED (2026-11-06).** Release run 42352261344
completed success (all three Package jobs; the Windows leg
was slow but inside its timeout). Verified by direct API
reads: /releases/latest is v0.1.66, draft=false, exactly
7 assets; the tarball's contents sit at the archive root;
the tarball signature verifies VALID with the tree's
verify-sig harness and a flipped-byte copy is rejected.
Main CI for the tagged commit (7908d41) was fully green
on all three OSes (run 42351104602) — the Windows MSVC
leg compiling the B17 FFI cold is its compile proof.

**v0.1.67 VERIFIED (2026-11-06).** Release run 42356689413
completed success — all three Package jobs plus Publish,
with the new steps green on their first live run: Bundle
tools (bin/) and the Packaged smoke test on all three
legs, and the bundle-app Archive step on macOS. Verified
by direct API reads: /releases/latest is v0.1.67,
draft=false, exactly 7 assets; the tarball signature
verifies VALID and a flipped-byte copy is rejected; the
downloaded BlindRSS.tar.gz contains bin/yt-dlp, bin/deno,
bin/ffmpeg, bin/ffprobe; the Windows BlindRSS.zip contains
bin/yt-dlp.exe, bin/deno.exe, bin/ffmpeg.exe,
bin/ffprobe.exe; the BlindRSS.dmg decompresses to an APFS
image carrying BlindRSS.app (Contents/MacOS,
Contents/Resources, Info.plist) with the tool names
present. Main CI for the tagged commit (84be0d0) was
fully green on all three OSes (run 42355352077), and the
local gate before it ran 990 tests green.

With v0.1.67, the finish-everything order is complete:
B16 (v0.1.65), B17 (v0.1.66), Task 23, Task 30 B14, the
close_to_tray parity flip, and every recorded deviation
in section 10 is fixed, superseded, or closed with a
final verdict. The only step left in the migration is
the classic-branch swap, on Brandon's explicit word.

## User report — Veto() debugging-check dialog on update (2026-11-07, shipped v0.1.68)

Brandon updated (the updater worked) and hit wxWidgets'
debugging-check dialog: wx/event.h "assert m_canVeto
failed in Veto(): call to Veto() ignored (can't veto this
event)". Root cause, proven from the wx 3.2 sources bundled
in the build tree: the asserting function is
wxCloseEvent::Veto(), which wxCHECKs m_canVeto — a close
event generated by a forced close (wxWindow::Close(true)),
a Windows session end, or an app-shutdown destroy carries
CanVeto() == false, and calling Veto() on it is a contract
violation. The app had exactly two CLOSE_WINDOW handlers
and both vetoed unconditionally: the main frame's
close_to_tray handler (app/src/main.rs) and the Podcast
Archive dialog's busy-close funnel (app/src/dialogs.rs).
Either can receive a non-vetoable close — the frame's on
the updater's restart/exit path, the dialog's on shutdown.

Which binary showed the dialog: not the new one. The
shipped Windows executables of v0.1.61, v0.1.65, v0.1.66
and v0.1.67 were checked directly — the string "m_canVeto"
occurs 0 times in each, i.e. wx's checks are compiled out
(wx-asserts off since v0.1.61) and those builds cannot
raise this dialog. The dialog therefore came from the
pre-v0.1.61 instance Brandon was updating FROM, while it
handled a close during the update. The code defect was
nevertheless live in every build — in an assert-free build
Veto() silently does nothing, which for the frame handler
also meant the hide-to-tray step ran against a frame that
was closing anyway.

Fix: both handlers now check the event's can_veto()
first (wxdragon exposes it; its own docs say veto "should
only be called if can_veto() returns true"). A vetoable
close behaves exactly as before; a non-vetoable close
proceeds natively, which is the only legal outcome. The
WebView navigating vetoes are a different event class
(wxNotifyEvent, vetoable by construction) and are not
implicated — the assert line is wxCloseEvent's. Guard:
the ui_parity_tests source scan
close_handlers_only_veto_vetoable_events requires every
CLOSE_WINDOW handler in the app crate to check can_veto()
before any veto (floor: the two known handlers).

## Audit 10 — skip silence (2026-11-07, shipped v0.1.68)

Brandon ordered a fresh audit of Skip Silence against
Python. Scope compared line by line: core/audio_silence.py
(detection), the silence regions of gui/player.py (scan
lifecycle, publish, _maybe_skip_silence, the verify block,
the floor ratchet, _note_user_seek), core/config.py
DEFAULT_CONFIG silence defaults, gui/dialogs.py settings,
against crates/core/src/silence.rs and the player wiring
in app/src/player.rs plus app/src/settings.rs.

Verified at parity, no change: the VAD detector (WebRTC
VAD ANDed with the per-frame RMS/dBFS check, frame sizes
10/20/30 coerced to 30, aggressiveness clamped 0..=3), the
RMS StreamingSilenceDetector fallback (window, ceil
min-windows, finalize semantics), merge_ranges /
merge_ranges_with_gap arithmetic, the publish transform
(EXPAND by padding, merge across the gap — never shrink),
the paced ffmpeg scan (lead, 5 s-of-audio progress
cadence, abort kills + reaps, error keeps
already-published ranges), every config default
(threshold -38 dB, min 700 ms, window 25 ms, padding
60 ms, merge gap 260 ms, backoffs 360/1400 ms, lead
300 s, scan delay 4 s, threads 2/1, sample rates
16000/8000) and every remote adjustment (aggressiveness
0, threshold capped at -52 dB, min 1200 ms, gap 200 ms),
the skip state machine (all guards, 2.5 s/5 s cooldowns,
resume backoff capped at 1000 ms remote, the ±100 ms
inside-span window, the start-1000 early break, target <
floor refusal, the ±1000 ms loop guard, the verify
re-seek — 700/1200 ms landing thresholds, 0.35/0.5 s
minimum wait, one attempt inside a 3 s/5 s window — the
floor ratchet with its reset and allow-backwards window,
and the tail bookkeeping), scan queueing on load with
generation staleness, and the settings surface (Python
exposes only the checkbox; the port's extra threshold /
minimum fields are a superset). The 250–4000 ms probe
CallLaters were re-read: _silence_skip_probe_tick only
logs — diagnostics, correctly not ported as behavior
(the verify re-seek above is the real mechanism, and it
is ported).

ONE gap found and fixed: the tick cadence. Python
restarts its player timer at 280 ms while Skip Silence
is enabled ("Run the timer faster when skip-silence is
enabled so jumps feel snappier"); the Rust player ticked
at a fixed 1000 ms, so a skip could land up to a second
after the position entered a silent span (and the verify
re-seek's sub-second waits were quantized to whole
seconds). The player now resolves tick_fast from the
skip_silence config at every load (in queue_silence_scan,
before its early return) and the tick applies 280 ms /
1000 ms to the wx timer as the setting changes. Guard:
player_tick_speeds_up_while_skip_silence_is_on.

Two deliberate non-changes, recorded: (1) Python
evaluates _maybe_skip_silence on every timer tick with
the raw backend position, playing or not — paused inside
a silent span it fires one seek while paused. The port
evaluates only while Playing; the seek then fires on
resume instead. Kept: firing seeks on a paused/stopped
player for a position it is not advancing serves no
listening purpose, and the difference is invisible in
normal use. (2) Python also drops its timer to 250 ms
while a resume seek is pending; the port's seek verifier
is a separate bounded machine designed around
observation ticks, and it inherits the 280 ms cadence
whenever Skip Silence is on. Not adopted.

## Release notes + Detect Audio row refresh (2026-11-07, Brandon's order)

Brandon, updating 0.1.66 → 0.1.67: the updater showed only "Full
Changelog: https://github.com/serrebidev/BlindRSS-Rust/compare/v0.1.66...v0.1.67"
— the Publish job used softprops/action-gh-release with
`generate_release_notes: true`, so GitHub's auto-generated compare text
was the whole body, and ship-shape's update dialog displays that body.
BlindRadio's mechanism is ported: a `CHANGELOG.md` at the repo root
holds one human-readable `## X.Y.Z` section per release (backfilled
0.1.55–0.1.68 individually, 0.1.0–0.1.54 combined); the Publish job now
checks out the tag, extracts that tag's section with the same Python
loop BlindRadio's workflow uses, fails the publish when the section is
missing, and passes the text as the release `body_path`. The published
v0.1.64–v0.1.67 release bodies were replaced with their CHANGELOG
sections the same day, so the updater shows real notes immediately.

Detect Audio focus bug (Brandon, same report): on a successful
detection the completion handler called `refill_articles`, which
rebuilds every row and drops the list's selection and focus, leaving
the user on the first article of the folder to hunt for the article
manually. Python's `on_detect_audio` never refills: it persists the
media, invalidates the media-column label, and refreshes only that
article's row (`_refresh_article_in_list`). The handler now matches:
after the in-memory row update it rewrites only that row's Media cell
in place (`set_item_text_by_column` at `state.media_column`, label
from `row_annotations.media_for`, which computes from the article
itself). Guard test `detect_audio_updates_the_row_in_place`: the
AudioDetected handler window must not contain `refill_articles` and
must contain the in-place cell write. Deviation note: under a
"no media" media filter the row now stays visible until the next
refill, exactly as Python's in-place refresh leaves it.

**v0.1.68 VERIFIED** (2026-11-07): commits `b3e0197` (veto guards +
Audit 10 cadence) and `d45e44f` (release notes + Detect Audio row
refresh), tag on `d45e44f`. Local gate: fmt clean, clippy zero
(workspace + core windows-gnu), 993 passed / 0 failed, build ok. Main
CI 42388997565 completed/success (all 3 jobs read directly); Release
run 42390517256 completed/success (all 3 Package jobs + Publish).
API verify: Latest = v0.1.68, draft=false, exactly 7 assets; the
release body is the CHANGELOG 0.1.68 section verbatim — the new
extraction step worked on its first live run. Signature: SIGNATURE
VALID on BlindRSS.tar.gz + tamper control ok.

## Refresh place-keeping (2026-11-07, Brandon's report)

Brandon: "if I'm reading through the list of articles, and a
refresh happens, I get jumped to the top of the list." Audit:
the refresh-completion drain (AllDone) calls `rebuild_tree` +
`refill_articles`; the refill deletes every row and re-inserts,
and nothing restored the widget's selection or focus — the same
missing machinery behind the Detect Audio jump, there fixed by
not refilling. Python never loses the place: EVERY article-list
rebuild captures the focused article id, the selected article id
and the top article id first (`_capture_list_view_state`) and
restores them after the render (`_restore_list_view`: re-select,
re-focus, EnsureVisible, scroll-top trick, placeholder focus
only when nothing was restored), and the refresh completion
funnels into those same reload paths. The port now matches at
the funnel: `refill_articles` captures a `ListPlace` (selected
id from the widget, falling back to `state.selected_article`;
focused id from the widget's focused row; a flag for the paging
placeholder row) before `refill_inner` and restores it after the
Thaw — selection re-set at the article's new index (and
`state.selected_article` corrected to it), focus re-set +
`ensure_visible`, placeholder focus restored via
`_restore_load_more_focus`'s shape when the focus sat on the
placeholder. Articles that left the view restore nothing, as in
Python. One shape difference, recorded: wxdragon exposes no
top-item getter, so scroll position follows the focused row via
ensure_visible instead of Python's explicit top-item restore —
the focused row is the one the screen reader tracks. Guard test
`refill_restores_the_reading_place`: capture/restore calls must
bracket `refill_inner` inside `refill_articles`.

## Refresh announcements opt-in (2026-11-07, Brandon's order)

Brandon: every refresh speaks how many feeds and new articles
arrived; he wants those spoken alerts OFF by default, turned on
in Settings by the user. Deliberate deviation from Python
(whose announcement events all default to MODE_BOTH): the
refresh family now defaults to none via `event_default` in
crates/core/src/announce.rs — `start_update` and `stop_update`
keep their ids and settings rows but default to Mode::None,
and the refresh-completion line ("Refresh complete: N new
articles") moved off the catch-all `general` event onto a new
appended event `refresh_complete` ("Refresh results"), also
defaulting to none. Stored user choices still win
(`from_value` merges by event id, so existing configs are
untouched in meaning). The Settings → Announcements page
iterates EVENTS, so the new row appears at the end with no UI
change. Status text and the refresh sounds are unchanged —
speech only. Tests: announce_slice defaults test reworked
(refresh family none, everything else both) plus
`refresh_results_event_exists_and_is_opt_in`.

**v0.1.69 VERIFIED** (2026-11-07): commits `6d3beaf` (refill
capture/restore) and `6924638` (refresh announcements opt-in),
tag on `6924638`. Local gate: fmt clean, clippy zero with real
exit codes (workspace + core windows-gnu), 995 passed / 0 failed
on the final tree (one OPML test flaked once with a readonly-DB
error in the run started right after a SIGTERM-killed compile;
it passed in isolation and in the clean full re-run). Main CI
42399698521 completed/success (all 3 jobs read directly);
Release run 42401122453 completed/success (all 3 Package jobs +
Publish). API verify: Latest = v0.1.69, draft=false, exactly 7
assets; the release body is the CHANGELOG 0.1.69 section
verbatim. Signature: SIGNATURE VALID on BlindRSS.tar.gz +
tamper control ok.

## Settings audit: refresh-on-start + update frequency (2026-11-07)

Brandon reported two things. (1) "Automatically refresh feeds
upon start" appearing on BOTH the General and the Feeds &
Articles tabs, to be removed from General. Audit result: the
checkbox exists exactly once — Feeds & Articles
(settings.rs, `feeds.win`) — in every shipped build
v0.1.61–v0.1.68, in current main, and in Python (feeds_panel).
No General copy exists to remove; the end state he asked for
is the shipped state. The likely mechanism of the report is
NVDA browse mode flattening a wx notebook's pages into one
accessibility tree, making the Feeds controls readable while
General is the selected tab. No code change made for (1); the
finding was reported to Brandon as-is. (2) The General tab's
"Check for updates:" frequency (translation updates) had no
disabled option. Added: value `never` / label "Never",
appended to the choice (indices 0–3 unchanged); core
`translation_updates::is_due` short-circuits "never" to false
so automatic checks stop, while the manual Check for Updates
button (which calls check_and_update directly) keeps working.
Deliberate deviation from Python, whose choice has the same
four values and no Never.

## Locked database + refresh stalls (2026-11-07, Brandon's report)

Brandon: many feeds failing with `feed cache: database is
locked`, and the app hard to navigate while feeds refresh.
Root cause, a Python-parity miss in connection setup: Python
`core/db.py` sets `PRAGMA journal_mode=WAL`,
`synchronous=NORMAL` and `busy_timeout=60000` on EVERY
connection. The port set none of them — rollback journal mode
with busy_timeout=5000 (set only inside init_schema). In
rollback mode the 16 refresh workers' writes block the UI
connection's reads (navigation stalls), and contention past
5 s fails refreshes outright with the exact error Brandon
pasted. Fixed: `Db::configure` applies Python's pragma set on
every `Db::open`/`open_or_create`; init_schema's timeout is
60000; the archive store's own open() gets the same set. WAL
persists in the file, so an existing library heals on first
open. Regression tests:
`db_files_run_in_wal_mode_with_python_connection_setup`.

## NPR audio per session (2026-11-07, Brandon's report)

Brandon: NPR program feeds (feeds.npr.org/7/rss.xml given)
still refresh with no audio; "audio should be discovered and
fetched per session". Audit against Python providers/local.py:
(1) Python resolves media per entry in phase 1 of EVERY
refresh — new and existing rows alike — and the existing-row
update stores the fresh result (a story whose audio posts
late gains it next refresh; stored media the resolver no
longer confirms is cleared). The port resolved only at first
insert, so every already-stored NPR article stayed silent
forever. (2) Python never fetches NPR feeds conditionally
(`use_conditional` excludes them) and has no unchanged-body
shortcut; the port sent validators and took the body-digest
fast path, so NPR stores often never ran at all. (3) The
extractor itself (story-id shapes incl. nx-s1-…, embed player
first, story-page markers) was already at parity — verified
live against feeds.npr.org/7/rss.xml (10 items, zero
enclosures) and the embed page for a current story. Fixes:
media resolution moved above the existing/new branch in
store_parsed (the existing-row sync already writes media,
including clearing); fetch_feed_document_cancellable treats
NPR feed URLs as always-full (no validators, no digest
shortcut). Regression test:
`npr_audio_is_resolved_per_session_for_existing_articles`
(three sessions: audio posts late → gained; resolver stops
confirming → cleared; second fetch carries no If-None-Match).

## Settings slow open + instability (2026-11-07, Brandon's report)

Brandon: Settings takes a few seconds to open; after making
changes and saving, the app loses stability and finally
crashes. Root cause found in the Media Player page build:
`media_backend::soundcards` constructed a FULL VlcPlayer —
libVLC instance + media player + output set + equalizer —
synchronously on the UI thread, just to enumerate audio
devices, then destroyed it. libVLC instance creation takes
seconds (the open delay), and churning full player instances
in process is the destabilizer. Python gui/dialogs.py does
the opposite on every point: the choice opens instantly, a
background thread enumerates through a MINIMAL instance
(`--no-video --aout=mmdevice`, no player object), and the
result populates the choice afterwards. Ported: vlc.rs gains
`enumerate_soundcards` (minimal instance; the device-list
walk is factored into `device_list`, shared with
VlcPlayer::soundcards); the Settings build starts with
System Default + the saved device, a worker thread
enumerates, and a 100 ms dialog timer swaps the real list
in, preserving the current selection; the save path reads
the swapped list through the shared Rc. Guard test:
`settings_enumerates_soundcards_off_the_ui_thread`. The
crash half is attributed to the instance churn — the only
libVLC lifecycle abuse in the save/open path; the live
player's apply_preferred_soundcard routes to the existing
session and creates nothing.

**v0.1.70 VERIFIED** (2026-11-07): tag on `763981f`; main CI
42405966063 success; Release run 42407317995 success (all
three Package jobs + Publish). API verification: Latest =
v0.1.70, draft = false, exactly 7 assets (BlindRSS.zip,
BlindRSS.tar.gz, BlindRSS.dmg, each with .minisig, plus
SHA256SUMS), body identical to the CHANGELOG.md 0.1.70
section. Signature verified locally against the compiled-in
public key: SIGNATURE VALID, tamper control rejected a
flipped byte. 995 tests on the ship tree.

WAL fallout found by the gate and Windows CI before v0.1.71
tagged (recorded here so it is not re-diagnosed): (1) the
journal flip ignores busy_timeout — `PRAGMA journal_mode`
needs a momentary exclusive lock and SQLite does not run the
busy handler for it, so the batch32 concurrent-open test
failed on Windows while Linux passed; Db::configure now sets
busy_timeout first and retries the flip briefly (shared with
the archive store). (2) A WAL file's -wal/-shm sidecars pair
by NAME: core_slice's fixture copies used fixed temp names,
so a stale -wal resurrected the previous run's state into a
fresh copy and mark_read_roundtrip flaked; fixture_copy now
uses unique names and clears sidecars first.

**v0.1.71 VERIFIED** (2026-11-07): tag on `f5e2ad6`; main CI
42412907172 success (all three jobs, Windows included after
the WAL-flip retry); Release run 42414336879 success (all
three Package jobs + Publish). API verification: Latest =
v0.1.71, draft = false, exactly 7 assets, body identical to
the CHANGELOG.md 0.1.71 section. Signature verified locally:
SIGNATURE VALID, tamper control rejected a flipped byte. 998
tests on the ship tree. Ships the three fixes above: WAL
connection setup (locked-database errors + refresh stalls),
NPR audio resolved per session, and Settings' async minimal
soundcard enumeration.

## Patois adoption (Brandon's order, 2026-11-07; ships in v0.1.72)

Translations moved from the hand-rolled core catalog
(core/i18n.rs + core/translation_updates.rs, both deleted)
to Quin's patois crates, the system Paperback, ship-shape
and wx-utils use: patois 0.3 (wx feature) + patois-build
0.1 in the app crate. What changed:

- Catalogs moved locale/<lang>/LC_MESSAGES/blindrss.po ->
  app/po/<lang>.po and locale/blindrss.pot ->
  app/po/blindrss.pot, contents unchanged (all 15 languages,
  still the Python-parity msgid set; the .pot is NOT
  regenerated for this move, so no translation can be lost
  to extraction gaps). app/examples/gen_pot.rs regenerates
  the template going forward.
- Catalogs compile at build time (msgfmt via
  patois-build::compile_translations in app/build.rs) and
  embed in the binary, compressed. No locale folder ships;
  release.yml's `cp -r locale` packaging step is removed.
- All UI strings now go through patois::t (242 crate::tr
  call sites + main.rs' own, renamed mechanically). The
  app's tr(), the OnceLock Catalog, locale_roots() and the
  BLINDRSS_LOCALE override are gone.
- Language selection: patois::wx::WxTranslationManager,
  owned in a OnceLock, initialized before any UI (system-
  best language), then the saved config `language` applied
  via set_language; wxWidgets stock strings ride the same
  language through embed_wx_translations!. The Settings
  picker keeps its exact shape ("Automatic (system
  language)" + "Native name (code)", sorted, requires
  restart) but lists LanguageManager's embedded languages.
- SUPERSEDED: the downloadable translation-updates
  lifecycle (Task 29 / Audit 8 O16 — override tree, ETag
  fetch from this repo's main, version-transition reset,
  the Settings Translation Updates group with its enable
  checkbox, frequency choice incl. the v0.1.70 Never
  option, manual Check for Updates button, and the watcher
  tick's due check) is deleted with the feature it served:
  under patois, translations ship inside releases. The
  translation_updates_* config keys are inert leftovers.
  i18n_slice.rs (the O16 test file) was deleted with its
  subject; batch22/i18n_help lost only their TU/catalog
  tests.
- ui_language() survives for the non-gettext consumers
  (help guides, BLINDRSS_UI_LANGUAGE) and now returns the
  chosen code verbatim / the OS locale instead of the
  primary subtag — the old reduction made the nl_BE
  catalog and guide unreachable; both work now.
- Guards (app tests): the 15 catalogs are embedded (fails
  loudly when a build lacked gettext), German lookup +
  English fallback round-trip, patois-build's catalog
  damage lint over app/po (passes on the inherited
  catalogs), and its source lint over app/src.
- Known seam: ship-shape 0.5.0 still uses patois 0.2, so
  its updater strings stay English until it moves to 0.3
  (patois' own upgrade rule); the app's catalog is 0.3.
  Brandon's machine acceptance: switch the interface
  language in Settings, restart, and confirm the UI (and
  stock dialog buttons) render in the chosen language.

**v0.1.72 VERIFIED** (2026-11-07): tag on `f96c739`;
main CI 42435516264 success (all three jobs — Windows
green only after four workflow-fix commits got msgfmt
onto every platform: gettext apt step on Linux, MSYS2
gettext on Windows, and the explicit
`${RUNNER_TEMP}\msys64\usr\bin` PATH append, because
setup-msys2's path propagation never reached the cargo
steps; the two embedding guard tests caught every one
of these silent no-translation builds and were never
weakened). Release run 42437805928 success (3 Package
jobs + Publish). API read: Latest = v0.1.72,
draft=false, exactly 7 assets, release body identical
to this CHANGELOG's `## 0.1.72` section.
BlindRSS.tar.gz signature VALID against the compiled-in
key, tamper control rejected. Local gate on the ship
tree: 995 tests passed, clippy zero on Linux and
windows-gnu.

## Settings close plays a stray chime (Brandon's report, 2026-11-08; ships in v0.1.76)

**Report (Brandon, 2026-11-08, on v0.1.74):** "as I
close BlindRSS Rust's Settings dialog, I hear an extra
chime or ding as if something has gone wrong. But
nothing goes wrong and the app stays alive."

**Root cause (from the wx sources in the build
tree):** the app itself plays no sound on that path
(no bell/beep call exists in the app or core announce
code; on MSW wxBell is MessageBeep, and wx calls it
only from validators and propgrid, neither used here).
wxDialogBase::OnCharHook answers Escape through
SendCloseButtonClickEvent, which calls
EmulateButtonClickIfPresent(GetEscapeId()) —
src/common/dlgcmn.cpp: it finds the button carrying
the escape id and clicks it. Settings set its escape
id to 5101 but built its Cancel button with an
anonymous id, so FindWindow(5101) found nothing, the
emulation returned false, OnCharHook skipped the key,
and it fell through to the native Windows dialog
handling, which beeps and only then closes. The
affirmative side had the same hole: the affirmative
id defaulted to 5100, carried by no button (Enter in
a field is a primary close path since v0.1.74 made
Save the default button). Python's SettingsDialog
has neither hole: `CreateButtonSizer(wx.OK |
wx.CANCEL)` builds its buttons with wxID_OK (5100)
and wxID_CANCEL (5101) on them. Rig check (Xvfb +
openbox, 2026-11-08): on the v0.1.76 build, Enter in
the dialog saves (config written) and closes, and
Escape closes without saving — behavior identical to
the v0.1.75 build driven the same way. (The rig's
known key-leak artifact, where the Return release
re-opens Settings after the modal closes, appears on
both builds alike and is not part of this change.)

**Fix (Python parity):** the Save button is built
with id 5100 and the Cancel button with id 5101 (the
same values the codebase already uses as its modal
result codes), and the dialog calls
set_affirmative_id(5100) beside its existing
set_escape_id(5101). The click handlers are unchanged
(they are bound to the button objects, not the ids),
and the end_modal(5100/5101) results are unchanged.
Guarded by
settings_buttons_carry_the_standard_dialog_ids.
Brandon's retest on Windows is the acceptance check:
closing Settings — Enter in a field above all — must
be silent.

VERIFIED 2026-11-08: local gate green (fmt, clippy
-D warnings, 1005 tests, build). Main CI 42514860532
success on all three OSes; tag v0.1.76 on 169ddaa;
Release run 42517559278 success (three Package jobs +
Publish). Direct API read: Latest = v0.1.76,
draft=false, exactly the seven contract assets,
release body identical to the CHANGELOG 0.1.76
section. Signature: BlindRSS.tar.gz verifies VALID
against the compiled-in key, tamper control rejected
(verify-0176/).

## Caret loss, refresh focus steal, GitHub 504, MacRumors server junk (Brandon's reports, 2026-11-09; v0.1.79)

**Reports (Brandon, 2026-11-09, on v0.1.78), four items:**
(1) Tab / Shift+Tab still returns to the very top of the
article, to the first line — the v0.1.75/v0.1.77 place
fixes did not hold. (2) A feed refresh completing while
he works the article list moves focus into the Feeds tree.
(3) GitHub-based feeds fail (e.g.
https://github.com/nvaccess/nvda/commits/master.atom),
with and without VPN; they worked on v0.1.75. (4) The
MacRumors footer/rail junk is still at the bottom of
articles.

**Item 1 — root cause and fix.** The v0.1.77 diagnosis was
wrong: wxDragon does not run same-event bindings in
reverse connection order consuming at the first — its C++
`WxdEventHandler` dispatches to the Rust closures in
CONNECTION order, resets Skip(true) before each, and only
stops when one vetoes, so both focus halves already ran in
v0.1.75 and v0.1.78 alike, as does default handling. The
restore ran yet never held. The rewrite stops depending on
that fragility: the reader's place (text, caret, selection,
placement sequence) lives in State; ONE set-focus binding
marks read and restores; key-up/left-up track every caret
move (the control is read-only, so moves never change
text); focus-out re-records the text but keeps a
mid-article record over a spurious caret-0 reading (a real
move to 0 always passes through key/mouse first); the
restore applies synchronously AND from a 10 ms one-shot
past native handling, generation-guarded so a newer
placement (reader write, Find match) drops a stale
snapshot; every programmatic write funnels through
`set_reader_text`, which skips identical text (Python
`_set_article_reader_text`'s memo — a redundant write
resets the caret for nothing); Find records the landed
match as the place and focuses after its borrow ends
(set_focus fires the binding synchronously, so focusing
inside the place borrow would panic). Regression test
`article_text_keeps_its_place_across_focus_changes`
rewritten for the merged shape (one binding, both halves,
deferred leg, tracker events, write funnel, Find record).

**Item 2 — root cause and fix.** Three defects shared the
blame. (a) Refresh completion rebuilt the tree
unconditionally — Python's own comment says a full rebuild
"disturbs screen-reader focus", and the port skipped its
signature gate. The completion block now computes
`tree_signatures` (node structure + unread counts) over
fresh feed rows (Python reloads feeds before its tree
pass), skips the rebuild when both match, patches labels
in place on a counts-only change (`patch_tree_labels`,
Python `_patch_tree_unread_labels`), and rebuilds only on
structural change; every rebuild records the signatures.
(b) Rebuild-time `select_item`/`set_item_state` fire
selection events synchronously while State is mutably
borrowed, so every same-view rebuild/refill panicked with
"RefCell already borrowed" (caught at the wxDragon
trampoline, visible all over blindrss.log) and corrupted
`focus_list` plus the expansion memory. The four
selection/expansion handlers now `try_borrow_mut` and skip
when borrowed — exactly Python's `_updating_tree` /
`_updating_list` semantics. (c) Belt and braces: the
completion block captures which surface holds focus and
restores it after the borrow drops, covering the rebuilds
that still run. Regression test
`refresh_completion_keeps_focus_and_skips_unchanged_tree`
holds the gate, the patch, the restore, and the guards.
Follow-up diagnosed, not fixed: modal dialogs hold `&State`
across `show_modal` while the 150 ms tick borrows mutably,
so a tick crossing an open modal still panics (caught;
one drain delayed). That needs dialogs-signature surgery
and is separate work.

**Item 3 — root cause and fix.** Not a code regression
(no fetch-path change between v0.1.75 and v0.1.78):
Brandon's imported GitHub session cookies went stale, and
GitHub intermittently answers 504 ("Unicorn!") to the
stale logged-in session on .atom feeds while the anonymous
request always succeeds — reproduced live: anonymous 6/6
success, cookie-carrying flaky 504, identical content when
it does answer. Both feed fetch paths (`fetch_response`
for refresh, `fetch_and_parse` for subscribe/discovery)
now retry once without the jar (keeping validators,
Referer and per-feed custom headers) when the
cookie-carrying request draws 413/431 (Python
`_retry_feed_without_site_cookies`, issue #101) or
500/502/503/504, and adopt the retry only when it is a
non-error response — otherwise the ORIGINAL error stands
(a broken server, or a private feed whose cookies the
anonymous request correctly lacks). Live-confirmed: 504
with cookies, clean retry, 20 entries. Unit tests for the
decision/adoption rules plus a localhost wiring test
(504-with-Cookie / 200-without) in refresh_slice.rs.

**Item 4 — root cause and fix.** Two bypasses, one harden.
(a) The footer anchors could split across extraction
lines ("To Subscribe or\nUnsubscribe") that only glue
into the verbatim sentences when the reader text
collapses — after the strip already ran — and the page
uses a curly apostrophe in "Apple's". The byte-cut anchors
are now whitespace-tolerant regexes (straight-or-curly
apostrophe), still case-sensitive and still refusing a
bare "Push Notifications" (a how-to could bear that
heading). (b) Proven by Miniflux API reads: 17 of 25
stored MacRumors bodies carry the "Popular Stories" rail,
and neither the fetch-content arbitration nor the stored-
body display ran any postprocess — the strips only ever
saw local-cascade output. The arbitration now strips the
server text with the article URL BEFORE the length
comparison (boilerplate counts toward neither "more
complete" nor the adopted text), and stored Miniflux
bodies run the MacRumors strip at display time
(`hosted_body_for_display`: reader selection + re-render,
copy-text, feed description, article history; the database
keeps the raw server text; MacRumors-gated, local
publisher bodies untouched). Regression tests: split/curly
anchors in batch25_slice.rs, and the
`miniflux_server_text_runs_the_site_postprocess` scan for
both application points.

## MacRumors credits and glued footer, focus restore, refresh progress (Brandon's reports, 2026-11-08; ships in v0.1.77)

**Reports (Brandon, 2026-11-08, on v0.1.75), four items:**
(1) MacRumors image credits are still in articles, and
worse than before: the credit glues onto the following
paragraph ("Image credit: MacRumors forum member
sniffiesIf you'd rather keep the logo dark, ...";
example:
https://www.macrumors.com/how-to/turn-glowing-apple-logo-macbook-pro/).
(2) MacRumors articles still end with the site footer,
in his copy as one fully glued blob ("Push
NotificationsApple's Safari browser includes a feature
that allows users to subscribe to "push" notifications
... support@macrumors.com.To Subscribe or
UnsubscribeVideosGuides ... Other Stories1 day ago by
Joe Rossignol ..."). (3) Tab / Shift+Tab still returns
the focus to the very top of the article text, "from
anywhere in the text". (4) The Python app's status bar
says which feed is being refreshed as a refresh runs
("refreshing feed x, y of z"); the Rust app only said
"refreshing 55 feeds" at the start.

**Item 1 — root cause and fix.** The credit is not a
figcaption (so the v0.1.75 figcaption drop never saw
it): MacRumors prints it as a short standalone element
after the image — `<div class="center-wrap"><em>Image
credit: MacRumors forum member <a ...>sniffies</a></em></div>`
— and text conversion glues it to the next paragraph.
New `drop_image_credits` in extract.rs removes any
short standalone element (div, p, span, em, aside,
figure, section, td; raw span at most 1200 bytes,
collapsed text at most 300 chars) whose text starts
with "Image credit:" or "Image credits:". Outermost
match wins; a long element that merely opens by
discussing image credits is body text and survives
(regression-tested). Wired into both pre-passes beside
drop_figcaptions / drop_pswp_captions:
extract_article_opts and article_html's
clean_article_html. Probe-verified against the real
how-to page fetched from this network: before the fix
the classic output carried the credit glued to the
prose and the rich output carried it as its own line;
after, "Image credit" and "sniffies" count zero in
both and the prose is intact. (Verification note: an
early probe run appeared to clear the function while
the outputs still carried the credit — that run was in
fact the stale pre-fix binary, because the rebuild had
died on a bindgen libclang failure after a VM package
wipe while its wrapper command still exited 0. The
function itself was proven directly, then the full
cascade re-probed on a genuinely fresh build.)

**Item 2 — root cause and fix.** The v0.1.75 strip
only cut at paragraph starts. In the page variant
Brandon's app is served, the footer arrives glued —
onto the last body paragraph or as one blob — so no
paragraph ever starts with a marker. (The variant does
not reproduce from this network: the curl fetch of the
same how-to page contains no footer at all, the same
fingerprint split recorded in the v0.1.75 round. His
pasted text is the fixture.) strip_macrumors_boilerplate
now also cuts at the footer's unambiguous boilerplate
anchors wherever they appear in the kept text: the
"Apple's Safari browser includes a feature" pitch
sentence (taking an immediately preceding "Push
Notifications" heading with it), "To Subscribe or
Unsubscribe", and "If you have any questions or
problems, email support@macrumors.com". A bare "Push
Notifications" substring is deliberately NOT an anchor
(a how-to could legitimately bear that heading); the
pitch sentence is the signature. Regression tests feed
his pasted blob shape through postprocess, glued and
subscribe-only.

**Item 3 — root cause and fix.** The v0.1.75
keep-place pair on the main window's reader was dead:
wx's SearchDynamicEventTable runs dynamically bound
handlers for one event in REVERSE connection order and
stops at the first handler that does not skip
(wxWidgets src/common/event.cpp). The pair was bound
at the reader's build site; the mark-as-read-on-focus
handler is bound later and never skips, so on
SET_FOCUS it always ran first and consumed the event —
the restore half never ran at all. (Kill-focus has no
competitor, so the capture half worked, which is why
the failure looked like "restore doesn't stick".) The
pair now binds immediately AFTER the mark-read focus
handlers in build_main_frame, with the rule recorded
at the site; the scan test asserts the order. The
article window reader, article history, and Feed
Errors detail have no competing focus handlers (grep
verified) and keep dialogs::keep_text_place as wired
in v0.1.75. Brandon's retest is the acceptance check,
above all in the main window; if the article window
still loses its place on Windows after this, that is
a separate MSW-specific behaviour needing fresh
investigation, not covered by this fix's claim.

**Item 4 — Python parity.** Python
gui/mainframe.py: `_begin_refresh_activity` posts
"Refreshing feeds..." for a full refresh (total = the
feed count), "Refreshing feed: {title}..." for one
feed (no total — the title says more than "1 out of
1"), "Refreshing category: {name}..." for a category,
"Refreshing imported feeds..." for OPML imports;
`_note_refresh_progress_feed` counts DISTINCT feed ids
(retries count once) and `_refresh_progress_counts`
never lets the total fall behind the done count;
`_set_feed_activity_status` posts "Checked {done} out
of {total}" per success and "Error checking: {title}
({done} out of {total})" per failure (without a total:
"Checked: {title}" / "Error checking: {title}"). The
Rust port mirrors this: State gains refresh_total and
refresh_seen (a HashSet of feed ids); the four start
sites set them and use Python's start lines (the old
"Refreshing {} feeds..." and "Refreshing {}
({} feeds)..." lines are gone; the single-feed line
becomes "Refreshing feed: {title}..."); the drain's
FeedDone arms count and post the per-feed lines,
replacing the error arm's "{title}: {error}" status
(the error itself still lands in Feed Errors). The
end-of-refresh line is unchanged ("Refresh complete:
N new articles" is a Rust-side summary Brandon has not
objected to; his report was about progress DURING the
refresh). Guarded by the scan test
refresh_status_counts_feeds_as_they_finish. OPML-import
refresh keeps its existing wording (Python's
"Refreshing imported feeds..." has no per-feed
counterpart wired there either — recorded as a
deviation candidate if Brandon flags it).

Regression coverage: core tests
macrumors_image_credit_div_is_dropped_not_glued and
macrumors_glued_footer_is_cut_at_its_anchors in
crates/core/tests/batch25_slice.rs; app scan tests
refresh_status_counts_feeds_as_they_finish and the
extended article_text_keeps_its_place_across_focus_changes
(bind-order assert). Local gate and release
verification are recorded below once run.

## Extraction junk, reading-place loss, find bugs, tree shape (Brandon's reports, 2026-11-08; ships in v0.1.75)

**Reports (Brandon, 2026-11-08, on v0.1.74), five items:**
(1) Ars Technica full text carries the figure caption
("Multicolored lines show undersea Internet cable
routes...") plus duplicated "Credit:" / "TeleGeography"
lines, and a rail tail of unrelated headlines. (2) The
article text loses the reading place: Tab/Shift+Tab away
and back lands at the very top — "a more important
problem". (3) MacRumors full text carries a "Popular
Stories" block mid-article and a footer tail (Push
Notifications pitch, Videos/Guides/Roundups, Other
Stories, author/date runs). (4) Windows Latest articles
carry figcaption text with "Credit: Windows Latest"
lines and end with "The post ... appeared first on
Windows Latest". (5) Ctrl+F is broken two ways: Enter
after typing leaves the cursor on the first line (an
extra F3 is needed to reach the first match), and
Shift+F3 at the first match announces nothing instead
of "No more occurrences". (6) The tree opens on a
visible "Feeds" parent that itself means "all articles";
Python's root is hidden with All Articles / Favorites /
categories side by side at the top level.

**Extraction — reproduced through the real pipeline**
(probe harness over the three live pages, fetched
2026-11-08): Ars site path carried the figcaption twice
(nested post-content divs) — the rich path once, plus a
second hidden copy in the PhotoSwipe block
(`pswp-caption-content`, not a figcaption element);
MacRumors had no site branch, so the whole-page second
opinion won and carried the Popular Stories rail
(`data-track="popular-stories"`, outside `<article>`,
glued into one paragraph: "Popular StoriesRIP, MacBook
Ultra...") and story entries after the body; Windows
Latest fell to the whole-page base engine with its
figcaptions inline. The Ars rail tail did not reproduce
in either path (his copy likely came via a route the
probe does not exercise); no dedicated tail cut was
needed once captions died at the source.

**Fixes (extraction):** `drop_figcaptions` +
`drop_pswp_captions` (extract.rs) run on the raw page in
`extract_article_opts` before any engine reads it, and
again inside `clean_article_html`; `publisher_body_text`
and `publisher_body_fragment` also drop figcaption
elements directly, so no path can resurrect them. The
image itself stays (alt markers are a separate opt-in).
Text-layer strips in postprocess.rs, wired into the
dispatcher: `strip_macrumors_boilerplate` cuts at a
paragraph starting "Popular Stories" (heading alone or
glued), or at the footer anchors "Push Notifications" /
"To Subscribe or Unsubscribe" / "Other Stories";
`strip_windowslatest_boilerplate` cuts at the "The
post ... appeared first on" syndication paragraph. Both
return the original text if the cut would empty the
article. Probe after the fix, all three live pages:
caption/credit/rail/footer counts 0 in text AND rich
paths, bodies intact. Regression cases in
batch25_slice.rs cover the figcaption + pswp twin, the
MacRumors rail (incl. a mid-sentence mention surviving)
and the Windows Latest caption + footer.

**Reading place (fix):** the main window's reader pane
now captures (text, caret, selection) on focus-out and
restores on focus-in only when the text is unchanged,
consuming the capture so a later focus-in cannot
resurrect a stale position over a fresh article
(dialogs::keep_text_place does the same, position-only,
for the article window, Article History text and Feed
Errors detail — their text never changes under them).
wx resets the native caret on focus gain; the restore
runs in the SET_FOCUS handler, after that reset.
Guarded by the source-scan test
article_text_keeps_its_place_across_focus_changes.
Brandon's NVDA pass is the acceptance check, as with
every focus fix in this port.

**Find (fix):** act_find read the search start AFTER
prompt_focus closed the modal; the prompt round-trip
(tree/list focus restore) had already reset the
reader's caret to 0, so Enter searched from the top
and its match selection did not survive focus landing
on the list — the extra F3 "worked" because F3 starts
from the selection end with focus already in the
reader. The start is now captured before the prompt
opens; a landed match takes focus via run_find's
reader.set_focus(). Bug (5b) was a wording divergence,
not a dead announce: the backward terminal miss said
"No previous occurrences" while Python
(_content_find_no_more) and Brandon's expectation both
use "No more occurrences" for both directions — the
backward action now uses that sentence. Guarded by
find_searches_from_the_pre_prompt_place_and_announces_both_ends.

**Tree (fix):** rebuild_tree's root is now the hidden
wx root (TreeCtrlStyle::HideRoot at creation, Python's
AddRoot("Root") shape): the visible top level is a new
"All Articles" node (all-articles filter data "", the
same value the root keeps for programmatic fallback),
then Favorites, YouTube Recommendations when signed in,
Smart Folders, the categories, and Uncategorized last,
as before. Selection restore special-cases the empty
filter to land on the visible All Articles node rather
than the hidden root. Python also has a "Deleted
Articles" top-level node; the port's Deleted Articles
remains a dialog (recorded deviation since Audit 8 —
restored_view_state maps deleted:all to All), unchanged
here. Guarded by
tree_is_flat_with_a_top_level_all_articles_node.
This also resolves the stale PARITY note (v0.1.64
section) that the port lacked an "All Articles" root
node.

VERIFIED 2026-11-08: local gate green (fmt,
clippy --workspace --all-targets -D warnings, 1004
tests passed, build). Main CI run 42508629348 success
(all three jobs). Release run 42511547508 success
(three Package jobs + Publish). v0.1.75 is Latest,
draft=false, exactly the 7 contract assets, and the
release body is identical to the CHANGELOG.md 0.1.75
section. BlindRSS.tar.gz signature verified VALID
with the tamper control rejected (verify-0175).
Brandon's acceptance checks: the named Ars article
reads with no caption/"Credit:" lines and no rail
tail; MacRumors articles have no Popular Stories
block and no push/footer tail; Windows Latest
articles have no image captions/credits and no
"appeared first on" ending; reading position in
article text survives a Tab/Shift+Tab round-trip;
Ctrl+F + Enter lands on the first match from the
current place; Shift+F3 pressed again at the first
match announces "No more occurrences"; the tree opens
flat with All Articles / Favorites / categories as
top-level siblings.

## Settings values silently not saving (Brandon's report, 2026-11-08; ships in v0.1.74)

**Report (Brandon, 2026-11-08, on v0.1.73):** "settings
mostly don't save. For instance, if I set the Refresh
Interval (seconds, 0 = off) to 0, and clear the 300
option, upon closing and returning to the Settings
window I see 300 there."

**Reproduced under Xvfb** driving the real GUI
(xdotool; screenshots + notes in the chains repro0174
folder): the mouse path works — type 0, click Save,
config gains refresh_interval=0, reopen shows 0. Two
defects proven: (1) clearing the interval field to
EMPTY and clicking Save closed the dialog with the
stored value unchanged — no error, no write; (2)
pressing Enter inside a field did nothing at all
(no default button), where Python's SettingsDialog
activates OK.

**Root cause:** every numeric setting in show_settings
was a free-text TextCtrl saved through an
`if let Ok(v) = text.parse()` guard (the interval
directly, the rest through the `num()` helper), so
empty or unparseable text silently skipped the write
and the previously stored value survived. A keyboard
user clearing a field (select-all + Delete) to retype
or to zero it hit this exactly. Python cannot produce
the state: its interval is a preset wx.Choice
(refresh_map, "Never" = 0 … "4 hours") and its
numerics are wx.SpinCtrl, which always hold a valid
clamped number.

**Fix (Python parity):** the Refresh Interval is a
preset Choice again — the table and Python's
closest-preset selection rule live in core
feed_settings (GLOBAL_REFRESH_INTERVALS,
global_refresh_interval_index,
global_refresh_interval_seconds) with unit tests —
and every other numeric Settings field is now a
spin control (add_spin/add_spin_f64 on
SpinCtrlDouble, which unlike wxdragon's SpinCtrl has
a get_value) with the same ranges the old save path
clamped to, read and written unconditionally; the
parse-guard `num()` helper is deleted. The Save
button is the dialog's default button
(Button::set_default), so Enter in any field saves
and closes, as Python's OK does. A source-scan test
(settings_numeric_controls_always_hold_a_value) bans
parse::<i64>/parse::<f64> and the num-closure pattern
from settings.rs and pins the preset save + default
button.

**Fix verified by driving the rebuilt GUI** (same
rig): the Choice shows "5 minutes" for a stored 300;
selecting "Never" + Save writes refresh_interval=0;
spin edits (Max Concurrent 7/9/11 across runs) write
unconditionally; Enter inside a spin field saves and
closes. One rig artifact chased down: with no window
manager, the Return event leaked to the main window
after the modal closed and re-opened Settings; under
openbox the same Enter saved and closed with no
reopen — WM-less focus plumbing, not app behavior.

**v0.1.74 VERIFIED** (2026-11-08): tag on `0ebfb41`
(the fix commit `77ab723` plus a clippy -D warnings
fix — CI's clippy runs with -D warnings and caught
add_spin_f64's 8 arguments; the local gate now runs
clippy the same way). Main CI 42494384383 success
(all three jobs); Release run 42497232441 success
(3 Package jobs + Publish). API read: Latest =
v0.1.74, draft=false, exactly 7 assets, release body
identical to this CHANGELOG's `## 0.1.74` section.
BlindRSS.tar.gz signature VALID against the
compiled-in key, tamper control rejected. Local gate
on the ship tree: 998 tests passed. Brandon's
acceptance: in Settings, set the Refresh Interval to
Never, Save, reopen — Never stays; numeric fields
hold what was typed; Enter in a field saves.

## Settings close crash (Brandon's report, 2026-11-07; ships in v0.1.73)

Report: open Settings, do nothing, press Escape — the app
crashes. Root cause proven from the code and the wxdragon
sources: the v0.1.71 async soundcard fill runs on a
`Box::leak`ed wxTimer started at 100 ms. wxdragon's
`wxd_Timer_Create` makes a plain wxTimer whose owner is
only the event sink, so destroying the dialog does not
stop the timer, and the tick only disarmed itself when
the libVLC enumeration landed. Any close (Escape, Cancel,
OK) that beat the enumeration left the timer firing into
destroyed controls — `soundcard_choice.clear()/append()`
on a dead wxChoice. The codebase's frame-owned leaked
timers all stop in `on_close` (player, article window,
cast queue); the seven dialog-owned ones had no stop
anywhere. Fix: stop-before-destroy at every dialog site —
show_settings (soundcard + OpenRouter timers, both
hoisted to function scope so the end of the function can
stop them after show_modal), the Inoreader sign-in wait
dialog, and dialogs.rs' show_video_search,
show_feed_errors, show_youtube_signin and
show_import_cookies. A source-scan regression test
(`dialog_owned_timers_stop_before_the_dialog_is_destroyed`)
holds the rule, including the stop-after-show_modal-
before-destroy order in show_settings.

**v0.1.73 VERIFIED** (2026-11-07): tag on `ad76b36`;
main CI 42443173646 success (all three jobs); Release
run 42445447665 success (3 Package jobs + Publish). API
read: Latest = v0.1.73, draft=false, exactly 7 assets,
release body identical to this CHANGELOG's `## 0.1.73`
section. BlindRSS.tar.gz signature VALID against the
compiled-in key, tamper control rejected. Local gate on
the ship tree: 996 tests passed (995 + the new scan),
clippy zero. Brandon's acceptance: open Settings and
close it immediately (Escape) before the soundcard list
fills in — no crash.

## Parity closeout batch 1 (ships in v0.1.82)

Four genuine ports plus a table reconciliation. The §1/§2
table had drifted: several rows still said "missing" or
"remaining" for code that landed in earlier batches
without its row update. Those rows now say what the code
does; the ports below are new in this batch.

**Onefile-aware worker kills (Python #150).** A frozen
(onefile) yt-dlp runs workers as same-exe child processes,
and killing the bootloader orphans them along with their
`%TEMP%\_MEI*` trees. Ported: `is_onefile_tool` /
`onefile_child_pids` / `end_tool_tree` (end the worker
tree first, 3 s grace plus 1 s on a late worker, then the
bootloader kill), wired into `dependency.rs`,
`source_feeds.rs` and `feedsearch.rs`. Regression:
`onefile_worker_is_a_same_exe_child`,
`only_ytdlp_takes_the_onefile_kill_path`.

**Miniflux companion browser fallback.** Python recovers
blocked Miniflux feed fetches through a same-origin
companion POST. Ported: `browser_fallback_endpoint`
(same-origin URL builder + timeout clamp),
`browser_feed_fallback_request`, and
`queue_browser_feed_recovery`, with recovery queued on
refresh, feed add and single-feed refresh; a 404 disables
further probing. Six `miniflux_fallback_tests` hold the
endpoint shape, the clamp, the 404 rule and the queue
points. Table row §1/Miniflux updated.

**accessible-output2 speech layer.** Ported without the
library: Windows speaks through SAPI (`sapi::speak`, tried
after NVDA then JAWS — Python's order) and Linux through
`spd-say --wait/--cancel` with accessible_output2's
roles; macOS keeps `say`. Regression:
`fallback_speech_tests`, plus the live SAPI dry-run
(`sapi_voice_resolves_without_speaking --ignored`,
passed) and the silent spd-say probe test. Table row
§2/Announcements updated: no remaining items.

**YouTube anonymous browser-session bootstrap.**
Python's last yt-dlp recovery phase mints a visitor
identity in a hidden browser visit and retries once with
the warmed cookie jar, visitor data and the browser UA.
Ported: `core/youtube_session.rs` (warm-browser identity
through `browser::youtube_identity`, sharing the fetch
lock/session; 30-minute jar reuse beside the config;
Netscape jar in Python's shape; cookie contents, values,
tokens and visitor data never logged) plus
`Phase::{browser_bootstrap, visitor_data,
browser_user_agent}` and the bootstrap arm in
`run_recovery`, wired into all three recovery callers
(media streaming, play cache, downloads) with the empty-
target/empty-response fail-closed rules. The phase runs
last, exactly Python's order; an unavailable session
skips it silently. Regression: five `youtube_session`
unit tests, the `recovery_phases` bootstrap-last shape
assertions, and
`bootstrap_phase_skips_silently_without_session`. Table
row §2/YouTube updated; the Task 22 deviation and the
§10 non-port verdict are narrowed to the pytubefix client
phase, which stays unported (no Rust analogue).

**Reconciliation (no new code).** Miniflux remote
category CRUD (§1) closed back in v0.1.52
(`mirror_hosted_category`); GitHub page completion (§2)
ported whole (threads/github.rs mirrors the Python
module's pull/issue/commit/release/discussion documents —
the module has no feed-enumeration half); the Rumble /
Odysee / NPR / Sky source family (§2) enumerated at
refresh (sources.rs + source_feeds.rs); directory +
social + yt-dlp search (§2) ported (feedsearch.rs
providers); forum sources (§2) are partial (Groups/
Groups.io subscriptions ported, Reddit/Lemmy shaping via
discovery transforms, dedicated enumeration still open).
Each row now carries its evidence.
