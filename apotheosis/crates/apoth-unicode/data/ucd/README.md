# Vendored Unicode Character Database, version 18.0.0

Inputs for `gen-tables` and the T-04 conformance tests. Source:
https://www.unicode.org/Public/18.0.0/ucd/ (files flattened into this directory):
`EastAsianWidth.txt`, `UnicodeData.txt`, `DerivedCoreProperties.txt`,
`emoji/emoji-data.txt`, `emoji/emoji-test.txt`, `auxiliary/GraphemeBreakProperty.txt`, `auxiliary/GraphemeBreakTest.txt`.

Licence: Unicode License v3 (https://www.unicode.org/license.txt); the files carry their
own copyright header. Data only, never compiled into a binary except via `src/tables.rs`.

Update: replace the files with a newer version, run
`cargo run -p apoth-unicode --bin gen-tables`, commit the regenerated `src/tables.rs`.
