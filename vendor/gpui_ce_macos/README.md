# Local GPUI macOS fix

Source: the crates.io `gpui_ce_macos` 0.1.0 package, checksum
`af0fe6b615e579ce0180198417815547c1c9ae654dc1e2382299f0c3f3564f79`.
Its Apache 2.0 license and normalized Cargo manifest are included.

The local change in `src/window.rs` stops synthetic drag updates when the
window closes, a new press replaces the gesture, or its physical mouse button
has been released without a native mouse-up reaching GPUI. Recovery sends one
mouse-up outside the viewport with a zero click count to cancel pending
click/drop state, followed by a neutral move at the actual pointer position.
Vamprowser's custom drag handlers recognize the exact combination of zero click
count and position `(-1, -1)` as cancellation, so an old drag location cannot
activate a control or commit a tab drop.
