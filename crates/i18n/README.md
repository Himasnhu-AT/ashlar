# ashlar-i18n

Translations for ashlar apps, small enough for wasm32: no full Unicode library, just what UI
strings need.

- **Catalogs**: flat JSON objects per locale (`{"files.other": "{count} files"}`), usually
  embedded with `include_str!` and parsed on first use. Later catalogs for a locale override
  earlier ones key by key, so an app layers its texts over a library's. A bad catalog is
  reported (`I18n::problems`, and a log warning) and acts as empty; nothing panics.
- **Lookups**: `text`, `format` (`{name}` placeholders, `{{` for a brace), `plural` (`key.one`,
  `key.few`, `key.other`… by CLDR rules for about 50 languages). The chain is locale →
  language → source locale (`zh-TW` → `zh-Hant` → `zh` → `en`); a key missing everywhere shows
  as itself.
- **Numbers**: `integer` and `decimal` with each locale's separators and grouping (`1,234.5`,
  `1.234,5`, `1 234,5`, `1,23,456`).
- **Locales**: `Locale::parse` reads BCP 47 and POSIX names (`de_DE.UTF-8` → `de-DE`),
  `system_locales()` reads `LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG`, `I18n::negotiate` picks
  the best available catalog.
- **Checks**: `I18n::check()` lists keys a locale lacks and placeholders that differ from the
  source; call it in a test.

```rust
use ashlar_i18n::{I18n, system_locales};

let mut i18n = I18n::new("en");
i18n.add("en", include_str!("../i18n/en.json")).add("de", include_str!("../i18n/de.json"));
let best = i18n.negotiate(&system_locales());
i18n.set_locale(&best);
let title = i18n.plural("export.done", 3, &[]);
```
