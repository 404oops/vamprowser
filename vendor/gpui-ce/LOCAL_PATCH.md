# Local GPUI popup frame pacing fix

Source: the crates.io `gpui-ce` 0.2.2 package, checksum
`b0af79c6659e0fea67773cfbd751fc5c8e51be00139827f365d7d1237a468a4d`.
The published sources, examples, tests, resources, documentation, normalized
Cargo manifest and Apache 2.0 license are included. Registry cache metadata and
the package's generated lockfile/workspace manifest are omitted.

`src/window.rs` exempts `WindowKind::PopUp` from the inactive-window 30 fps cap.
These windows deliberately keep keyboard focus in their parent while showing
short animations. Normal inactive windows retain their cap; serious/critical
thermal pressure retains its 60 fps cap. This change does not request frames,
force presentation, or redraw an idle popup. The frame policy has regression
tests covering popup motion, ordinary background windows, thermal pressure and
idle/presentation behavior.

Run `cargo test --test gpui_popup_frame_pacing` from the application root.
This harness compiles the exact production policy with GPUI's public types;
the published crate's full lib-test suite references font fixtures that are
not included in its package.
