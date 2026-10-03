# Interface translations

`en.json`, `zh-CN.json`, `zh-TW.json`, `ja.json` and `ko.json` adapt the official
LocalSend catalogs from revision `033d97511d1980db5283914f8a726f74d5fb7c17`.
The German, French, Spanish and Russian catalogs (`de.json`, `fr.json`,
`es.json`, `ru.json`) reproduce the importer output from official revision
`f9b0e361052d31a4fddd26c6b4e84310d566d828`; the same revision also reproduces
the other five catalogs. Spanish uses upstream `es-ES.json`.
Copyright LocalSend contributors, Apache-2.0; see
`../../LICENSES/LocalSend-Apache-2.0.txt` and `../../THIRD_PARTY_NOTICES.md`.

`import-upstream.mjs` reproduces the conversion from `app/assets/i18n`: English
source strings become keys, direct Slang references are resolved, and missing or
incompatible template translations fall back to English. Translator metadata,
generated aliases and version-specific release notes are excluded. The first
occurrence is used for duplicate English strings, so common action terminology
comes from the upstream `general` section.

`gtk-*.json` supplies wording for GTK-specific flows and English aliases used
by this client. Keep the same keys and named placeholders in all nine files.
Do not overwrite these files when importing newer upstream catalogs. Both sets
are embedded in the application; no external translation service is used.

`scripts/generate-i18n.py` holds the German, French, Spanish and Russian GTK
translations and writes their `gtk-*.json` files after checking key coverage and
named placeholders against `gtk-en.json`. Keep edits to those four catalogs in
sync with the generator, otherwise regenerating will replace the edits.

Only explicitly authored interface strings should call `tr`, `tr_format`,
`tr_plural` or a binding helper. Never translate received messages, filenames,
aliases, paths, PINs or editable entry contents. Bind the entry's title or placeholder instead.
`bind_property` and `bind_format_property` replace older bindings on that same
object/property; status transitions should rebind with their current template.
Bindings hold weak object references. Runtime switches refresh these objects
without recreating the UI, changing user data or restarting networking.

Combo rows must ignore selected notifications while `is_updating()` is true,
and restore their selected index if their translated StringList is replaced.
Counted noun phrases use `tr_plural` or `bind_plural_property`, passing both the
singular and plural source strings and an integer count. These helpers supply
`{n}` and retain the count when changing language. Catalog entries may be plain
strings or an object with `one`, `few`, `many` and a required `other` fallback;
each form must keep every named placeholder from its source key. Russian uses
the [CLDR integer cardinal rules](https://unicode.org/cldr/charts/49/supplemental/language_plural_rules.html#ru)
for files, items, incoming offers and browser download requests/progress.
Ordinary count labels such as `Files: {n}` do not need noun agreement.
Template substitution is a single pass, so braces inside filenames or other
argument values remain literal.
