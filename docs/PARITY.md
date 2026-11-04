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
| Miniflux (`providers/miniflux.py`) | 2,945 lines, 33 public | + test_connection, podcast archive, article delete, fetch_full_content, browser fallbacks for some endpoints | **partial** — sync + remote state + enclosures done; v0.1.19 adds test_connection (Settings button), OPML export/import via API, fetch-content (v0.1.18 arbitration), per-feed refresh, create/delete feed, bookmark toggle. Remaining: remote category CRUD via /v1/categories (the client resolves category titles on add_feed; no rename/delete surface), browser fallbacks |
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
| Feed discovery + site search | discovery.py 5,530 | **partial** — social transforms (Reddit/Bluesky/Mastodon/Lemmy/PieFed/Kbin/YouTube channel+playlist) + HTML alternate discovery + Add Feed resolution chain done; directory search (Feedsearch/Podcast Index/Apple/Fyyd), yt-dlp site search, synthetic forum subscriptions missing |
| Article extraction (layered cascade) | article_extractor.py 5,098 | **partial** — cascade + full gate-marker list, paywall/link-list rejection, JSON-LD, Register + Axios dedicated extractors, metered notice, zero-width strip (slices 1, 6); engine-disagreement vetting hardened v0.1.4 (error-page title/body markers + prose shape, with BBC article + BBC 404 fixture regression tests — a blanket disagreement rejection discarded good extractions); precision-first base + lean lead recovery v0.1.11 (Python's favor_precision recipe: balanced mode dragged footer/affiliate boilerplate into Brandon's 9to5Mac full text; precision reproduced Python's output exactly; regression fixtures 9to5mac_whatsapp.html + cbc_syndicated.html); remaining: Python's meta-description lead recovery depth. Landed v0.1.18: fetch fallback chain (stored cookies -> Sky News via Google Translate -> Jina-as-text on gates -> Smry -> Wayback raw snapshots; curl_cffi TLS impersonation and the warm-browser stage are documented non-ports — no Rust equivalent exists for either), byte-accurate decoding (override -> BOM -> header charset -> meta charset -> UTF-8, encoding_rs), BBC/WSJ/TechRadar boilerplate strips, Bloomberg video JSON-LD text, GSMArena battery-table sort (HTML-level; not ported — the table extracts unsorted), thread rebuilds for the API platforms (Lemmy api/v3, GitHub REST, Reddit .json, Discourse .json — core/threads.rs; the HTML-selector forum configs (Invision/vBulletin/XenForo/Google Groups) are not ported: no CSS-selector engine), captcha solver client (core/captcha.rs; its only Python caller is the browser stage) |
| HTML rendering of articles | article_html.py 1,190 | **missing** (native TextCtrl reader instead; rich view is a Phase-6 manual gate) |
| Utilities (dates, text, URLs) | utils.py 2,893 | **partial** — pulled in as needed |
| Browser cookies (Chromium) + site cookies + clearance | chromium_cookies.py 2,184; site_cookies.py 1,533 | **done (v0.1.21)** — Netscape jar + matching + 8KB budget + fetch wiring; Chromium import: profile discovery for the full Chromium family on all 3 OSes, v10 keys (Windows DPAPI + AES-256-GCM; macOS Keychain PBKDF2 + AES-128-CBC; Linux peanuts default), DB copy/read/decrypt with domain-hash strip + merge into the jar (core/chromium.rs) + Tools > Import Site Cookies dialog. NOT ported: the v20 app-bound scheme (Python resolves it via SYSTEM impersonation + CNG + a UAC-elevated helper; the Rust app ships no elevation machinery) — v20 rows are counted as skipped; the exported-jar auto-import watcher landed v0.1.22 (core/cookies_import.rs: YouTube export auto-import with mtime persistence, Downloads exports merged into the site jar, changed Chromium profiles re-read hands-free; a 45-second watcher timer in the app mirrors CookieImportWatcher) |
| Range cache proxy / stream proxy / play cache | range_cache_proxy.py 2,000; stream_proxy.py 818; play_cache.py | **partial** — the cast LAN proxy (Range passthrough + ffmpeg HLS) and the YouTube play cache landed v0.1.20 (core/proxy.rs, core/play_cache.rs); Python's general range-cache proxy for ordinary playback is not ported (MediaCtrl streams directly) |
| Dependency check (yt-dlp, ffmpeg, VLC…) | dependency_check.py 1,868 | **done (v0.1.20)** — core/dependency.rs: tool detection, managed yt-dlp download/update, startup prompt gated by prompt_missing_dependencies_on_startup, Settings surface |
| Casting adapter | casting.py 1,609 (+ Caster engine, see Phase 5 report) | **partial** — caster-core in core: SSDP discovery, hand-rolled CASTv2 wire + TLS session, receiver launch, media load/control, Cast button; DLNA/Sonos/Roku/Kodi landed v0.1.20 (core/castupnp.rs, cast.rs discovery incl. mDNS); AirPlay and the cast queue UI remain |
| Updater + translation updates | updater.py 1,283; translation_updates.py | **partial** — check + button + verified download/stage/apply landed v0.1.16 (SHA256SUMS asset ships with each release); translation updates landed v0.1.22 (core/translation_updates.rs: ETag-conditional catalog fetch from the repo's main branch into the data-dir override tree the loader prefers, version-transition reset, MIN_SANE_MESSAGES sanity gate, Settings section with frequency + Check Now, background due-checks on the watcher tick) |
| Translation (LLM clients) | translation.py 1,020 | **done (v0.1.20)** — core/translate.rs: six providers, Python's system prompt, chunking + caps, language detection; Translate action + Settings section |
| Browser feed detection | browser_feed.py 963 | **missing** |
| GitHub-as-source feeds | github_source.py 946 | **missing** |
| Podcast archive | podcast_archive.py 716 | **partial** — rescan (live + Wayback union) done; dialog/lineage depth missing |
| YouTube full text (description/chapters/transcript) | youtube_fulltext.py 755; youtube_pytubefix.py | **done** — description + chapters + VTT transcript via yt-dlp (slice 12) |
| YouTube account + browser session | youtube_account.py; youtube_browser_session.py | **missing** |
| YouTube Takeout import | youtube_takeout.py | **done** — ZIP subscriptions/playlists/history import (slice 12) |
| Forum sources (Reddit/Lemmy/Groups.io) | forum_sources.py 551; groups_io.py 448 | **missing** |
| Rumble / Odysee / NPR / Sky sources | rumble.py 582; odysee.py; npr.py; sky.py | **missing** |
| SponsorBlock | sponsorblock.py | **done** (v0.1.14: core/sponsorblock.rs — YouTube id extraction, skipSegments fetch + category filter; the player applies segments on its tick with the already-skipped guard and sponsor_skip announcements) |
| Audio silence detection + skip | audio_silence.py 534 | **partial** (v0.1.14: core/silence.rs — ffmpeg silencedetect with Python's RMS-fallback parameters, merge-gap + padding post-processing; the player analyses on a worker and skips with announcements. Python's primary WebRTC-VAD detector is not ported — no Rust VAD binding exists; the threshold detector is the documented substitute) |
| Playback state persistence | playback_state.py | **done** (slice 7: get/upsert/delete against the Python table) |
| Equalizer (libVLC 10-band presets) | equalizer.py | **missing** |
| User agents / fetch identity | user_agents.py 540; http_headers.py | **partial** — all fetches ride the shared http agent with the resolved UA (8 presets + custom + auto=platform preset) and yt-dlp gets the same UA, v0.1.16; installed-browser version detection + per-feed identity + http_headers.py per-channel headers missing |
| Backup/restore | backup.py | **done** — ZIP backup (VACUUM INTO + config) + restore + app buttons; restore is staged to restore_pending/ and applied at startup before the DB opens, replaced files kept as .pre-restore (Python semantics; the earlier live swap was a correctness bug, fixed v0.1.13) |
| Retention/purge | retention.py | **done** (v0.1.16: named policies ported in core/retention.rs incl. the hidden 2_days/3_months ids; article prune honours article_retention, downloads pruned by download_retention at startup) |
| Smart folders / filters logic | smart_folders.py; filters.py | **partial** — rule tree evaluator + action pipeline ported and tested (tag field awaits articles.tags) |
| Metadata enrichment | metadata_enrich.py | **done** (v0.1.18: core/enrich.rs — meta tags + JSON-LD author/keywords/section; Db::apply_enrichment fills a missing author and merges tags into articles.tags with Python's dedupe/caps; the full-text drain enriches from the fetched page; smart-folder Tag rules read the stored tags) |
| Media URL resolution | media_url.py; download_formats.py | **partial** — yt-dlp info/stream resolution + download presets done; playback resolution chain ported v0.1.10 (enclosures keep their MIME type, audio/video enclosure preferred over images, yt-dlp gated to Python's supported-site allowlist, page media detection scans stored content then the fetched page, detected media persisted via update_article_media, yt-dlp generic only as last resort — fixes Brandon's Double Tap "Unsupported URL" failure); media_url.py normalize rules landed v0.1.22 (core/media_url.rs, wired into Open Media URL + title_from_url naming); download_formats.py presets **done** v0.1.16 (core/download.rs: 8 named formats, Python's exact selectors + mp3 bitrates) |
| Article language detect | article_lang.py | **done (v0.1.22)** — core/article_lang.rs: BCP-47 normalize_lang, lang_from_page_html, resolve_content_language priority chain; wired into the Translate action (target normalization + already-in-language check). Content lang marking of the reader itself awaits the rich reader (Phase-6 gate) |
| Announcements | announcements.py | **partial** — event table + mode resolution ported (v0.1.12, core/announce.rs, Python's normalize_modes semantics); NVDA controller-client speech + Braille backend on Windows (vendored nvdaControllerClient.dll ships in the Windows zip), macOS `say` speech; Settings > Announcements page with per-event mode choices + Test button; wired events: start_update, refresh-complete (general), favorite_toggle, status_toggle, filter_change, playback_speed, media_navigation, sponsor_skip. JAWS COM backend landed v0.1.21 (FreedomSci.JawsApi SayString via hand-rolled IDispatch, gated on a live jfw/jaws/fusion process, tried after NVDA exactly as Python; Braille stays NVDA-only as in Python). Remaining: accessible-output2 layer (filter_change wired v0.1.13; playback_speed / media_navigation / sponsor_skip wired v0.1.14) |
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
documented ffmpeg fallback position; dead Python keys
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
