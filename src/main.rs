use std::time::{Duration, SystemTimeError};
use std::{env, process};

use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use configory::{Manager as ConfigManager, Options as ConfigOptions};
#[cfg(feature = "profiling")]
use profiling::puffin;
#[cfg(feature = "profiling")]
use puffin_http::Server;
use smithay_client_toolkit::data_device_manager::data_source::CopyPasteSource;
use smithay_client_toolkit::reexports::client::globals::{
    self, BindError, GlobalError, GlobalList,
};
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::WlKeyboard;
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{
    ConnectError, Connection, DispatchError, QueueHandle,
};
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers, RepeatInfo};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use crate::config::{Config, ConfigEventHandler};
use crate::ui::view::View;
use crate::ui::view::map::MapView;
use crate::ui::window::Window;
use crate::wayland::{ProtocolStates, TextInput};

mod config;
mod entity_type;
mod geocoder;
mod geometry;
mod region;
mod tiles;
mod ui;
mod wayland;

mod gl {
    #![allow(clippy::all, unsafe_op_in_unsafe_fn)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

#[tokio::main]
async fn main() {
    // Setup logging.
    let directives = env::var("RUST_LOG").unwrap_or("warn,charon=info,configory=info".into());
    let env_filter = EnvFilter::builder().parse_lossy(directives);
    FmtSubscriber::builder().with_env_filter(env_filter).with_line_number(true).init();

    // Start profiling server.
    #[cfg(feature = "profiling")]
    let _server = {
        puffin::set_scopes_on(true);
        Server::new(&format!("0.0.0.0:{}", puffin_http::DEFAULT_PORT)).unwrap()
    };

    info!("Started Charon");

    if let Err(err) = run().await {
        error!("[CRITICAL] {err}");
        process::exit(1);
    }
}

async fn run() -> Result<(), Error> {
    // Initialize Wayland connection.
    let connection = Connection::connect_to_env()?;
    let (globals, queue) = globals::registry_queue_init(&connection)?;

    let mut event_loop = EventLoop::try_new()?;
    let mut state = State::new(event_loop.handle(), connection.clone(), &globals, queue.handle())?;

    // Insert wayland source into calloop loop.
    let wayland_source = WaylandSource::new(connection, queue);
    wayland_source.insert(event_loop.handle())?;

    // Start event loop.
    while !state.terminated {
        event_loop.dispatch(None, &mut state)?;
    }

    // Ensure database is cleanly terminated.
    let map_view: &mut MapView = state.window.views.get_mut(View::Map).unwrap();
    map_view.tiles().fs_cache().close().await;

    Ok(())
}

/// Application state.
struct State {
    event_loop: LoopHandle<'static, Self>,
    protocol_states: ProtocolStates,

    keyboard: Option<KeyboardState>,
    text_input: Vec<TextInput>,
    pointer: Option<WlPointer>,
    clipboard: ClipboardState,
    touch: Option<WlTouch>,
    pointer_down: bool,

    window: Window,

    terminated: bool,

    _config_manager: ConfigManager<ConfigEventHandler>,
}

impl State {
    fn new(
        event_loop: LoopHandle<'static, Self>,
        connection: Connection,
        globals: &GlobalList,
        queue: QueueHandle<Self>,
    ) -> Result<Self, Error> {
        let protocol_states = ProtocolStates::new(globals, &queue)?;

        // Initialize configuration state.
        let config_options = ConfigOptions::new("charon").notify(true);
        let config_handler = ConfigEventHandler::new(&event_loop);
        let config_manager = ConfigManager::with_options(&config_options, config_handler)?;
        let config = config_manager
            .get::<&str, Config>(&[])
            .inspect_err(|err| error!("Config error: {err}"))
            .ok()
            .flatten()
            .unwrap_or_default();

        // Create the Wayland window.
        let window = Window::new(&event_loop, &protocol_states, connection, queue, config)?;

        Ok(Self {
            protocol_states,
            event_loop,
            window,
            _config_manager: config_manager,
            pointer_down: Default::default(),
            terminated: Default::default(),
            text_input: Default::default(),
            clipboard: Default::default(),
            keyboard: Default::default(),
            pointer: Default::default(),
            touch: Default::default(),
        })
    }
}

/// Key status tracking for WlKeyboard.
pub struct KeyboardState {
    wl_keyboard: WlKeyboard,
    repeat_info: RepeatInfo,
    modifiers: Modifiers,

