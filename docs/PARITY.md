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
| Local (`providers/local.py`) | 6,063 lines, 55 public methods | Full interface + smart folders, filter rules, feed errors, retention, podcast archive | **partial** — refresh/store, feed CRUD, OPML, categories, search, favorites, delete/restore, smart folders + filter rules (evaluate on store), retention prune done; feed errors view, scheduled/startup refresh, refresh cancel missing |
| Miniflux (`providers/miniflux.py`) | 2,945 lines, 33 public | + test_connection, podcast archive, article delete, fetch_full_content, browser fallbacks for some endpoints | **partial** — feeds/entries sync + remote mark-read done (slice 5); connection test, categories sync, OPML import, full-content fetch, delete missing |
| Inoreader (`providers/inoreader.py`) | 1,110 lines, 24 public | GReader protocol + OAuth2 (`core/inoreader_oauth.py`), rate-limit handling, metadata/article caches | **partial** — OAuth-token GReader sync done (slice 5); OAuth flow, rate-limit backoff, caches missing |
| BazQux (`providers/bazqux.py`) | 765 lines, 24 public | GReader protocol | **partial** — ClientLogin sync done via shared GReader client (slice 5); per-feed refresh, remote feed add/remove/category ops missing |
| The Old Reader (`providers/theoldreader.py`) | 744 lines, 23 public | GReader protocol; defines the family's stream-id helpers | **partial** — same as BazQux |
| YouTube account (`providers/youtube_account.py`) | 231 lines, 22 public | Wraps another provider: subscriptions/recommendations from a signed-in YouTube account (device-code sign-in) | **missing** |
| Hosted podcast archive (`providers/hosted_podcast_archive.py`) | 313 lines, mixin (4 public) | Archive state + refresh for hosted providers | **missing** |

Base-interface method groups (54) and where they stand in Rust core:

- Lifecycle: get_name, refresh, cancel_refresh, refresh_feed,
  refresh_feeds_by_ids, scheduled_refresh_tick,
  should_force_startup_refresh — **partial** (manual refresh only;
  cancel + scheduled + startup-force missing)
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
  uncategorized_is_real_category — **partial** (flat add/remove done;
  rename + move done (provider depth pass); hierarchy/subcategories missing)
- Full text: fetch_full_content — **partial** (cascade wired into
  the app via Read Full Text, slice 6; fetch fallback chain missing)
