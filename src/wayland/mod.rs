//! Wayland protocol handling.

use std::io::Write;
use std::sync::{Arc, Mutex};

use _text_input::zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3};
use _text_input::zwp_text_input_v3::{self, ZwpTextInputV3};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client as _text_input;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo,
};
use smithay_client_toolkit::seat::pointer::{
    BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler,
};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output,
    delegate_pointer, delegate_registry, delegate_seat, delegate_touch, delegate_xdg_shell,
    delegate_xdg_window, registry_handlers,
};

use crate::geometry::Size;
use crate::wayland::fractional_scale::{FractionalScaleHandler, FractionalScaleManager};
use crate::wayland::viewporter::Viewporter;
use crate::{Error, KeyboardState, State};

pub mod fractional_scale;
pub mod viewporter;

/// Wayland protocol globals.
#[derive(Debug)]
pub struct ProtocolStates {
    pub fractional_scale: Option<FractionalScaleManager>,
    pub data_device_manager: DataDeviceManagerState,
    pub compositor: CompositorState,
    pub data_device: DataDevice,
    pub registry: RegistryState,
    pub viewporter: Viewporter,
    pub xdg_shell: XdgShell,

    text_input: TextInputManager,
    output: OutputState,
    seat: SeatState,
}

impl ProtocolStates {
    pub fn new(globals: &GlobalList, queue: &QueueHandle<State>) -> Result<Self, Error> {
        let registry = RegistryState::new(globals);
        let text_input = TextInputManager::new(globals, queue);
        let output = OutputState::new(globals, queue);
        let xdg_shell = XdgShell::bind(globals, queue)
            .map_err(|err| Error::WaylandProtocol("xdg_shell", err))?;
        let compositor = CompositorState::bind(globals, queue)
            .map_err(|err| Error::WaylandProtocol("wl_compositor", err))?;
        let viewporter = Viewporter::new(globals, queue)
            .map_err(|err| Error::WaylandProtocol("wp_viewporter", err))?;
        let fractional_scale = FractionalScaleManager::new(globals, queue).ok();
        let seat = SeatState::new(globals, queue);
        let data_device_manager = DataDeviceManagerState::bind(globals, queue)
            .map_err(|err| Error::WaylandProtocol("wl_data_device_manager", err))?;

        // Get data device for the default seat.
        let default_seat = seat.seats().next().unwrap();
        let data_device = data_device_manager.get_data_device(queue, &default_seat);

        Ok(Self {
            data_device_manager,
            fractional_scale,
            data_device,
            text_input,
            compositor,
            viewporter,
            xdg_shell,
            registry,
            output,
            seat,
        })
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
            Capability::Keyboard if self.keyboard.is_none() => {
                let keyboard = self.protocol_states.seat.get_keyboard(queue, &seat, None).ok();
                self.keyboard = keyboard.map(KeyboardState::new);

                // Add new IME handler for this seat.
                self.text_input.push(self.protocol_states.text_input.text_input(queue, seat));
            },
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
        seat: WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                self.keyboard = None;

                // Remove IME handler for this seat.
                self.text_input.retain(|text_input| text_input.seat != seat);
            },
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

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
        _raws: &[u32],
        _keysyms: &[Keysym],
    ) {
        self.window.keyboard_enter();
    }

    fn leave(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _surface: &WlSurface,
        _serial: u32,
    ) {
        let keyboard_state = match &mut self.keyboard {
            Some(keyboard_state) => keyboard_state,
            None => return,
        };

        // Cancel active key repetition.
        keyboard_state.cancel_repeat(&self.event_loop);

        self.window.keyboard_leave();
    }

    fn press_key(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let keyboard_state = match &mut self.keyboard {
            Some(keyboard_state) => keyboard_state,
            None => return,
        };
        keyboard_state.press_key(&self.event_loop, event.time, event.raw_code, event.keysym);

        // Update pressed keys.
        self.window.press_key(event.raw_code, event.keysym, keyboard_state.modifiers);
    }

    fn release_key(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let keyboard_state = match &mut self.keyboard {
            Some(keyboard_state) => keyboard_state,
            None => return,
        };
        keyboard_state.release_key(&self.event_loop, event.raw_code);
    }

    fn repeat_key(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let keyboard_state = match &mut self.keyboard {
            Some(keyboard_state) => keyboard_state,
            None => return,
        };
        keyboard_state.press_key(&self.event_loop, event.time, event.raw_code, event.keysym);

        // Update pressed keys.
        self.window.press_key(event.raw_code, event.keysym, keyboard_state.modifiers);
    }

    fn update_modifiers(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        let keyboard_state = match &mut self.keyboard {
            Some(keyboard_state) => keyboard_state,
            None => return,
        };

        // Update pressed modifiers.
        keyboard_state.modifiers = modifiers;
    }

    fn update_repeat_info(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _keyboard: &WlKeyboard,
        repeat_info: RepeatInfo,
    ) {
        let keyboard_state = match &mut self.keyboard {
            Some(keyboard_state) => keyboard_state,
            None => return,
        };

        // Update keyboard repeat state.
        keyboard_state.repeat_info = repeat_info;
    }
}
delegate_keyboard!(State);

