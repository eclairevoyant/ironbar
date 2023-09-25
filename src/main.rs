#![doc = include_str!("../README.md")]

use std::cell::{Cell, RefCell};
use std::env;
use std::future::Future;
use std::path::PathBuf;
use std::process::exit;
use std::rc::Rc;
use std::sync::mpsc;

use cfg_if::cfg_if;
#[cfg(feature = "cli")]
use clap::Parser;
use color_eyre::eyre::Result;
use color_eyre::Report;
use dirs::config_dir;
use gtk::gdk::{Display, Monitor};
use gtk::prelude::*;
use gtk::Application;
use smithay_client_toolkit::output::OutputInfo;
use tokio::runtime::Handle;
use tokio::spawn;
use tokio::task::{block_in_place, spawn_blocking};
use tracing::{debug, error, info, warn};
use universal_config::ConfigLoader;

use clients::wayland;

use crate::bar::create_bar;
use crate::bridge_channel::BridgeChannel;
use crate::cached_broadcast::Event;
use crate::config::{Config, MonitorConfig};
use crate::error::ExitCode;
use crate::global_state::GlobalState;
use crate::style::load_css;

mod bar;
mod bridge_channel;
mod cached_broadcast;
#[cfg(feature = "cli")]
mod cli;
mod clients;
mod config;
mod desktop_file;
mod dynamic_value;
mod error;
mod global_state;
mod gtk_helpers;
mod image;
#[cfg(feature = "ipc")]
mod ipc;
#[cfg(feature = "ipc")]
mod ironvar;
mod logging;
mod macros;
mod modules;
mod popup;
mod script;
mod style;
mod unique_id;

const GTK_APP_ID: &str = "dev.jstanger.ironbar";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
    let _guard = logging::install_logging();

    let global_state = Rc::new(RefCell::new(GlobalState::new()));

    cfg_if! {
        if #[cfg(feature = "cli")] {
            run_with_args(global_state).await;
        } else {
            start_ironbar(global_state);
        }
    }
}

#[cfg(feature = "cli")]
async fn run_with_args(global_state: Rc<RefCell<GlobalState>>) {
    let args = cli::Args::parse();

    match args.command {
        Some(command) => {
            let ipc = ipc::Ipc::new(global_state);
            match ipc.send(command).await {
                Ok(res) => cli::handle_response(res),
                Err(err) => error!("{err:?}"),
            };
        }
        None => start_ironbar(global_state).await,
    }
}

async fn start_ironbar(global_state: Rc<RefCell<GlobalState>>) {
    info!("Ironbar version {}", VERSION);
    info!("Starting application");

    let app = Application::builder().application_id(GTK_APP_ID).build();

    let output_bridge = BridgeChannel::<Event<OutputInfo>>::new();
    let output_tx = output_bridge.create_sender();

    let running = Rc::new(Cell::new(false));

    let global_state2 = global_state.clone();
    app.connect_activate(move |app| {
        if running.get() {
            info!("Ironbar already running, returning");
            return;
        }

        running.set(true);

        cfg_if! {
            if #[cfg(feature = "ipc")] {
                let ipc = ipc::Ipc::new(global_state2.clone());
                ipc.start(app);
            }
        }

        let style_path = env::var("IRONBAR_CSS").ok().map_or_else(
            || {
                config_dir().map_or_else(
                    || {
                        let report = Report::msg("Failed to locate user config dir");
                        error!("{:?}", report);
                        exit(ExitCode::CreateBars as i32);
                    },
                    |dir| dir.join("ironbar").join("style.css"),
                )
            },
            PathBuf::from,
        );

        if style_path.exists() {
            load_css(style_path);
        }

        let (tx, rx) = mpsc::channel();

        #[cfg(feature = "ipc")]
        let ipc_path = ipc.path().to_path_buf();
        spawn_blocking(move || {
            rx.recv().expect("to receive from channel");

            info!("Shutting down");

            #[cfg(feature = "ipc")]
            ipc::Ipc::shutdown(ipc_path);

            exit(0);
        });

        let wc = wayland::get_client();

        let output_tx = output_tx.clone();
        let mut output_rx = lock!(wc).subscribe_outputs();

        spawn(async move {
            while let Some(event) = output_rx.recv().await {
                try_send!(output_tx.clone(), event);
            }
        });

        ctrlc::set_handler(move || send!(tx, ())).expect("Error setting Ctrl-C handler");
    });

    let config = load_config();

    {
        let app = app.clone();
        let global_state = global_state.clone();

        output_bridge.recv(move |event: cached_broadcast::Event<_>| {
            let display = get_display();
            match event {
                Event::Add(output) => {
                    debug!("Adding bar(s) for monitor {:?}", &output.name);
                    create_bars_for_monitor(&app, &display, &output, config.clone(), &global_state)
                        .unwrap();
                }
                // TODO: Implement
                Event::Remove(_) => {}
                Event::Replace(_, _) => {}
            }
            Continue(true)
        });
    }
    // Ignore CLI args
    // Some are provided by swaybar_config but not currently supported
    app.run_with_args(&Vec::<&str>::new());
}