- Media/chapters: get_article_chapters, HostedArticleMediaMixin
  (update_article_media) — **missing**
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
| Config (defaults incl. providers, shortcuts, columns) | config.py 886 | **partial** — reader + writer (dotted set_path, pretty save) + settings dialog done; Python's default surface is 189 keys (Audit 2, §5B) and Rust touches 13 — the settings-depth gap lives there |
| Feed discovery + site search | discovery.py 5,530 | **partial** — social transforms (Reddit/Bluesky/Mastodon/Lemmy/PieFed/Kbin/YouTube channel+playlist) + HTML alternate discovery + Add Feed resolution chain done; directory search (Feedsearch/Podcast Index/Apple/Fyyd), yt-dlp site search, synthetic forum subscriptions missing |
| Article extraction (layered cascade) | article_extractor.py 5,098 | **partial** — cascade + full gate-marker list, paywall/link-list rejection, JSON-LD, Register + Axios dedicated extractors, metered notice, zero-width strip (slices 1, 6); engine-disagreement vetting hardened v0.1.4 (error-page title/body markers + prose shape, with BBC article + BBC 404 fixture regression tests — a blanket disagreement rejection discarded good extractions); remaining: BBC/Sky/Bloomberg/GSMArena fixes, thread rebuilds (Reddit/Lemmy/forums), fetch fallback chain, lead recovery |
| HTML rendering of articles | article_html.py 1,190 | **missing** (native TextCtrl reader instead; rich view is a Phase-6 manual gate) |
| Utilities (dates, text, URLs) | utils.py 2,893 | **partial** — pulled in as needed |
| Browser cookies (Chromium) + site cookies + clearance | chromium_cookies.py 2,184; site_cookies.py 1,533 | **partial** — Netscape jar + matching + 8KB budget + fetch wiring done; Chromium import/clearance harvest missing |
| Range cache proxy / stream proxy / play cache | range_cache_proxy.py 2,000; stream_proxy.py 818; play_cache.py | **missing** |
| Dependency check (yt-dlp, ffmpeg, VLC…) | dependency_check.py 1,868 | **missing** |
| Casting adapter | casting.py 1,609 (+ Caster engine, see Phase 5 report) | **partial** — caster-core in core: SSDP discovery, hand-rolled CASTv2 wire + TLS session, receiver launch, media load/control, Cast button; DLNA/AirPlay/queue missing |
| Updater + translation updates | updater.py 1,283; translation_updates.py | **partial** — GitHub latest-release check + app button done; download/apply + translation updates missing |
| Translation (LLM clients) | translation.py 1,020 | **missing** |
| Browser feed detection | browser_feed.py 963 | **missing** |
| GitHub-as-source feeds | github_source.py 946 | **missing** |
| Podcast archive | podcast_archive.py 716 | **partial** — rescan (live + Wayback union) done; dialog/lineage depth missing |
| YouTube full text (description/chapters/transcript) | youtube_fulltext.py 755; youtube_pytubefix.py | **done** — description + chapters + VTT transcript via yt-dlp (slice 12) |
| YouTube account + browser session | youtube_account.py; youtube_browser_session.py | **missing** |
| YouTube Takeout import | youtube_takeout.py | **done** — ZIP subscriptions/playlists/history import (slice 12) |
| Forum sources (Reddit/Lemmy/Groups.io) | forum_sources.py 551; groups_io.py 448 | **missing** |
| Rumble / Odysee / NPR / Sky sources | rumble.py 582; odysee.py; npr.py; sky.py | **missing** |
| SponsorBlock | sponsorblock.py | **missing** |
| Audio silence detection + skip | audio_silence.py 534 | **missing** |
| Playback state persistence | playback_state.py | **done** (slice 7: get/upsert/delete against the Python table) |
| Equalizer (libVLC 10-band presets) | equalizer.py | **missing** |
| User agents / fetch identity | user_agents.py 540; http_headers.py | **partial** — UA set on full-text fetch; presets + per-feed identity missing |
| Backup/restore | backup.py | **done** — ZIP backup (VACUUM INTO + config) + restore + app buttons |
| Retention/purge | retention.py | **partial** — prune read non-favorites + old tombstones (30d button) done; policy options missing |
| Smart folders / filters logic | smart_folders.py; filters.py | **partial** — rule tree evaluator + action pipeline ported and tested (tag field awaits articles.tags) |
| Metadata enrichment | metadata_enrich.py | **missing** |
| Media URL resolution | media_url.py; download_formats.py | **partial** — yt-dlp info/stream resolution + download presets done; media_url.py rules missing |
| Article language detect | article_lang.py | **missing** |
| Announcements | announcements.py | **missing** |
| Single instance, runtime env, self-test | single_instance.py; runtime_env.py; runtime_self_test.py | **missing** |
| Platform integration (Windows/macOS) | windows_integration.py; macos_integration.py | **missing** |
| Screen-reader announcements | screen_reader_announce.py | **partial** — status-bar text; no dedicated announce channel yet |
| i18n (17 locales) + po compile | i18n.py; po_compile.py | **partial** — PO parser + catalog load + language resolution done, main-window labels wired (Refresh); full string sweep + .mo compile step missing |
| Help system (80 top-level topics; guide in 17 locales) | help_topics.py; docs/help/*.md | **partial** — guide loader with locale fallback, topic split, plain-text viewer dialog done; context-sensitive help mapping (help_topics.py: topic per dialog/command) not wired; release zips do not bundle the guide/locale assets yet |
| Shortcuts registry (70 actions, remappable) | shortcuts.py 439; gui/shortcut_keys.py; gui/hotkeys.py | **partial** — full registry extracted from Python, normalisation/conflicts, overrides dialog; live key dispatch for wired actions missing |

## 3. GUI surface (Python `gui/`)

- MainFrame: 127 user-action handlers. Rust app today covers (as
  buttons; there is no menu bar yet — Audit 2 §5C): refresh all,
  add/remove/edit feed, rename category, set feed category,
  import/export OPML, sync providers, toggle read/favorite,
  delete + deleted restore, mark all read, search, read full text,
  play media + player window, download, Takeout import, archive
  rescan, cast, backup/restore, prune, settings dialog, shortcuts
  dialog, help viewer, check updates, smart folder new/delete.
  **Missing:** menu bar + context menus, feed properties, feed
  errors view, per-feed/category refresh, stop refresh, sort
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
reads/writes through the 54-method interface. Rust today is
local-DB-only with a manual Sync Providers button. Porting the
switchable-backend model is a design slice of its own; until then
every "partial" provider row above understates the gap.

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
missing in Rust include: per-feed Refresh Feed, Refresh
Category, Add Subcategory, Edit/Remove Category, Delete Category
and Feeds, Import OPML Here, Export Category to OPML, Copy Feed
URL, Open Article, Open in Default Browser, multi-select
Mark {count} as Read/Unread, Copy Links / Copy Media Link /
Copy Image Link, Download As..., Detect Audio, play-queue
Add/Remove/Open, View History, View Feed Description,
Notifications for This Feed, playback speed selector,
Show Chapters + Previous/Next Chapter (main window), in-player
Cast to Device..., Open Chapter Link, Cut/Copy/Paste/Select All.

### 5D. Dialogs: 31 Python classes vs Rust
No Rust equivalent yet: AboutDialog, AccessibleBrowserFrame,
AddShortcutsDialog, CategoryPropertiesDialog, FeedPropertiesDialog,
FeedSearchDialog (discovery UI), FilterRulesDialog +
FilterRuleEditorDialog, OpenArticleDialog (open-by-URL,
core/open_article.py), OpenMediaUrlDialog (core/media_url.py),
PersistentSearchDialog, PodcastArchiveDialog, QueueDialog,
SmartFolderDialog (Rust has a name prompt only),
TakeoutImportSelectionDialog (Rust imports the whole ZIP),
YtdlpGlobalSearchDialog (18 handlers), ExcludeNotificationFeedsDialog,
ShortcutCaptureDialog, ArticleWindow (separate reading window with
HTML View menu; Rust reads in the main window only).

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
  Rust restore currently swaps rss.db live under the open
  connection — **correctness fix owed**; auto-backup **missing**.
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

1. Focus choreography: startup focus in tree (FIXED v0.1.6),
   focus restore after dialogs, reader-surface focus, large-reader
   mode — **mostly missing**.
2. Startup state restore: remember_last_feed/last_selected_feed,
   tree expansion default, start maximized — **missing**.
3. AccessibleBrowserFrame + accessible view entries (flattened
   speech-first view model with special views and category
   hierarchy) — **missing entirely**.
4. Announcements: 9 events x none/speech/braille/both modes
   (config["announcements"]), NVDA speech + braille channel —
   **missing** (status bar only).
5. Keyboard model: frame-level char hook; Ctrl+1..6 filters,
   Backspace toggle-read, F5/Shift+F5 refresh start/stop,
   Ctrl+D favorite, find-in-article with spoken results —
   **missing** (registry exists, no dispatch).
6. Full-text arbitration: web vs feed vs provider server-side
   fetch (Miniflux fetch-content), completeness wins, cacheable
   flag, per-feed encoding override, enrichment on the same fetch
   — **partial** (local cascade only). HTML in reader FIXED
   v0.1.6 via core html_to_text.
7. Player behaviours: raise on play, announced speed control,
   accessible chapter presentation, announced SponsorBlock/
   silence skips — **partial**.