impl TouchHandler for State {
    fn down(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        time: u32,
        _surface: WlSurface,
        slot: i32,
        position: (f64, f64),
    ) {
        self.window.touch_down(slot, time, position.into());
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _time: u32,
        slot: i32,
        position: (f64, f64),
    ) {
        self.window.touch_motion(slot, position.into());
    }

    fn up(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        slot: i32,
    ) {
        self.window.touch_up(slot);
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
                PointerEventKind::Press { button: BTN_LEFT, time, .. } => {
                    self.pointer_down = true;

                    self.window.touch_down(-1, time, event.position.into());
                },
                PointerEventKind::Motion { .. } if self.pointer_down => {
                    self.window.touch_motion(-1, event.position.into());
                },
                PointerEventKind::Release { button: BTN_LEFT, .. } => {
                    self.window.touch_up(-1);

                    self.pointer_down = false;
                },
                PointerEventKind::Leave { .. } => self.pointer_down = false,
                _ => (),
            }
        }
    }
}
delegate_pointer!(State);

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}
impl DataSourceHandler for State {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        mut pipe: WritePipe,
    ) {
        let _ = pipe.write_all(self.clipboard.text.as_bytes());
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}
impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}
delegate_data_device!(State);

/// Factory for the zwp_text_input_v3 protocol.
#[derive(Debug)]
struct TextInputManager {
    manager: ZwpTextInputManagerV3,
}

impl TextInputManager {
    fn new(globals: &GlobalList, queue: &QueueHandle<State>) -> Self {
        let manager = globals.bind(queue, 1..=1, ()).unwrap();
        Self { manager }
    }

    /// Get a new text input handle.
    fn text_input(&self, queue: &QueueHandle<State>, seat: WlSeat) -> TextInput {
        let _text_input = self.manager.get_text_input(&seat, queue, Default::default());
        TextInput { _text_input, seat }
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for State {
    fn event(
        _state: &mut State,
        _input_manager: &ZwpTextInputManagerV3,
        _event: zwp_text_input_manager_v3::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<State>,
    ) {
        // No events.
    }
}

/// State for the zwp_text_input_v3 protocol.
#[derive(Default)]
struct TextInputState {
    surface: Option<WlSurface>,
    preedit_string: Option<(String, i32, i32)>,
    commit_string: Option<String>,
    delete_surrounding_text: Option<(u32, u32)>,
}

/// Interface for the zwp_text_input_v3 protocol.
pub struct TextInput {
    _text_input: ZwpTextInputV3,
    seat: WlSeat,
}

impl Dispatch<ZwpTextInputV3, Arc<Mutex<TextInputState>>> for State {
    fn event(
        state: &mut State,
        text_input: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        data: &Arc<Mutex<TextInputState>>,
        _connection: &Connection,
        _queue: &QueueHandle<State>,
    ) {
        let mut data = data.lock().unwrap();
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                state.window.text_input_enter(text_input.clone());
                data.surface = Some(surface);
            },
            zwp_text_input_v3::Event::Leave { surface } => {
                if data.surface.as_ref() == Some(&surface) {
                    state.window.text_input_leave();
                    data.surface = None;
                }
            },
            zwp_text_input_v3::Event::PreeditString { text, cursor_begin, cursor_end } => {
                data.preedit_string = Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            },
            zwp_text_input_v3::Event::CommitString { text } => {
                data.commit_string = Some(text.unwrap_or_default());
            },
            zwp_text_input_v3::Event::DeleteSurroundingText { before_length, after_length } => {
                data.delete_surrounding_text = Some((before_length, after_length));
            },
            zwp_text_input_v3::Event::Done { .. } => {
                let preedit_string = data.preedit_string.take().unwrap_or_default();
                let delete_surrounding_text = data.delete_surrounding_text.take();
                let commit_string = data.commit_string.take();

                if let Some((before_length, after_length)) = delete_surrounding_text {
                    state.window.delete_surrounding_text(before_length, after_length);
                }
                if let Some(text) = commit_string {
                    state.window.commit_string(text);
                }
                let (text, cursor_begin, cursor_end) = preedit_string;
                state.window.set_preedit_string(text, cursor_begin, cursor_end);
            },
            _ => unreachable!(),
        }
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![OutputState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}
delegate_registry!(State);
