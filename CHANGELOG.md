# Changelog

Readable release history for BlindRSS, newest first. When adjacent releases
were part of the same fix stream, they are combined with a version range such
as `0.1.2-0.1.3`. Each release's section below is its Forgejo release note,
taken from this file by the release workflow. The in-app updater shows the
same text.

## 0.1.106

- Windows Latest: image galleries no longer leave their title and slide counter ("Windows 11 running on iPhone Duo", "1 of 3") in the middle of articles, in the text view or the Rich View. Section headings stay.
- The Rich View no longer stops on empty image links, and Windows Latest articles no longer end with the "The post ... appeared first on Windows Latest" line in either view.

## 0.1.105

- Escape now closes every dialog: Feeds with Errors, About, Find a Podcast or RSS Feed, Podcast Archive, Video Search, Filter Rules, Import Site Cookies, YouTube Account, Keyboard Shortcuts, Add BlindRSS Shortcuts, Configure Persistent Search, Open Article and the rest. Escape does what the dialog's Close or Cancel button does.
- Times in Feeds with Errors (and Article History and Import Site Cookies) now show your computer's local time instead of GMT.
- Feed commands such as Remove Feed, Edit Feed, Refresh Feed and Feed Properties now tell you when no feed is selected, and category commands such as Remove Category tell you when no category is selected, instead of doing nothing.
- The Atlantic: recommended-article cards such as "[Ian Bogost: Airport chaos began long before the ground stop]" no longer appear in the middle of articles, in the text view or the Rich View.

## 0.1.104

- View Changelog now opens the whole release history as one page in your default web browser, the way BlindRadio does it — faster and more comfortable than the in-app viewer it replaces, which is gone.

## 0.1.103

- Detect Feeds on Page (Feeds menu) works again. It silently did nothing because its menu command shared an ID with Stop AirPlay Cast, so choosing it ran that instead — and with no cast running, nothing happened at all.
- Escape now closes the Add Feed dialog, like it does elsewhere. Before, cancelling out of Add Feed meant pressing the Cancel button.

## 0.1.102

- In the Rich Full-Text View, pressing Alt (or F10) no longer opens the menu silently. It does nothing there now, so NVDA keeps speaking. Escape or F6 returns to the article list, where Alt works as usual.

## 0.1.101

- The YouTube Account dialog (Tools menu) is now the full account manager: sign out, copy or open the sign-in code while one is waiting, choose the category your subscriptions go into, toggle automatic updates, and update the account right away — not just sign in.
- Settings path fields (download folder, playback cache, cookies file, backup folder, and the FFmpeg, FFprobe and yt-dlp paths) now have Browse buttons, and the media tools page shows the detected path for each tool.
- The YouTube settings page can import cookies from your browser with a guided flow: install the export extension, export your cookies, and BlindRSS finds the export in your Downloads.
- Video Search has a Filter Results choice that narrows the results to one site without searching again, and results that only had a web address for a title now pick up their real title in the background.
- Rokfin links that yt-dlp cannot play now try Rokfin's public stream directly, and tell you when a post needs a Rokfin login.
- The player now says "Beginning of track" or "End of track" when a seek cannot move any further.
- The notifications feed picker now works the way it reads: checked feeds send notifications, with Check All and Uncheck All buttons.
- The About dialog has buttons for the author's GitHub, the repository, and the changelog.
- The article right-click menu now says Add to Favorites or Remove from Favorites, matching what will actually happen.
- Playback now honours the playback_resolve_timeout_s setting for redirect lookups and yt-dlp timeouts.

## 0.1.100

- The Rich Full-Text View no longer shows video cards with "Watch on YouTube" links, "Related Roundup" footer lines, "[Read: ...]" and "[Watch: ...]" recommendation cards, newsletter promos, or Axios photo captions mid-article. Videos now show as a single playable embed.
- The Rich Full-Text View no longer traps keyboard focus: Shift+Tab at the top returns to the article list, Shift+F6 to the feed tree, F6 or Escape to the article list, and Ctrl+Shift+H switches back to plain text.

## 0.1.99

- TechCrunch articles no longer end with the affiliate disclosure ("When you purchase through links ...") and the trailing author bio with its "View Bio" line in full text.

## 0.1.98

- Atlantic articles no longer show the in-article recommendation cards such as "[Read: Is Marco Rubio the happiest Cabinet member?]" in full text.
- Sky News articles no longer show only "Powered and protected by Akamai". The Akamai bot wall is now recognised as a block page, so the normal fallback chain fetches the story text instead of displaying the block notice.

