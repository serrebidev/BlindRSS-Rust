# Rich View performance implementation plan

Goal: reduce time to readable rich articles without changing extraction quality,
stored data, native keyboard navigation, or screen-reader behavior.

Use the Python reader's existing approach: a bounded rendered-HTML cache,
debounced requests, and a stable WebView document. Retain the current sanitizer
and extraction cascade. Avoid dependency upgrades and workflow changes.

1. Extend `crates/core/src/reader_cache.rs` with debounced, deduplicated rich
   requests and bounded successful-result caching. Test stale results,
   invalidation, repeated selections, and failures before implementation.
2. Add `app/src/rich_view.rs` to queue/coalesce DOM replacements in one stable
   document, preserve base URLs and keyboard listeners, and acknowledge only
   the latest applied update. Integrate standalone article windows.
3. Integrate the cache and presenter into the main reader. Avoid redundant
   plain extraction while reading rich HTML; defer plain extraction until
   needed. Update parity documentation and benchmark the Netmeister example.
4. Review the complete diff, fix findings, bump above all published/tagged
   versions, and run formatting, zero-warning clippy, workspace tests, and app
   build. Commit, push, tag, and verify the published Forgejo assets and updater.

Validation includes rapid A/B/A selection, startup updates, repeated identical
HTML, changed article data, failed loads, account/settings invalidation,
language/base-URL preservation, and keyboard bridge retention. Automated
checks cannot substitute for the owner's NVDA acceptance.
