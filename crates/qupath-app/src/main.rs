//! A whole-slide viewer you can open a slide in.
//!
//! `crates/qupath-viewer` is the library and `examples/window.rs` is its
//! demonstration: it takes a slide path on the command line, and everything it
//! has to say arrives in the title bar. This is the same viewer with an
//! interface — a toolbar, a file dialog, a status bar, and somewhere to put a
//! message when a slide will not open.
//!
//! # What is here
//!
//! Opening a slide, and the viewer around it. Pan, zoom, level selection, the
//! loader and the cache all come straight from the library and are not
//! reimplemented; what this crate adds is chrome and the event loop that drives
//! it.
//!
//! Detection is a button, and [`detect`] is what it presses: a run of
//! `qupath_pipeline::run_collecting` over what the window is showing, on a
//! thread, drawn as a [`qupath_viewer::Overlay`] when it lands.
//!
//! # Why the interface is drawn twice
//!
//! `docs/VIEWER.md` §3.4b settled that a machine with no GPU still gets the
//! viewer, and §2 settled that the chrome is `egui`. Those conflict the moment
//! there is any chrome at all, because `egui-wgpu` needs an adapter.
//!
//! They are both kept. `egui` tessellates its whole interface to triangles
//! carrying a position, a texture coordinate and a premultiplied colour, and
//! `qupath_viewer::compose` rasterises exactly that — so the software path draws
//! the same interface with no driver installed, rather than falling back to a
//! bare window.
//!
//! # Running it
//!
//! ```text
//! cargo run -p qupath-app --release
//! cargo run -p qupath-app --release -- data/CMU-1.svs
//! ```
//!
//! A path on the command line is opened at startup; without one the window
//! comes up empty and asks for a slide. `--release` matters: a debug build
//! decodes JPEG tiles slowly enough to be annoying.

use winit::event_loop::{ControlFlow, EventLoop};

use qupath_app::state::App;

fn main() {
    let path = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"));

    let event_loop = EventLoop::new().expect("create the event loop");
    // `Wait`, not `Poll`. A viewer at rest has nothing to do, and a loop that
    // spins is a laptop with its fan on for a still picture. Everything that
    // needs a later frame — a tile arriving, an animation in the interface —
    // asks for one explicitly.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(path);
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
