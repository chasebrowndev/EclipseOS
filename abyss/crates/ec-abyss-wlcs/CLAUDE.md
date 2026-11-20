# ec-abyss-wlcs — conformance harness

Governing spec: COMP-15 §1, Vol 1 §15. Not TCB.

A `cdylib` that wlcs `dlopen`s. It runs one compositor per test on its own
thread and speaks to it only over `ec_abyss::backend::headless::WlcsEvent`; no
state is shared, so the single-threaded-core invariant is intact. Nothing here
may touch `AbyssState` directly — if a test needs something new, add an
injection entry point in `ec-abyss` and send an event to it.