## 0.1.97

- Alt+Tab now keeps your place again. The last fix only restored focus at the moment Windows switched windows back, and wx could reset it to the All Articles tree right after. Focus is now also restored again a moment later, once the switch has settled.

## 0.1.96

- Skip Silence no longer clips the first words after a pause. Skips now land exactly where the silence ends instead of several hundred milliseconds into the next word.

## 0.1.95

- Switching back to BlindRSS with Alt+Tab now restores the control you were using when Windows activation falls back to the feed tree. Article-list focus, article-text selection, and other native controls are preserved. Clicking a different control still follows the mouse.

## 0.1.93

- Wired articles no longer end with the "Power up with unlimited access to WIRED ... Subscribe Today" sales pitch in full text.
- Fixed missing spaces between words in full text where a page puts a link or bold text inside italics, for example "Will Knight’sAI Lab" on Wired. These now read "Will Knight’s AI Lab".

## 0.1.92

- NPR stories without transcripts now show their short article text cleanly, without player controls, embed code or NPR's anti-fraud notice. Full transcripts and fuller feed text remain preserved.

## 0.1.91

- BlindRSS uses much less CPU while it sits idle. The tray's Unread count was recounted against the whole database about seven times a second; it now updates every two seconds, which removes most of the background load on large libraries.
- Chapters embedded in local MP4, M4A and M4B files now load. Earlier versions looked for them in the wrong place and found none, so audiobooks and podcasts with real chapters showed no chapter list.

## 0.1.90

- Fixed keyboard focus getting trapped between the Search field and Search button after switching back with Alt+Tab. Tab from the Search button, Shift+Tab from the field, and Escape from either control now return to the feed/category tree.
- Turning off Search in the View menu now hides both the field and button, including at startup. The search shortcut shows the row before focusing it.

## 0.1.89

- New: Help > View Changelog. The whole release history is built into the app and reads offline: pick a version from the list to jump straight to it, or just read through, and Find works the same as in the user guide. Handy when an update skips versions — upgrading from 0.1.85 to 0.1.88, for instance, you can now read what 0.1.86 and 0.1.87 changed.
- Fixed a gap in the release history itself: 0.1.87 never got its own release page. Its change (dialogs opened from the reader return to the reader) shipped inside 0.1.88 and is in the changelog there.

## 0.1.88

- Closing a dialog you opened while reading an article now puts you back in the article text, where you were, instead of in the article list.
- On a busy Mac, BlindRSS no longer mistakes the bundled yt-dlp for a broken copy and tries to replace it. yt-dlp can take a few seconds to start there, so a slow first start now gets one more try.

## 0.1.86

- A Miniflux (or other hosted account) sync finishing in the background no longer moves keyboard focus. Reading an article, browsing the list or typing in search, focus stays where it was instead of jumping to the feed tree. The tree is also left alone when the sync changed nothing in it, which removes the short freeze and the double "All Articles" announcement.
- The same focus protection now covers the live updates shown while a local refresh is still running.

## 0.1.85

- Miniflux refresh is much faster. After the first sync, BlindRSS asks the server only for articles added or changed since the last complete sync (new posts, read and starred changes) instead of downloading every feed's whole history each time, fetches feeds in parallel, and saves everything in one database write. The first refresh after updating still downloads everything once.
- A Miniflux refresh that fails part-way no longer leaves a half-updated article list.
- The Delete key works again. In the article list it deletes the selected articles, with the same confirmation as the context menu; Shift+Delete deletes without asking. In the feed tree it removes the selected feed or category, with its confirmation.
- Each folder keeps its own place in the article list. Visiting another folder and coming back to All Articles no longer moves you to the article you were on in that other folder.
- Until you pick an article, focus stays on the newest one: as a refresh brings in new articles, focus moves to them, like the Python app.
- While reading an article, the status bar shows your position as "Line 3 of 64", updated as you move, like the Python app.
- BBC articles show their publish date ("Published 9 November 2026") instead of a bare "Published".
- MacRumors articles keep the date and author line separate from the first paragraph.

## 0.1.84