/// Closes all current bars and entirely reloads Ironbar.
/// This re-reads the config file.
pub async fn reload(app: &Application, global_state: &Rc<RefCell<GlobalState>>) -> Result<()> {
    info!("Closing existing bars");
    let windows = app.windows();
    for window in windows {
        window.close();
    }

    let config = load_config();

    let wl = wayland::get_client();
    let wl = lock!(wl);
    let outputs = wl.get_outputs();

    let display = get_display();
    for output in outputs.iter() {
        create_bars_for_monitor(app, &display, output, config.clone(), global_state)?;
    }

    Ok(())
}
fn create_bars_for_monitor(
    app: &Application,
    display: &Display,
    output: &OutputInfo,
    config: Config,
    global_state: &Rc<RefCell<GlobalState>>,
) -> Result<()> {
    let Some(monitor_name) = &output.name else {
        return Ok(());
    };

    let monitor = match get_monitor(&monitor_name, display) {
        Ok(monitor) => monitor,
        Err(err) => return Err(err),
    };

    let Some(monitor_config) = config.get_monitor_config(&monitor_name) else {
        return Ok(());
    };

    match monitor_config {
        MonitorConfig::Single(config) => {
            create_bar(&app, &monitor, &monitor_name, config, global_state)
        }
        MonitorConfig::Multiple(configs) => configs
            .into_iter()
            .map(|config| create_bar(&app, &monitor, &monitor_name, config, global_state))
            .collect(),
    }
}
fn get_display() -> Display {
    Display::default().map_or_else(
        || {
            let report = Report::msg("Failed to get default GTK display");
            error!("{:?}", report);
            exit(ExitCode::GtkDisplay as i32)
        },
        |display| display,
    )
}

fn get_monitor(name: &str, display: &Display) -> Result<Monitor> {
    let wl = wayland::get_client();
    let wl = lock!(wl);
    let outputs = wl.get_outputs();

    let monitor = (0..display.n_monitors()).into_iter().find_map(|i| {
        let monitor = display.monitor(i)?;
        let output = outputs.get(i as usize)?;

        let is_match = output.name.as_ref().map(|n| n == name).unwrap_or_default();
        if is_match {
            Some(monitor)
        } else {
            None
        }
    });

    monitor.ok_or_else(|| Report::msg(error::ERR_OUTPUTS))
}

fn load_config() -> Config {
    let config = env::var("IRONBAR_CONFIG")
        .map_or_else(
            |_| ConfigLoader::new("ironbar").find_and_load(),
            ConfigLoader::load,
        )
        .unwrap_or_else(|err| {
            error!("Failed to load config: {}", err);
            warn!("Falling back to the default config");
            info!("If this is your first time using Ironbar, you should create a config in ~/.config/ironbar/");
            info!("More info here: https://github.com/JakeStanger/ironbar/wiki/configuration-guide");

            Config::default()
        });

    debug!("Loaded config file");
    config
}

/// Blocks on a `Future` until it resolves.
///
/// This is not an `async` operation
/// so can be used outside of an async function.
///
/// Do note it must be called from within a Tokio runtime still.
///
/// Use sparingly! Prefer async functions wherever possible.
///
/// TODO: remove all instances of this once async trait funcs are stable
pub fn await_sync<F: Future>(f: F) -> F::Output {
    block_in_place(|| Handle::current().block_on(f))
}
