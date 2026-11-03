# BlindRSS (Rust)

Rust port of [BlindRSS](https://github.com/serrebidev/BlindRSS), developed
in parallel with the shipping Python application. The Python app remains
the product until this port reaches feature parity under the migration
programme's acceptance checklist.

Design records: audit, research, benchmarks, and the staged plan live in
the migration workspace (`blindrss-rust-migration/reports/` on the
maintainer's machine); the key decisions:

- GUI: wxDragon 0.9.24 (wxWidgets), BlindFB workspace shape.
- Data: opens the existing Python `rss.db` and `config.json` in place.
  No re-setup, no destructive migration; Python can keep using the same
  database (dual-run safe).
- Feeds: feed-rs. Extraction: hybrid cascade — rust-trafilatura base,
  dom_smoothie (Readability) second opinion, ported BlindRSS gate/site
  layer owning every final call.
- Media: yt-dlp stays a managed executable; libVLC is the parity
  default. Casting ports Caster's engine as a shared core crate.

## Status

The current Python v2.5.8 versus Rust v0.1.53 source comparison is recorded in
[Audit 8](docs/AUDIT-8-2026-11-03.md), with 160 outstanding findings and a
[prioritized implementation plan](docs/superpowers/plans/2026-11-03-parity.md).
The early-stage description below is historical; full parity is not established.

Stage 1-3 first slice: data layer, config reader, RSS/Atom parsing,
extraction cascade v1 (gate markers, engine-disagreement rule,
prefer-feed-content, table re-append) with regression tests against the
benchmark corpus. App: main window (feed tree, article list, native
reader) over the existing database.

Screen-reader behaviour (NVDA/JAWS/VoiceOver/Orca) is a manual
acceptance gate and has NOT been verified yet for this port.

## Build

Needs the wxWidgets build dependencies for your platform (on this
Linux VM: libclang, GTK 3, GStreamer, WebKitGTK dev packages).

    cargo test -p blindrss-core
    cargo run -p blindrss

Set `BLINDRSS_DB` to point the app at a database; the dev default is
the Python checkout's `rss.db`.