- Article-list dates now match the Python app: recent articles say "51 minutes ago" or "2 hours ago"; articles older than 24 hours show local date and time without seconds or a timezone suffix. Half-hour timezones, including GMT+3:30, are supported.
- Bluesky and other feeds with missing article titles now use a short preview of the post text, like the Python app. Already-cached "No Title" articles also show their available description text without losing read or favorite status.

- Fixed a possible article-text crash when embedded scripts contain non-ASCII characters.

## 0.1.82

- YouTube playback and downloads survive more of YouTube's blocking. When every normal route fails, the app now makes one last attempt with a fresh anonymous browser session (the Python version's final retry), then gives up quietly when no browser session is available.
- Announcement speech no longer needs a screen reader running. On Windows it falls back to the built-in Windows voice when neither NVDA nor JAWS is speaking; on Linux it uses Speech Dispatcher.
- Miniflux users: feed fetches that the server blocks now recover through the browser fallback instead of failing outright.
- Stopping a frozen (single-file) yt-dlp no longer leaves orphaned worker processes and temp folders behind.

## 0.1.81

- Fixed the "Refresh complete" count missing most new articles. Only feeds that finished on the very last tick were counted, so a 54-feed refresh could report a handful of new articles while the list showed many more. The count now covers the whole refresh.

## 0.1.80

- New articles now appear while a refresh is still running. Before, the article list and unread counts only updated when the last feed finished; now each feed's articles show up as that feed completes, the way the Python version behaves. Your place in the list and your keyboard focus stay put while the rows arrive.
- Removed the main window's two rows of toolbar buttons. They trapped keyboard focus after Alt+Tab — once focus landed on a button there was no way back to the feed tree or the article list except restarting. Every action they offered is still available in the menus, the feed-tree and article-list context menus, or keyboard shortcuts; only the search field and its Search button remain. Deleted Articles is now also listed in the article list's context menu when the list is empty.

## 0.1.79

- Fixed the article text jumping back to the first line when you Tab away and back. The earlier attempt at this fix never ran in the main window: its focus handler was bound before another one that took the event first. There is now a single focus handler that restores your reading place, with a deferred restore for the way screen readers move focus, and your position is also tracked as you arrow through the text. A new article still starts at the top.
- Fixed a finished refresh yanking focus out of the article list into the Feeds tree. The tree now only rebuilds when its shape actually changed; when only unread counts changed it patches the labels in place, and when nothing changed it does nothing at all — and your focus stays where it was.
- Fixed GitHub feeds (commit and release feeds) failing with an error. GitHub intermittently answers feed requests that carry stale session cookies with a "try again" page instead of the feed. Such failures are now retried without the site cookies, and the retry is only taken when it actually returns a feed — otherwise you still see the original error.
- Fixed MacRumors articles still ending with the site's footer (push notifications pitch, Videos and Guides lists, Other Stories) when the extra text comes from the feed server itself rather than the article page. The footer is now cut from server-provided text too; what is stored stays untouched.

## 0.1.78

- The in-app updater now checks this repository's Forgejo releases instead of GitHub Releases. There is no GitHub fallback: Check for Updates looks only at the Forgejo host, and downloads still verify against the release signing key before anything installs.
- Release signing moved to the per-product key (key ID FC1444911E7601CC). The old shared key no longer verifies anything.

## 0.1.77

- Fixed MacRumors image credits still appearing in articles. The credit is not part of a figure caption — MacRumors prints it as a short line of its own after the photo ("Image credit: MacRumors forum member ..."), and it was gluing itself onto the next paragraph. Standalone image credit lines are now removed before the article is read, in the plain text view and the HTML view alike.
- Fixed the MacRumors footer still appearing at the end of articles when it arrives glued onto the article text with no paragraph break (push notifications pitch, subscribe links, Videos and Guides lists, Other Stories). The footer is now cut at its own wording wherever it starts, not only when it begins a paragraph.
- Fixed the article text still losing your place when you Tab away and back in the main window. The first fix for this was in place but never ran: another focus handler, bound after it, was taking the event first (wxWidgets runs focus handlers in reverse binding order). The reading-place restore is now bound last, so it actually runs. Your place is kept, and a new article still starts at the top.
- The status bar now reports refresh progress the way the Python version does. A full refresh counts "Checked 12 out of 55" as feeds finish, a failed feed names itself ("Error checking: Site Name (12 out of 55)"), refreshing one feed says "Refreshing feed: Site Name...", and a category refresh says "Refreshing category: Name...". Before, a refresh only announced the total number of feeds at the start and then went quiet.

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
