# Changelog

Readable release history for BlindRSS, newest first. When adjacent releases
were part of the same fix stream, they are combined with a version range such
as `0.1.2-0.1.3`. Each release's section below is its GitHub release note,
taken from this file by the release workflow. The in-app updater shows the
same text.

## 0.1.76

- Fixed an extra chime or ding when the Settings dialog closes. The Save and Cancel buttons now carry the standard dialog button ids, the way the Python version's OK and Cancel buttons do, so pressing Enter in a field to save and close no longer falls through to the Windows default beep.

## 0.1.75

- Fixed figure captions and photo credits leaking into article text. Ars Technica, Windows Latest, and other sites print a caption and a "Credit:" line under each photo; both now stay out of the article, in the plain text view and the HTML view alike. Ars Technica also hid a second copy of each caption in its photo lightbox data; that copy is gone too.
- Fixed MacRumors articles carrying the site's Popular Stories rail in the middle of the article and its footer (push notifications pitch, Videos and Guides lists, Other Stories) at the end. Both are cut; the article body is untouched.
- Fixed Windows Latest articles ending with the "The post ... appeared first on Windows Latest" line. It is removed.
- Fixed the article text losing your place when you Tab away and back. Your reading position in the article is now kept — in the main window, in the separate article window, and in the article history and feed error details — and a new article still starts at the top.
- Fixed Find in the article text (Ctrl+F). Pressing Enter after typing your search now moves to the first match from where you were reading, instead of leaving the cursor on the first line. Shift+F3 at the first match now announces "No more occurrences", like F3 does at the last match.
- Fixed the feed tree's shape. It no longer opens on a "Feeds" parent that contains everything: like the Python version, the top level is now All Articles, Favorites, and your categories side by side, with Uncategorized at the bottom. All Articles is a plain entry in the list, not a folder.


## 0.1.74

- Fixed Settings changes silently not saving. Numeric
  settings were plain text boxes whose contents were only
  written when they parsed as a number, so clearing a
  field — for example clearing the Refresh Interval's
  300 to turn scheduled refresh off — quietly kept the
  old value. The Refresh Interval is now a list of
  presets from Never to 4 hours, like the Python
  version, and every other numeric setting is now a
  spin control that always holds a valid value, so what
  you see in Settings is always what gets saved.
- Pressing Enter in a Settings field now saves and
  closes the dialog, matching the Python version's OK
  button.

## 0.1.73

- Opening Settings and closing it again — with Escape, Cancel, or OK — could crash the app if the soundcard list had not finished loading yet: the background timer that fills the list in kept running after the window was gone and wrote into its controls. Fixed: the window's background timers now stop before the window closes. The same latent fault was fixed in the Video Search, Feed Errors, YouTube sign-in, and Import Cookies windows, which shared the pattern.

## 0.1.72

- Interface translations now run on patois (the gettext system by Quin that Paperback uses): the 15 language catalogs are compiled into the app itself instead of shipping as a folder of files, and wxWidgets' own stock strings — standard dialog buttons and the like — follow the same interface language for the first time. One visible removal: the Translation Updates settings are gone, because translations now arrive with app releases rather than downloading separately between them. Regional catalogs that existed but could never be selected before, such as Flemish (nl_BE), now work.

## 0.1.71

- Feeds failing with "feed cache: database is locked" during a refresh, and the app stalling so it was hard to move around while feeds refreshed: the database now uses the same connection setup as Python (WAL journal mode, 60-second busy timeout), so refresh workers writing no longer block reading and navigation.
- NPR program feeds refresh with no audio: fixed. NPR audio is now discovered per session, the way Python does it — every refresh re-resolves each story's audio from NPR's embed player, so stories whose audio posts after the story itself gain it on the next refresh, and NPR feeds are always fetched in full instead of being shortcut as unchanged.
- The Settings window took seconds to open and the app could destabilize after saving: opening Settings no longer builds a throwaway media player just to list soundcards. The list is filled in the background through a minimal audio-only instance, like Python, and the window opens immediately.

## 0.1.70

- The General tab's "Check for updates:" frequency (translation updates) gains a Never option: automatic update checking can now be fully disabled from that control. The manual Check for Updates button still works with Never selected.

## 0.1.69

- A refresh finishing while you read no longer throws you back to the top of the article list. The article you have selected and focused is kept by identity and restored at its new position after the rebuild, so new articles arriving above yours do not move you.
- Refreshes are silent by default now: the spoken "Refresh Feeds", "Refresh stopped" and "Refresh complete: N new articles" alerts are off unless you turn them on. Each has its own setting in Settings → Announcements — Start update, Stop update, and the new Refresh results row at the end of the list. The status bar still shows the result, and the completion sound is unchanged.

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
