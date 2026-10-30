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
| Local (`providers/local.py`) | 6,063 lines, 55 public methods | Full interface + smart folders, filter rules, feed errors, retention, podcast archive | **partial** — refresh/store, feed CRUD, OPML, categories, search, favorites, delete/restore done (slices 1–4); smart folders, filter rules, feed errors, retention, scheduled refresh missing |
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
| Database (schema, counts, chapters, playback state, smart folders, filter rules) | db.py 2,656 | **partial** — feeds/articles/categories/deleted done; chapters, playback_state, smart_folders, filter_rules tables unread |
| Config (defaults incl. providers, shortcuts, columns) | config.py 886 | **partial** — reader with defaults; no writer/settings model yet |
| Feed discovery + site search | discovery.py 5,530 | **partial** — social transforms (Reddit/Bluesky/Mastodon/Lemmy/PieFed/Kbin/YouTube channel+playlist) + HTML alternate discovery + Add Feed resolution chain done; directory search (Feedsearch/Podcast Index/Apple/Fyyd), yt-dlp site search, synthetic forum subscriptions missing |
| Article extraction (layered cascade) | article_extractor.py 5,098 | **partial** — cascade + full gate-marker list, paywall/link-list rejection, JSON-LD, Register + Axios dedicated extractors, metered notice, zero-width strip (slices 1, 6); remaining: BBC/Sky/Bloomberg/GSMArena fixes, thread rebuilds (Reddit/Lemmy/forums), fetch fallback chain, lead recovery |
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
| Help system (92 topics; guide in 17 locales) | help_topics.py; docs/help/*.md | **done** — guide loader with locale fallback, topic split, plain-text viewer dialog |
| Shortcuts registry (70 actions, remappable) | shortcuts.py 439; gui/shortcut_keys.py; gui/hotkeys.py | **partial** — full registry extracted from Python, normalisation/conflicts, overrides dialog; live key dispatch for wired actions missing |

## 3. GUI surface (Python `gui/`)

- MainFrame: 127 user-action handlers. Rust app today covers:
  refresh all, add/remove feed, import/export OPML, sync providers,
  article open (mark read), delete, favorite, mark all read, deleted
  restore, search. **Missing:** feed properties/edit, feed errors
  view, per-feed/category refresh, stop refresh, sort controls,
  article/media filters, persistent searches, smart-folder UI,
  filter-rules UI, copy link/text variants, open in browser,
  downloads (article/media, download-as), podcast archive dialog,
  find in article (next/prev), article history, play queue UI,
  rich-view toggle, accessible browser, feed notifications toggles,
  feed images setting, backup/restore UI, site-cookies import UI,
  equalizer UI, YouTube account UI, settings dialog, check updates,
  user guide/about, keyboard shortcuts dialog, tray, global hotkeys.
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

</content>
