//! Wayland protocol handling.

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{
    BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler,
};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_touch, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use crate::geometry::Size;
use crate::wayland::fractional_scale::{FractionalScaleHandler, FractionalScaleManager};
use crate::wayland::viewporter::Viewporter;
use crate::{Error, State};

pub mod fractional_scale;
pub mod viewporter;

/// Wayland protocol globals.
#[derive(Debug)]
pub struct ProtocolStates {
    pub fractional_scale: Option<FractionalScaleManager>,
    pub compositor: CompositorState,
    pub registry: RegistryState,
    pub viewporter: Viewporter,
    pub xdg_shell: XdgShell,

    output: OutputState,
    seat: SeatState,
}

impl ProtocolStates {
    pub fn new(globals: &GlobalList, queue: &QueueHandle<State>) -> Result<Self, Error> {
        let registry = RegistryState::new(globals);
        let output = OutputState::new(globals, queue);
        let xdg_shell = XdgShell::bind(globals, queue)
            .map_err(|err| Error::WaylandProtocol("xdg_shell", err))?;
        let compositor = CompositorState::bind(globals, queue)
            .map_err(|err| Error::WaylandProtocol("wl_compositor", err))?;
        let viewporter = Viewporter::new(globals, queue)
            .map_err(|err| Error::WaylandProtocol("wp_viewporter", err))?;
        let fractional_scale = FractionalScaleManager::new(globals, queue).ok();
        let seat = SeatState::new(globals, queue);

        Ok(Self { fractional_scale, compositor, viewporter, xdg_shell, registry, output, seat })
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        factor: i32,
    ) {
        if self.protocol_states.fractional_scale.is_none() {
            self.window.set_scale_factor(factor as f64);
        }
    }

    fn frame(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
        self.window.draw();
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: Transform,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &WlSurface,
        _output: &WlOutput,
    ) {
    }
}
delegate_compositor!(State);

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.protocol_states.output
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}
delegate_output!(State);

impl WindowHandler for State {
    fn request_close(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _window: &Window,
    ) {
        self.terminated = true;
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        if let (Some(width), Some(height)) = configure.new_size {
            let size = Size::new(width.get(), height.get());
            self.window.set_size(&self.protocol_states.compositor, size);
        }

        // Ensure we draw at least once after initial configure.
        self.window.perform_initial_draw();
    }
}
delegate_xdg_window!(State);
delegate_xdg_shell!(State);

impl FractionalScaleHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        factor: f64,
    ) {
        self.window.set_scale_factor(factor);
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.protocol_states.seat
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _connection: &Connection,
        queue: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer if self.pointer.is_none() => {
                self.pointer = self.protocol_states.seat.get_pointer(queue, &seat).ok();
            },
            Capability::Touch if self.touch.is_none() => {
                self.touch = self.protocol_states.seat.get_touch(queue, &seat).ok();
            },
            _ => (),
        }
    }

    fn remove_capability(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    pointer.release();
                }
            },
            Capability::Touch => {
                if let Some(touch) = self.touch.take() {
                    touch.release();
                }
            },
            _ => (),
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}
delegate_seat!(State);

impl TouchHandler for State {
    fn down(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        _surface: WlSurface,
        _id: i32,
        position: (f64, f64),
    ) {
        self.window.touch_down(position.into());
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _time: u32,
        _id: i32,
        position: (f64, f64),
    ) {
        self.window.touch_motion(&self.config, position.into());
    }

    fn up(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        _id: i32,
    ) {
        self.window.touch_up();
    }

    fn cancel(&mut self, _connection: &Connection, _queue: &QueueHandle<Self>, _touch: &WlTouch) {}

    fn shape(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _major: f64,
        _minor: f64,
    ) {
    }

    fn orientation(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _orientation: f64,
    ) {
    }
}
delegate_touch!(State);

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            // Dispatch event to the window.
            match event.kind {
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    self.window.touch_down(event.position.into());
                },
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    self.window.touch_up();
                },
                _ => (),
            }
        }
    }
}
delegate_pointer!(State);

impl ProvidesRegistryState for State {
    registry_handlers![OutputState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}
delegate_registry!(State);
