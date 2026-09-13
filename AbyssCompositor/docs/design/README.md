# Vendored design reference

`eclipse-panes.html` is a copy of the Claude Design canvas for the Eclipse
panes. It ships its own `./support.js`, which is **not** vendored and is not
needed: the file is a *reading* reference, and the markup plus inline styles
are the payload. Opening it in a browser will render the artboards statically
and log one missing-script error; that is expected.

Five artboards, referenced throughout `docs/COMPOSITION.md`:
`4a` Display, `4b` Sound, `4c` Notifications, `4d` Power,
`4e` Privacy & security.

The accompanying style spec was **not** vendored here — it is byte-identical
to `docs/STYLE.md`, which is the copy the code and `tokens.rs` cite.
