# Changelog

Readable release history for BlindRSS, newest first. When adjacent releases
were part of the same fix stream, they are combined with a version range such
as `0.1.2-0.1.3`. Each release's section below is its GitHub release note,
taken from this file by the release workflow. The in-app updater shows the
same text.

## 0.1.68

- Release notes are now written for people, in this file, and both the GitHub release and the updater show them — no more bare "Full Changelog" compare link.
- Detect Audio no longer throws you back to the first article in the folder when it finds audio. The article's row is updated where it sits, and your place in the list is kept.
- Closing the app in a way that cannot be interrupted — for example while an update restarts it, or at Windows shutdown — can no longer trip an internal consistency check in the close handling. Both close handlers (the main window and the Podcast Archive dialog) now check whether a close may be vetoed before vetoing it.
- Skip Silence now reacts as quickly as the Python app: while it is on, the player checks the position every 280 ms instead of once a second, so skips land when the silence starts instead of up to a second late.

## 0.1.67

- The tools the app relies on — yt-dlp, Deno, ffmpeg and ffprobe — now ship inside the package in a `bin` folder, so playback and extraction helpers are present from the first run instead of being fetched afterwards.
- The macOS download is a real BlindRSS.app inside the dmg.
- Closing the window to the tray is now the default for new installs, matching the Python app. Your existing setting is unchanged.

## 0.1.66

- Cookie import now works with current Chrome, Edge and Brave profiles, whose cookies use the newer app-bound (v20) encryption: one Windows approval prompt the first time, then it runs silently. A browser database that is locked because the browser is open is read safely from a shadow copy instead of failing.

## 0.1.65

- Sites that block plain feed readers with browser fingerprint checks can now be fetched: a browser-impersonation stage serves both full-text extraction and feed refresh, with a per-feed setting of Auto, Always or Never.
- The yt-dlp download at startup could never finish — the download was cut off at 10 MB and every copy of yt-dlp is bigger than that. Downloads now stream to disk, and the error message is no longer doubled.
- YouTube channel feeds add and refresh again: when the channel feed itself refuses the request, the channel's uploads are listed through yt-dlp instead, the same recovery the Python app uses.

## 0.1.64

- Ctrl+Up and Ctrl+Down move by paragraph in the article reader again.
- Articles show their date, author and link header again, and full text shows title and author — and the blank lines between paragraphs that made the screen reader stop on empty lines are gone.
- The focus trap that could strand you on the Add Feed button after Alt+Tab is fixed structurally: the toolbar buttons and search box live on their own panel now.
- Menu items announce their shortcuts (for example "New Feed (Ctrl+N)"), and the labels update when you rebind a shortcut.
- A failed refresh no longer flashes a PowerShell window into the speech history; sounds play inside the app.
- Uncategorized sits at the bottom of the folder tree, below Smart Folders.
- Tabbing into the article list focuses and reads the first article.
- Detect Audio finds NPR audio: it runs the full detection pipeline (publisher routes, then yt-dlp) instead of only scanning the article page.
- While media plays, Ctrl+arrows drive the player from the main window: Left/Right seek, Up/Down volume.

## 0.1.63

- Chapters embedded in local MP4, M4A and M4B files are listed, and YouTube chapters arrive fast and stay consistent within a session.
- Cast discovery merges a TV that answers on two protocols into one entry with its video-capable path, and the repeated discovery searches find more devices.

## 0.1.62

- Escape closes every dialog in the app, and the cast queue window too.

## 0.1.61

- No more "debugging check" dialogs: the interface library is built the way release builds are meant to be, with its developer consistency checks compiled out.
- No console window opens when the app starts on Windows.

## 0.1.60

- A reconciliation sweep over the whole feature audit: stored browser cookies are pruned the way the Python app prunes them, the automatic full-text delay matches, and the remaining small divergences found by the audit are closed or recorded.

## 0.1.59

- Browser recovery for blocked pages, feed discovery and search, OPML and Google Takeout import, the archive manager, casting from the Caster engine, and the settings and help surfaces complete the feature audit's fourth wave.

## 0.1.57

- One player session everywhere, Podcasting 2.0 and YouTube chapters, the keyboard shortcut editor over one flat shortcut map, and Skip Silence rewritten on a real voice-activity detector.

## 0.1.56

- Refresh runs on workers so the window stays responsive, hosted providers (Inoreader, Miniflux and friends) sync completely, YouTube views merge like the Python app's, and translation requests follow the article you are reading.

## 0.1.55

- Data safety first: configuration saves are atomic, category changes are transactional, the Deleted Articles dialog is rebuilt, and updates move to the signed updater used by the other apps — every download is signature-checked before it is offered.

## 0.1.0-0.1.54

- The Rust build-out: the Python BlindRSS feature set transferred piece by piece — feeds and categories, the reader, the media player with resume and chapters, hosted providers, YouTube, casting, archives, and the signed self-updater — while the Python app remained the shipping product.
