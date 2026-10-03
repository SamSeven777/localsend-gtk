# Third-party notices

The bundled `assets/logo.png` is the unmodified LocalSend logo from
https://github.com/localsend/localsend. The receive/send layouts are based on
LocalSend's interface. Reference revision: `033d97511d1980db5283914f8a726f74d5fb7c17`.
LocalSend is copyright its contributors and licensed under Apache-2.0;
see `LICENSES/LocalSend-Apache-2.0.txt`.

`localsend-rs` is a separate MIT-licensed protocol implementation by CrossCopy,
pinned in Cargo.toml and copied into `vendor/localsend-rs` with local cancellation
changes. See that directory's `UPSTREAM.md` for provenance and modifications and
its `THIRD_PARTY_NOTICES.md` for embedded upstream assets. GTK4, libadwaita, ksni
and other dependencies retain their own
licenses. LocalSend GTK is an independent community client.

Material icon SVGs in assets/icons are from google/material-design-icons,
Copyright Google LLC, Apache-2.0. See LICENSES/Material-Icons-Apache-2.0.txt.

Material color generation uses `material-colors` 0.4.2, an MIT OR Apache-2.0
Rust port of Google's Material Color Utilities. Role mappings are checked
against the official app's pinned Dart `material_color_utilities` 0.13.0.
The Yaru palette values follow the official app's `yaru` 10.2.0 theme.


The English, Simplified Chinese, Traditional Chinese, Japanese and Korean
catalogs in `assets/i18n/{en,zh-CN,zh-TW,ja,ko}.json` are adapted from
LocalSend's `app/assets/i18n` at revision
`033d97511d1980db5283914f8a726f74d5fb7c17`, copyright LocalSend contributors,
Apache-2.0. The German, French, Spanish and Russian catalogs in
`assets/i18n/{de,fr,es,ru}.json` reproduce the importer output from official
revision `f9b0e361052d31a4fddd26c6b4e84310d566d828` under the same copyright
and license; Spanish uses upstream `es-ES.json`. All nine catalogs reproduce
the importer output from that revision. See `LICENSES/LocalSend-Apache-2.0.txt`.
The adaptation flattens
Slang keys into English source strings and resolves translation references;
`import-upstream.mjs` records the conversion. `gtk-*.json` supplements the
upstream terminology with wording specific to this GTK client.
