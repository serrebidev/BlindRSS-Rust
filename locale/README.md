# BlindRSS Translations

BlindRSS uses gettext for interface internationalization (issue #44). English
source strings are the message keys; when no catalog matches the selected
language, the interface stays in English.

## Layout

```
locale/
  blindrss.pot                     <- template, regenerated from source
  <lang>/LC_MESSAGES/blindrss.po   <- editable translation
  <lang>/LC_MESSAGES/blindrss.mo   <- generated catalog the app loads
```

## Adding or updating a translation

1. Regenerate the template after string changes:

   ```
   python tools/extract_strings.py
   ```

2. Start a new language (example: Russian):

   ```
   msginit -i locale/blindrss.pot -o locale/ru/LC_MESSAGES/blindrss.po -l ru
   ```

   or update an existing one:

   ```
   msgmerge -U locale/ru/LC_MESSAGES/blindrss.po locale/blindrss.pot
   ```

3. Translate the `msgstr` entries (any PO editor works, e.g. Poedit — which is
   screen-reader accessible — or a plain text editor).

4. Translators commit the `.po` file. Source changes that add or remove
   messages must also commit the regenerated `locale/blindrss.pot`. Generated
   `.mo` files are build artifacts and are ignored by Git.

5. Restart BlindRSS. The language is selected in Settings > General >
   "Interface language" ("Automatic" follows the OS locale), or via the
   `"language"` key in config.json.

During `build.bat build`, `build.bat release`, and `build.sh build`,
`tools/compile_translations.py` compiles every
`locale/<lang>/LC_MESSAGES/blindrss.po` file to `blindrss.mo` before
PyInstaller runs. The generated catalogs are bundled automatically by
`main.spec` / `portable.spec`.

For local source-tree testing without a full build, run:

```
python tools/compile_translations.py
```

Notes for translators:

- Keep `{placeholder}` tokens exactly as written; they are substituted at
  runtime (e.g. `Unread: {count}`).
- An `&` marks the menu access key (e.g. `&File`); place it before whichever
  letter works best in your language. To show a **literal** ampersand in a
  label (e.g. the "Feeds && Articles" and "Startup && Tray" settings tabs), it
  must be doubled as `&&`. Never collapse `&&` back to a single `&` — wx would
  swallow it as a mnemonic and the character would vanish from the UI
  (issue #66). `tests/test_i18n.py` guards these labels.
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
