//! A whole-slide viewer on top of [`qupath_slide`].
//!
//! `docs/VIEWER.md` is the plan. This crate is **not** on the parity path and
//! claims no speedup: nothing here makes the detection pipeline faster, and no
//! fixture recorded from QuPath governs any of it, because QuPath's viewer is
//! not what is being ported.
//!
//! # What is here
//!
//! Milestones 3.1 and 3.2 of `docs/VIEWER.md` — a window, and one tile from a
//! real slide on screen — plus the coordinate types and viewport math that
//! milestone 3.3 needs.
//!
//! The split is deliberate. [`coords`] and [`viewport`] are pure arithmetic
//! over the level table, with no GPU and no window, so they carry the tests;
//! `docs/VIEWER.md` §6 explains why that matters here specifically, given this
//! repository's standing rule against asserting on appearance.
//!
//! # What is not here
//!
//! Pan and zoom input, asynchronous tile loading, the texture cache, and
//! overlays — milestones 3.3 through 3.5. [`Viewport`] already computes what
//! those need; nothing calls it from the render loop yet.

pub mod coords;
pub mod viewport;

pub use coords::{LevelPoint, ScreenPoint, SlidePoint, TileIndex};
pub use viewport::Viewport;
