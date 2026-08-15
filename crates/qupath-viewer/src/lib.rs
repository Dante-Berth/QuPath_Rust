//! A whole-slide viewer on top of [`qupath_slide`].
//!
//! `docs/VIEWER.md` is the plan. This crate is **not** on the parity path and
//! claims no speedup: nothing here makes the detection pipeline faster, and no
//! fixture recorded from QuPath governs any of it, because QuPath's viewer is
//! not what is being ported.
//!
//! # What is here
//!
//! Milestones 3.1 to 3.4b of `docs/VIEWER.md` — a window showing a real slide,
//! pan and zoom, pyramid-level selection, tiles loaded off the main thread into
//! a cache with a hard byte budget, and a frame that can be composed with no
//! GPU at all.
//!
//! The split is deliberate. [`coords`], [`viewport`], [`cache`], [`loader`] and
//! [`compose`] are pure logic with no GPU and no window, so they carry the
//! tests; `docs/VIEWER.md` §6 explains why that matters here specifically,
//! given this repository's standing rule against asserting on appearance. The
//! window and the shader live in `examples/`, and `wgpu` is a dev-dependency,
//! so `cargo test -p qupath-viewer` builds none of the graphics tree.
//!
//! [`compose`] is the one that draws pixels, and it is in the library for the
//! same reason: a viewer whose entire draw call is axis-aligned textured quads
//! needs no adapter to produce a frame, so the machine with no GPU gets one and
//! the tests get to assert on where a tile actually landed.
//!
//! # What is not here
//!
//! Overlays — milestone 3.5. Detected objects drawn over the slide in slide
//! coordinates is where the viewer starts earning something back for the port.

pub mod cache;
pub mod compose;
pub mod coords;
pub mod loader;
pub mod viewport;

pub use cache::TileCache;
pub use compose::Framebuffer;
pub use coords::{LevelPoint, ScreenPoint, SlidePoint, TileIndex};
pub use loader::{Loaded, SlideTiles, TileLoader, TileSource};
pub use viewport::Viewport;
