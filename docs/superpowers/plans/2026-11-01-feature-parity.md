# BlindRSS source parity implementation plan

**Goal:** Compare the current Python application in `../BlindRSS` against this Rust port and port missing behavior, including small commands and existing-data compatibility.

**Architecture:** Keep the native wxWidgets interface and existing core modules. Use Python behavior and regression fixtures as the specification. Extend shared implementations and their callers rather than adding parallel paths.

**Tech stack:** Rust, wxDragon, SQLite, external yt-dlp/ffmpeg, dynamically loaded libVLC.

## Constraints

- Keep Python databases and config files usable by both applications.
- Preserve keyboard operation, native control names, and predictable focus.
- Leave the Python checkout and personal application data unchanged.
- Do not publish, tag, or change release versions as part of this port.
- Record unported behavior honestly; passing unit tests does not establish screen-reader or device acceptance.

## Implementation and checks

- [x] Compare source, menus, settings, providers, extraction, playback, discovery, and tests; distinguish stale checklist entries from real gaps.
- [x] Port feed-link selection, tags, rules, and Takeout compatibility; prove with database and parser regressions.
- [x] Port hosted pagination and read/favorite/content synchronization; prove with local HTTP fixtures and preserved local data.
- [x] Port playback resume compatibility, preferences, and active-player commands; prove policy with old/new keys and boundary values.
- [x] Add libVLC behind the existing native player; check loading, state, controls, and lifecycle against the installed runtime.
- [x] Track actual downloaded files and prefer them for playback; test legacy config and missing-file fallback.
- [x] Port existing command dispatch, copy actions, sorting, tray behavior, help lookup, and article-window controls.
- [x] Port directory search and missing subscription transforms; integrate worker results into the existing discovery dialog.
- [x] Port bounded multipage extraction and configurable article structure; test loops, origins, limits, and semantic markers.
- [x] Port synthetic forum subscriptions through existing subscribe and refresh seams.
- [x] Run formatting, workspace checks, and tests; review integration and record residual gaps and manual acceptance requirements. Independent agent review was interrupted by the worker usage limit.

## Review focus

- Python tag/resume/download formats survive loading and subsequent writes.
- Remote failure leaves the local cache unchanged; all pages are fetched without loops.
- UI shortcuts invoke the same actions as menus and respect native edit-control keys.
- Background jobs cannot reuse stale search or playback results for another selection.
- Missing VLC, deleted downloaded files, closed windows, and extraction failures retain functional fallbacks.

## Audit outcome

Implemented ports and outstanding work are recorded in `docs/PARITY-2026-11-01.md`. These checked tasks cover this implementation batch; they do not establish complete Python feature parity.
