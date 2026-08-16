//! Prints the pointer events a trackpad or wheel actually sends.
//!
//! Exists because **what a pinch produces is platform-specific and cannot be
//! read off the code**. `winit` 0.30 emits `PinchGesture` on macOS and iOS only
//! — its Wayland backend does not implement `zwp_pointer_gestures` — so on Linux
//! a two-finger pinch arrives as something else, usually ctrl with a scroll, and
//! the only way to know is to look.
//!
//! Run it, then: two-finger scroll, then pinch. The line kind and the delta
//! magnitudes are what `state::input`'s wheel arm has to divide down, and the
//! numbers here are what `TRACKPAD_PIXELS_PER_STEP` should be chosen against.
//!
//! ```sh
//! cargo run -p qupath-app --example gestures --release
//! ```
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct Probe {
    window: Option<Window>,
    seen: u32,
    ctrl: bool,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        self.window = Some(
            events
                .create_window(
                    Window::default_attributes().with_title("Scroll and pinch here; Escape to quit"),
                )
                .expect("create a window"),
        );
        println!("Two-finger scroll, then pinch. Escape closes the window.\n");
    }

    fn window_event(&mut self, events: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::MouseWheel { delta, phase, .. } => {
                self.seen += 1;
                let ctrl = if self.ctrl { "  [ctrl]" } else { "" };
                println!("{:>4}  MouseWheel    {delta:?}  {phase:?}{ctrl}", self.seen);
            }
            WindowEvent::PinchGesture { delta, phase, .. } => {
                self.seen += 1;
                println!("{:>4}  PinchGesture  delta={delta}  {phase:?}", self.seen);
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.ctrl = modifiers.state().control_key();
            }
            WindowEvent::CloseRequested => events.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state.is_pressed()
                    && event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) =>
            {
                println!("\n{} events seen.", self.seen);
                events.exit();
            }
            _ => {}
        }
    }
}

fn main() {
    EventLoop::new()
        .expect("an event loop")
        .run_app(&mut Probe::default())
        .expect("run");
}
