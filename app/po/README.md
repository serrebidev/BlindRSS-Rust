# BlindRSS Translations

BlindRSS (Rust) uses gettext through the patois crates. English source
strings are the message keys; when no catalog matches the selected
language, the interface stays in English.

## Layout

```
app/po/
  blindrss.pot   <- template, regenerated from source
  <lang>.po      <- editable translation, one file per language
```

Catalogs are compiled at build time and embedded in the binary
(compressed; each language unpacks on first use). No locale folder
ships with the app, and translations update with app releases —
there is no separate translation download.

## Adding or updating a translation

1. Regenerate the template after string changes:

   ```
   cargo run --example gen_pot
   ```

   (from the app package; it collects the strings marked with
   `t()` / `nt()` in the app and any translatable libraries)

2. Start a new language (example: Russian):

   ```
   msginit -i app/po/blindrss.pot -o app/po/ru.po -l ru
   ```

   or update an existing one (keep `--no-fuzzy-matching`):

   ```
   msgmerge --update --backup=none --no-fuzzy-matching app/po/ru.po app/po/blindrss.pot
   ```

3. Translate the `msgstr` entries (any PO editor works, e.g. Poedit — which is
   screen-reader accessible — or a plain text editor).

4. Commit the `.po` file (and the regenerated `blindrss.pot` when source
   strings changed). The build needs the gettext tools on PATH
   (msgfmt); without them the app builds without translations and
   prints a Cargo warning.

5. Restart BlindRSS. The language is selected in Settings > General >
   "Interface language" ("Automatic" follows the OS locale), or via the
   `"language"` key in config.json.

Notes for translators:

- Keep `{placeholder}` tokens exactly as written; they are substituted at
  runtime (e.g. `Unread: {count}`). The same goes for `%s` / `%d`.
- An `&` marks the menu access key (e.g. `&File`); place it before whichever
  letter works best in your language. To show a **literal** ampersand in a
  label (e.g. the "Feeds && Articles" and "Startup && Tray" settings tabs), it
  must be doubled as `&&`. Never collapse `&&` back to a single `&` — wx would
  swallow it as a mnemonic and the character would vanish from the UI
  (issue #66). The patois catalog lint in the test suite guards placeholders,
  accelerators, and shortcuts.
- Trailing `...` means the item opens a dialog; keep it.
- Keep terminology consistent within a language. Pick one word for a recurring
  UI concept and use it everywhere. For example, in Russian "feed" is always
  "канал"; do not alternate between synonyms across strings (issue #66).

## Translation quality policy

- Human translations and corrections take priority over machine-generated
  suggestions. **Never overwrite an existing non-empty translation with a
  machine-generated one.** Automated passes may only fill entries that are
  still blank; entries a translator has already filled are off-limits unless
  that same translator asks for a change (issue #66). When source strings
  change, use `msgmerge` (which preserves existing translations) rather than
  re-translating from scratch.
- Do not fill every empty entry automatically. Short labels can be ambiguous
  without UI context; leave them blank until a translator can inspect the
  source reference or ask for clarification. Blank entries safely fall back
  to the English msgid when catalogs are compiled.
- Treat machine translation as a draft only. Do not commit it as final catalog
  text unless a translator for that language has reviewed it.
- Prefer complete, self-contained source messages over translating isolated
  fragments that may need different grammar in different contexts.
