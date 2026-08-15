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
//! Detection is **not** wired to a button. `qupath_pipeline::run_collecting`
//! and [`qupath_viewer::Overlay`] exist and `examples/render.rs` drives them end
//! to end, so the remaining work is a background thread and a progress bar
//! rather than anything unknown.
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

mod chrome;
mod state;

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use state::App;

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

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("QuPath Rust")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 860.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("create the window"),
        );
        self.start(&window);
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // `RedrawRequested` is not input, and must never reach `egui`.
        //
        // It is the *answer* to a redraw request rather than a reason for one,
        // and `egui_winit` answers it with `repaint: true` — so passing it on
        // closes a loop that needs no input to sustain itself: render, receive
        // `RedrawRequested`, ask to render again. Measured at **90 frames a
        // second and 100% of one core** on a window sitting still, which is
        // precisely what `ControlFlow::Wait` is set to prevent.
        //
        // It is handled here, ahead of `egui`, rather than by testing the event
        // twice. See `docs/VIEWER.md` §3.6.
        if matches!(event, WindowEvent::RedrawRequested) {
            self.render(&window);
            return;
        }

        // Every *other* event goes to the interface first, which says whether
        // it wants it. Without this a drag that started on a slider also pans
        // the slide, and a scroll over a panel zooms the view underneath it.
        let Some(egui) = self.egui.as_mut() else {
            return;
        };
        let response = egui.on_window_event(&window, &event);
        if response.repaint {
            window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.resize(size.width, size.height);
                window.request_redraw();
            }
            other => {
                if self.input(&window, &other, response.consumed) {
                    window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // One more frame while tiles are still landing, and none once they have
        // all landed. This is what keeps the viewer at zero CPU at rest while
        // still filling in as a pan's tiles arrive.
        if self.wants_another_frame()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}
