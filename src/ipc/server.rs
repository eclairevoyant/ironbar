use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use color_eyre::{Report, Result};
use glib::Continue;
use gtk::prelude::*;
use gtk::Application;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::spawn;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, error, info, warn};

use crate::bridge_channel::BridgeChannel;
use crate::ipc::{Command, Response};
use crate::ironvar::get_variable_manager;
use crate::modules::PopupButton;
use crate::style::load_css;
use crate::{await_sync, read_lock, send_async, try_send, write_lock, GlobalState};

use super::Ipc;

impl Ipc {
    /// Starts the IPC server on its socket.
    ///
    /// Once started, the server will begin accepting connections.
    pub fn start(&self, application: &Application) {
        let bridge = BridgeChannel::<Command>::new();
        let cmd_tx = bridge.create_sender();
        let (res_tx, mut res_rx) = mpsc::channel(32);

        let path = self.path.clone();

        if path.exists() {
            warn!("Socket already exists. Did Ironbar exit abruptly?");
            warn!("Attempting IPC shutdown to allow binding to address");
            Self::shutdown(&path);
        }

        spawn(async move {
            info!("Starting IPC on {}", path.display());

            let listener = match UnixListener::bind(&path) {
                Ok(listener) => listener,
                Err(err) => {
                    error!(
                        "{:?}",
                        Report::new(err).wrap_err("Unable to start IPC server")
                    );
                    return;
                }
            };

            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        if let Err(err) =
                            Self::handle_connection(stream, &cmd_tx, &mut res_rx).await
                        {
                            error!("{err:?}");
                        }
                    }
                    Err(err) => {
                        error!("{err:?}");
                    }
                }
            }
        });

        let application = application.clone();
        let global_state = self.global_state.clone();
        bridge.recv(move |command| {
            let res = Self::handle_command(command, &application, &global_state);
            try_send!(res_tx, res);
            Continue(true)
        });
    }

    /// Takes an incoming connections,
    /// reads the command message, and sends the response.
    ///
    /// The connection is closed once the response has been written.
    async fn handle_connection(
        mut stream: UnixStream,
        cmd_tx: &Sender<Command>,
        res_rx: &mut Receiver<Response>,
    ) -> Result<()> {
        let (mut stream_read, mut stream_write) = stream.split();

        let mut read_buffer = vec![0; 1024];
        let bytes = stream_read.read(&mut read_buffer).await?;

        let command = serde_json::from_slice::<Command>(&read_buffer[..bytes])?;

        debug!("Received command: {command:?}");

        send_async!(cmd_tx, command);
        let res = res_rx
            .recv()
            .await
            .unwrap_or(Response::Err { message: None });
        let res = serde_json::to_vec(&res)?;

        stream_write.write_all(&res).await?;
        stream_write.shutdown().await?;

        Ok(())
    }

    /// Takes an input command, runs it and returns with the appropriate response.
    ///
    /// This runs on the main thread, allowing commands to interact with GTK.
    fn handle_command(
        command: Command,
        application: &Application,
        global_state: &Rc<RefCell<GlobalState>>,
    ) -> Response {
        match command {
            Command::Inspect => {
                gtk::Window::set_interactive_debugging(true);
                Response::Ok
            }
            Command::Reload => {
                await_sync(async move { crate::reload(application, global_state).await }).unwrap();
                Response::Ok
            }
            Command::Set { key, value } => {
                let variable_manager = get_variable_manager();
                let mut variable_manager = write_lock!(variable_manager);
                match variable_manager.set(key, value) {
                    Ok(_) => Response::Ok,
                    Err(err) => Response::error(&format!("{err}")),
                }
            }
            Command::Get { key } => {
                let variable_manager = get_variable_manager();
                let value = read_lock!(variable_manager).get(&key);
                match value {
                    Some(value) => Response::OkValue { value },
                    None => Response::error("Variable not found"),
                }
            }
            Command::LoadCss { path } => {
                if path.exists() {
                    load_css(path);
                    Response::Ok
                } else {
                    Response::error("File not found")
                }
            }
            Command::TogglePopup { bar_name, name } => {
                let global_state = global_state.borrow();
                let response = global_state.with_popup_mut(&bar_name, |mut popup| {
                    let current_widget = popup.current_widget();
                    popup.hide();

                    let data = popup
                        .cache
                        .iter()
                        .find(|(_, (module_name, _))| module_name == &name)
                        .map(|module| (module, module.1 .1.buttons.first()));

                    match data {
                        Some(((&id, _), Some(button))) if current_widget != Some(id) => {
                            let button_id = button.popup_id();
                            popup.show(id, button_id);

                            Response::Ok
                        }
                        Some((_, None)) => Response::error("Module has no popup functionality"),
                        Some(_) => Response::Ok,
                        None => Response::error("Invalid module name"),
                    }
                });

                response.unwrap_or_else(|| Response::error("Invalid monitor name"))
            }
            Command::OpenPopup { bar_name, name } => {
                let global_state = global_state.borrow();
                let response = global_state.with_popup_mut(&bar_name, |mut popup| {
                    // only one popup per bar, so hide if open for another widget
                    popup.hide();

                    let data = popup
                        .cache
                        .iter()
                        .find(|(_, (module_name, _))| module_name == &name)
                        .map(|module| (module, module.1 .1.buttons.first()));

                    match data {
                        Some(((&id, _), Some(button))) => {
                            let button_id = button.popup_id();
                            popup.show(id, button_id);

                            Response::Ok
                        }
                        Some((_, None)) => Response::error("Module has no popup functionality"),
                        None => Response::error("Invalid module name"),
                    }
                });

                response.unwrap_or_else(|| Response::error("Invalid monitor name"))
            }
            Command::ClosePopup { bar_name } => {
                let global_state = global_state.borrow();
                let popup_found = global_state
                    .with_popup_mut(&bar_name, |mut popup| popup.hide())
                    .is_some();

                if popup_found {
                    Response::Ok
                } else {
                    Response::error("Invalid monitor name")
                }
            }
            Command::Ping => Response::Ok,
            Command::SetVisible { bar_name, visible } => {
                let windows = application.windows();
                let found = windows
                    .iter()
                    .find(|window| window.widget_name() == bar_name);

                if let Some(window) = found {
                    window.set_visible(visible);
                    Response::Ok
                } else {
                    Response::error("Bar not found")
                }
            }
            Command::GetVisible { bar_name } => {
                let windows = application.windows();
                let found = windows
                    .iter()
                    .find(|window| window.widget_name() == bar_name);

                if let Some(window) = found {
                    Response::OkValue {
                        value: window.is_visible().to_string(),
                    }
                } else {
                    Response::error("Bar not found")
                }
            }
        }
    }

    /// Shuts down the IPC server,
    /// removing the socket file in the process.
    ///
    /// Note this is static as the `Ipc` struct is not `Send`.
    pub fn shutdown<P: AsRef<Path>>(path: P) {
        fs::remove_file(&path).ok();
    }
}
