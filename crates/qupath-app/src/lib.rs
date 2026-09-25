//! The application's interface, as a library.
//!
//! `main.rs` is the binary that opens a window and runs the event loop; this
//! exposes the same modules so they can be driven without one. The reason is
//! `docs/dev/VIEWER.md` §6: appearance is checked by looking, and looking at a
//! panel means laying it out and rasterising it, which `examples/chrome` does
//! with no window and no GPU.
//!
//! Nothing here is a public API for anyone else. It is a binary that also
//! happens to be importable by its own examples, which is why `missing_docs`
//! is off for it: these items are `pub` to reach an example in the same crate,
//! not to be depended on, and documenting them as though they were an API
//! would claim a stability this does not offer. The workspace lint stays on
//! everywhere it means something.
#![allow(
    missing_docs,
    reason = "pub only to reach this crate's own examples; see above"
)]

pub mod chrome;
pub mod detect;
pub mod inspect;
pub mod state;
pub mod thumbnails;
