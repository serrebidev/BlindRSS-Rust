# BlindRSS Parity Audit — every provider, every feature

Current source comparison and ports: [2026-11-01 audit](PARITY-2026-11-01.md).
The historical status rows below have not all been reconciled with later work.

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
