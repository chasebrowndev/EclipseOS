<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# vendor/

`smithay/` is smithay 0.7.0 from crates.io (MIT, third-party, so no SPDX
headers added) with the single change in
`docs/decisions/0079-smithay-argb-frame.patch` applied to
`src/xwayland/xwm/mod.rs`. ADR 0079. To re-derive: unpack the crate, `patch -p1`
that file. Drop this directory when upstream smithay carries the fix.
