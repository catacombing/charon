use std::{env, process};

use calloop::{EventLoop, LoopHandle};
use calloop_wayland_source::WaylandSource;
use configory::{Manager as ConfigManager, Options as ConfigOptions};
use smithay_client_toolkit::reexports::client::globals::{
    self, BindError, GlobalError, GlobalList,
};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{
    ConnectError, Connection, DispatchError, QueueHandle,
};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use crate::config::{Config, ConfigEventHandler};
use crate::ui::window::Window;
use crate::wayland::ProtocolStates;

mod config;
mod geometry;
mod ui;
mod wayland;

mod gl {
    #![allow(clippy::all, unsafe_op_in_unsafe_fn)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

fn main() {
    // Setup logging.
    let directives = env::var("RUST_LOG").unwrap_or("warn,charon=info,configory=info".into());
    let env_filter = EnvFilter::builder().parse_lossy(directives);
    FmtSubscriber::builder().with_env_filter(env_filter).with_line_number(true).init();

    info!("Started Charon");

    if let Err(err) = run() {
        error!("[CRITICAL] {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    // Initialize Wayland connection.
    let connection = Connection::connect_to_env()?;
    let (globals, queue) = globals::registry_queue_init(&connection)?;

    let mut event_loop = EventLoop::try_new()?;
    let mut state = State::new(&event_loop.handle(), connection.clone(), &globals, queue.handle())?;

    // Insert wayland source into calloop loop.
    let wayland_source = WaylandSource::new(connection, queue);
    wayland_source.insert(event_loop.handle())?;

    // Start event loop.
    while !state.terminated {
        event_loop.dispatch(None, &mut state)?;
    }

    Ok(())
}

/// Application state.
struct State {
    protocol_states: ProtocolStates,

    pointer: Option<WlPointer>,
    touch: Option<WlTouch>,

    window: Window,
    config: Config,

    terminated: bool,

    _config_manager: ConfigManager<ConfigEventHandler>,
}

impl State {
    fn new(
        event_loop: &LoopHandle<'static, Self>,
        connection: Connection,
        globals: &GlobalList,
        queue: QueueHandle<Self>,
    ) -> Result<Self, Error> {
        let protocol_states = ProtocolStates::new(globals, &queue)?;

        // Initialize configuration state.
        let config_options = ConfigOptions::new("charon").notify(true);
        let config_handler = ConfigEventHandler::new(event_loop);
        let config_manager = ConfigManager::with_options(&config_options, config_handler)?;
        let config = config_manager
            .get::<&str, Config>(&[])
            .inspect_err(|err| error!("Config error: {err}"))
            .ok()
            .flatten()
            .unwrap_or_default();

        // Create the Wayland window.
        let window = Window::new(&protocol_states, connection, queue, &config)?;

        Ok(Self {
            protocol_states,
            config,
            window,
            _config_manager: config_manager,
            terminated: Default::default(),
            pointer: Default::default(),
            touch: Default::default(),
        })
    }
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("Wayland protocol error for {0}: {1}")]
    WaylandProtocol(&'static str, #[source] BindError),
    #[error("{0}")]
    WaylandDispatch(#[from] DispatchError),
    #[error("{0}")]
    WaylandConnect(#[from] ConnectError),
    #[error("{0}")]
    WaylandGlobal(#[from] GlobalError),
    #[error("{0}")]
    EventLoop(#[from] calloop::Error),
    #[error("{0}")]
    Configory(#[from] configory::Error),
    #[error("{0}")]
    Glutin(#[from] glutin::error::Error),
}

impl<T> From<calloop::InsertError<T>> for Error {
    fn from(err: calloop::InsertError<T>) -> Self {
        Self::EventLoop(err.error)
    }
}