    current_repeat: Option<CurrentRepeat>,
}

impl Drop for KeyboardState {
    fn drop(&mut self) {
        self.wl_keyboard.release();
    }
}

impl KeyboardState {
    pub fn new(wl_keyboard: WlKeyboard) -> Self {
        Self {
            wl_keyboard,
            repeat_info: RepeatInfo::Disable,
            current_repeat: Default::default(),
            modifiers: Default::default(),
        }
    }

    /// Handle new key press.
    fn press_key(
        &mut self,
        event_loop: &LoopHandle<'static, State>,
        time: u32,
        raw: u32,
        keysym: Keysym,
    ) {
        // Update key repeat timers.
        if !keysym.is_modifier_key() {
            self.request_repeat(event_loop, time, raw, keysym);
        }
    }

    /// Handle new key release.
    fn release_key(&mut self, event_loop: &LoopHandle<'static, State>, raw: u32) {
        // Cancel repetition if released key is being repeated.
        if self.current_repeat.as_ref().is_some_and(|repeat| repeat.raw == raw) {
            self.cancel_repeat(event_loop);
        }
    }

    /// Stage new key repetition.
    fn request_repeat(
        &mut self,
        event_loop: &LoopHandle<'static, State>,
        time: u32,
        raw: u32,
        keysym: Keysym,
    ) {
        // Ensure all previous events are cleared.
        self.cancel_repeat(event_loop);

        let (delay_ms, rate) = match self.repeat_info {
            RepeatInfo::Repeat { delay, rate } => (delay, rate),
            _ => return,
        };

        // Stage timer for initial delay.
        let delay = Duration::from_millis(delay_ms as u64);
        let interval = Duration::from_millis(1000 / rate.get() as u64);
        let timer = Timer::from_duration(delay);
        let repeat_source = event_loop.insert_source(timer, move |_, _, state| {
            let keyboard = match state.keyboard.as_mut() {
                Some(keyboard) => keyboard,
                None => return TimeoutAction::Drop,
            };

            state.window.press_key(raw, keysym, keyboard.modifiers);

            TimeoutAction::ToDuration(interval)
        });

        match repeat_source {
            Ok(repeat_source) => {
                self.current_repeat = Some(CurrentRepeat::new(repeat_source, raw, time, delay_ms));
            },
            Err(err) => error!("Failed to stage key repeat timer: {err}"),
        }
    }

    /// Cancel currently staged key repetition.
    fn cancel_repeat(&mut self, event_loop: &LoopHandle<'static, State>) {
        if let Some(CurrentRepeat { repeat_source, .. }) = self.current_repeat.take() {
            event_loop.remove(repeat_source);
        }
    }
}

/// Active keyboard repeat state.
pub struct CurrentRepeat {
    repeat_source: RegistrationToken,
    interval: u32,
    time: u32,
    raw: u32,
}

impl CurrentRepeat {
    pub fn new(repeat_source: RegistrationToken, raw: u32, time: u32, interval: u32) -> Self {
        Self { repeat_source, time, interval, raw }
    }

    /// Get the next key event timestamp.
    pub fn next_time(&mut self) -> u32 {
        self.time += self.interval;
        self.time
    }
}

/// Clipboard content cache.
#[derive(Default)]
struct ClipboardState {
    serial: u32,
    text: String,
    source: Option<CopyPasteSource>,
}

impl ClipboardState {
    fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
    }
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("{0}")]
    SqlMigrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0}")]
    AtomicMove(#[from] tempfile::PersistError),
    #[error("{0}")]
    TokioJoin(#[from] tokio::task::JoinError),
    #[error("{0}")]
    WaylandDispatch(#[from] DispatchError),
    #[error("{0}")]
    WaylandConnect(#[from] ConnectError),
    #[error("{0}")]
    Glutin(#[from] glutin::error::Error),
    #[error("{0}")]
    SystemTime(#[from] SystemTimeError),
    #[error("{0}")]
    Configory(#[from] configory::Error),
    #[error("{0}")]
    WaylandGlobal(#[from] GlobalError),
    #[error("{0}")]
    EventLoop(#[from] calloop::Error),
    #[error("{0}")]
    Request(#[from] reqwest::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Sql(#[from] sqlx::Error),

    #[error("Wayland protocol error for {0}: {1}")]
    WaylandProtocol(&'static str, #[source] BindError),
    #[error("URL {0:?} is not a valid image")]
    InvalidImage(String),
    #[error("Missing user cache directory")]
    MissingCacheDir,
    #[error("Unexpected root path")]
    UnexpectedRoot,
}

impl<T> From<calloop::InsertError<T>> for Error {
    fn from(err: calloop::InsertError<T>) -> Self {
        Self::EventLoop(err.error)
    }
}
