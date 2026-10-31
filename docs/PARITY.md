# BlindRSS Parity Audit — every provider, every feature

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
| Miniflux (`providers/miniflux.py`) | 2,945 lines, 33 public | + test_connection, podcast archive, article delete, fetch_full_content, browser fallbacks for some endpoints | **partial** — sync + remote state + enclosures done; v0.1.19 adds test_connection (Settings button), OPML export/import via API, fetch-content (v0.1.18 arbitration), per-feed refresh, create/delete feed, bookmark toggle. Remaining: remote categories management, article delete (Miniflux has no entry-delete API; Python's is local too), browser fallbacks |
| Inoreader (`providers/inoreader.py`) | 1,110 lines, 24 public | GReader protocol + OAuth2 (`core/inoreader_oauth.py`), rate-limit handling, metadata/article caches | **partial** — OAuth-token sync (slice 5); v0.1.18/19: paste-URL OAuth + local HTTPS-listener OAuth (rcgen self-signed on 127.0.0.1:18423), 429 Retry-After backoff on the GReader API surface, metadata/article caches with Python's TTL keys + clamps (manual refresh invalidates), enclosures. Remaining: page-size key wiring |
| BazQux (`providers/bazqux.py`) | 765 lines, 24 public | GReader protocol | **partial** — ClientLogin sync (slice 5); v0.1.17/19: remote subscribe/unsubscribe, per-feed refresh, category set, batch mark-read, starred favorites, enclosures via the shared GReader client |
| The Old Reader (`providers/theoldreader.py`) | 744 lines, 23 public | GReader protocol; defines the family's stream-id helpers | **partial** — same as BazQux |
| YouTube account (`providers/youtube_account.py`) | 231 lines, 22 public | Wraps another provider: subscriptions/recommendations from a signed-in YouTube account (device-code sign-in) | **partial** — v0.1.19: device-code sign-in dialog (core/youtube_account.rs; the TV client's public client id is built in, its secret is read from youtube_account_client_secret config rather than embedded), subscription auto-add sync under Sync Providers (category + known-channels tracking). Remaining: the Recommendations virtual feed |
| Hosted podcast archive (`providers/hosted_podcast_archive.py`) | 313 lines, mixin (4 public) | Archive state + refresh for hosted providers | **partial** — hosted articles now carry their enclosures as media (v0.1.19, the mixin's data half: GReader + Miniflux enclosure mapping in sync); the archive-state lineage tracking for hosted feeds is still missing |

Base-interface method groups (54) and where they stand in Rust core:

- Lifecycle: get_name, refresh, cancel_refresh, refresh_feed,
  refresh_feeds_by_ids, scheduled_refresh_tick,
  should_force_startup_refresh — **done** locally (v0.1.13:
  Refresh Category covers by-ids, the refresh worker honours a
  cancel flag between feeds, a 60s tick fires refreshes on
  refresh_interval, refresh_on_startup fires shortly after launch)
- Feeds: get_feeds, add_feed, remove_feed, supports/update feed edit
  and URL update, supports/reset feed title — **partial**
  (add/remove done; edit dialog, URL update, title reset missing)
- Articles: get_articles, get_articles_page, get_article_by_id,
  get_feed_errors — **partial** (list done; paging-by-provider,
  by-id, feed errors missing)
- Read state: mark_read, mark_unread, mark_read_batch, mark_all_read —
  **partial** (local read/all done; unread, batch, remote propagation
  for GReader/Miniflux partial: mark_read_remote exists, not wired)
- Favorites: supports_favorites, toggle_favorite, set_favorite — **done** locally
- OPML: import_opml, export_opml — **done** (local)
- Categories: get_categories, add/rename/delete, move_category,
  supports_subcategories, get_category_hierarchy,
  uncategorized_is_real_category — **done** locally (v0.1.13:
  categories carry parent_id + full-path titles,
  ensure_category_path/delete_category_path with Python's
  reparenting, rename moves the subtree, the tree groups feeds
  under nested category nodes)
- Full text: fetch_full_content — **partial** (cascade wired into
  the app via Read Full Text, slice 6; fetch fallback chain missing)
- Media/chapters: get_article_chapters, HostedArticleMediaMixin
  (update_article_media) — **partial** (chapters stored/listed and
  navigated in the player since slice 7, update_article_media
  landed with the v0.1.10 playback-resolution pass; the hosted
  mixin's remote media table is batch 8)
- Delete/restore: supports_article_delete, delete_article,
  supports_restore_deleted, restore_article — **done** (local)
- Smart folders: supports_smart_folders, get/create/update/delete —
  **partial** (CRUD + rule evaluation + tree display done; update/rename + full rule editor dialog missing)
- Filter rules (LocalProvider extension): get/create/update/delete,
  reorder, apply to existing — **partial** (CRUD + pipeline applied on store done; reorder, apply-to-existing, editor dialog missing)

## 2. Core feature areas (Python `core/`, 46k lines)

| Area | Python module(s), size | Rust status |
|---|---|---|
| Database (schema, counts, chapters, playback state, smart folders, filter rules) | db.py 2,656 | **partial** — feeds/articles/categories/deleted/chapters/playback_state/smart_folders/filter_rules all read+written; podcast archive state tables + article counts unread |
| Config (defaults incl. providers, shortcuts, columns) | config.py 886 | **partial** — reader + writer (dotted set_path, pretty save) + settings dialog done; Python's default surface is 189 keys (Audit 2, §5B) and Rust touches 13 — the settings-depth gap lives there. Wired so far beyond the 13: remember_last_feed/start_maximized (v0.1.9), announcements map (v0.1.12), refresh_interval/refresh_on_startup/category_tree_default_expanded (v0.1.13) |
| Feed discovery + site search | discovery.py 5,530 | **partial** — social transforms (Reddit/Bluesky/Mastodon/Lemmy/PieFed/Kbin/YouTube channel+playlist) + HTML alternate discovery + Add Feed resolution chain done; directory search (Feedsearch/Podcast Index/Apple/Fyyd), yt-dlp site search, synthetic forum subscriptions missing |
| Article extraction (layered cascade) | article_extractor.py 5,098 | **partial** — cascade + full gate-marker list, paywall/link-list rejection, JSON-LD, Register + Axios dedicated extractors, metered notice, zero-width strip (slices 1, 6); engine-disagreement vetting hardened v0.1.4 (error-page title/body markers + prose shape, with BBC article + BBC 404 fixture regression tests — a blanket disagreement rejection discarded good extractions); precision-first base + lean lead recovery v0.1.11 (Python's favor_precision recipe: balanced mode dragged footer/affiliate boilerplate into Brandon's 9to5Mac full text; precision reproduced Python's output exactly; regression fixtures 9to5mac_whatsapp.html + cbc_syndicated.html); remaining: Python's meta-description lead recovery depth. Landed v0.1.18: fetch fallback chain (stored cookies -> Sky News via Google Translate -> Jina-as-text on gates -> Smry -> Wayback raw snapshots; curl_cffi TLS impersonation and the warm-browser stage are documented non-ports — no Rust equivalent exists for either), byte-accurate decoding (override -> BOM -> header charset -> meta charset -> UTF-8, encoding_rs), BBC/WSJ/TechRadar boilerplate strips, Bloomberg video JSON-LD text, GSMArena battery-table sort (HTML-level; not ported — the table extracts unsorted), thread rebuilds for the API platforms (Lemmy api/v3, GitHub REST, Reddit .json, Discourse .json — core/threads.rs; the HTML-selector forum configs (Invision/vBulletin/XenForo/Google Groups) are not ported: no CSS-selector engine), captcha solver client (core/captcha.rs; its only Python caller is the browser stage) |
| HTML rendering of articles | article_html.py 1,190 | **missing** (native TextCtrl reader instead; rich view is a Phase-6 manual gate) |
| Utilities (dates, text, URLs) | utils.py 2,893 | **partial** — pulled in as needed |
| Browser cookies (Chromium) + site cookies + clearance | chromium_cookies.py 2,184; site_cookies.py 1,533 | **partial** — Netscape jar + matching + 8KB budget + fetch wiring done; Chromium import/clearance harvest missing |
| Range cache proxy / stream proxy / play cache | range_cache_proxy.py 2,000; stream_proxy.py 818; play_cache.py | **missing** |
| Dependency check (yt-dlp, ffmpeg, VLC…) | dependency_check.py 1,868 | **missing** |
| Casting adapter | casting.py 1,609 (+ Caster engine, see Phase 5 report) | **partial** — caster-core in core: SSDP discovery, hand-rolled CASTv2 wire + TLS session, receiver launch, media load/control, Cast button; DLNA/AirPlay/queue missing |
| Updater + translation updates | updater.py 1,283; translation_updates.py | **partial** — check + button + verified download/stage/apply landed v0.1.16 (SHA256SUMS asset ships with each release); translation updates still missing (batch 9) |
| Translation (LLM clients) | translation.py 1,020 | **missing** |
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
| Media URL resolution | media_url.py; download_formats.py | **partial** — yt-dlp info/stream resolution + download presets done; playback resolution chain ported v0.1.10 (enclosures keep their MIME type, audio/video enclosure preferred over images, yt-dlp gated to Python's supported-site allowlist, page media detection scans stored content then the fetched page, detected media persisted via update_article_media, yt-dlp generic only as last resort — fixes Brandon's Double Tap "Unsupported URL" failure); media_url.py normalize rules still missing; download_formats.py presets **done** v0.1.16 (core/download.rs: 8 named formats, Python's exact selectors + mp3 bitrates) |
| Article language detect | article_lang.py | **missing** |
| Announcements | announcements.py | **partial** — event table + mode resolution ported (v0.1.12, core/announce.rs, Python's normalize_modes semantics); NVDA controller-client speech + Braille backend on Windows (vendored nvdaControllerClient.dll ships in the Windows zip), macOS `say` speech; Settings > Announcements page with per-event mode choices + Test button; wired events: start_update, refresh-complete (general), favorite_toggle, status_toggle, filter_change, playback_speed, media_navigation, sponsor_skip. Remaining: JAWS COM backend (Python falls back to it after NVDA), accessible-output2 layer (filter_change wired v0.1.13; playback_speed / media_navigation / sponsor_skip wired v0.1.14) |
| Single instance, runtime env, self-test | single_instance.py; runtime_env.py; runtime_self_test.py | **missing** |
| Platform integration (Windows/macOS) | windows_integration.py; macos_integration.py | **missing** |
| Screen-reader announcements | screen_reader_announce.py | **partial** — status-bar text; no dedicated announce channel yet |
| i18n (17 locales) + po compile | i18n.py; po_compile.py | **partial** — PO parser + catalog load + language resolution done, main-window labels wired (Refresh); full string sweep + .mo compile step missing |
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
Remaining from item 8: F1 context help (help_topics.py),
Chromium cookie import, global hotkeys, JAWS announcements,
full i18n sweep — batch 9c.

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
