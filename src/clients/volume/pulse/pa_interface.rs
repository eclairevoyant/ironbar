use std::ops::Deref;

// use lazy_static::lazy_static;
use libpulse_binding::proplist::Proplist;
use tracing::{debug, error, info};
// use state::Storage;
use color_eyre::{Report, Result};

use super::{callbacks, common::*, pa_actions};

// lazy_static! {
// 	pub static ref ACTIONS_SX: Storage<mpsc::UnboundedSender<EntryUpdate>> = Storage::new();
// }

pub async fn start(
	mut internal_rx: mpsc::Receiver<PAInternal>,
	info_sx: mpsc::UnboundedSender<EntryIdentifier>,
	actions_sx: mpsc::UnboundedSender<EntryUpdate>,
) -> Result<()> {
	// (*ACTIONS_SX).set(actions_sx);

	// Create new mainloop and context
	let mut proplist = Proplist::new().unwrap();
	proplist
		.set_str(libpulse_binding::proplist::properties::APPLICATION_NAME, "RsMixer")
		.unwrap();

	debug!("[PAInterface] Creating new mainloop");
	let mainloop = Rc::new(RefCell::new(match Mainloop::new() {
		Some(ml) => ml,
		None => {
			error!("[PAInterface] Error while creating new mainloop");
			return Err(Report::msg("Error while creating new mainloop"));
		}
	}));

	debug!("[PAInterface] Creating new context");
	let context = Rc::new(RefCell::new(
		match PAContext::new_with_proplist(
			mainloop.borrow_mut().deref().deref(),
			"RsMixerContext",
			&proplist,
		) {
			Some(ctx) => ctx,
			None => {
				error!("[PAInterface] Error while creating new context");
				return Err(Report::msg("Error while creating new context"));
			}
		},
	));

	// PAContext state change callback
	{
		debug!("[PAInterface] Registering state change callback");
		let ml_ref = Rc::clone(&mainloop);
		let context_ref = Rc::clone(&context);
		context
			.borrow_mut()
			.set_state_callback(Some(Box::new(move || {
				let state = unsafe { (*context_ref.as_ptr()).get_state() };
				if matches!(
					state,
					libpulse_binding::context::State::Ready
						| libpulse_binding::context::State::Failed
						| libpulse_binding::context::State::Terminated
				) {
					unsafe { (*ml_ref.as_ptr()).signal(false) };
				}
			})));
	}

	// Try to connect to pulseaudio
	debug!("[PAInterface] Connecting context");

	if context
		.borrow_mut()
		.connect(None, libpulse_binding::context::FlagSet::NOFLAGS, None)
		.is_err()
	{
		error!("[PAInterface] Error while connecting context");
		return Err(Report::msg("Error while connecting context"));
	}

	info!("[PAInterface] Starting mainloop");

	// start mainloop
	mainloop.borrow_mut().lock();

	if let Err(err) = mainloop.borrow_mut().start() {
		return Err(Report::new(err));
	}

	debug!("[PAInterface] Waiting for context to be ready...");
	// wait for context to be ready
	loop {
		match context.borrow_mut().get_state() {
			libpulse_binding::context::State::Ready => {
				break;
			}
			libpulse_binding::context::State::Failed | libpulse_binding::context::State::Terminated => {
				mainloop.borrow_mut().unlock();
				mainloop.borrow_mut().stop();
				error!("[PAInterface] Connection failed or context terminated");
				return Err(Report::msg("Connection failed or context terminated"));
			}
			_ => {
				mainloop.borrow_mut().wait();
			}
		}
	}
	debug!("[PAInterface] PAContext ready");

	context.borrow_mut().set_state_callback(None);

	callbacks::subscribe(&context, info_sx.clone())?;
	callbacks::request_current_state(context.clone(), info_sx.clone())?;

	mainloop.borrow_mut().unlock();

	debug!("[PAInterface] Actually starting our mainloop");

	let mut monitors = Monitors::default();
	let mut last_targets = HashMap::new();

	while let Some(msg) = internal_rx.recv().await {
		mainloop.borrow_mut().lock();

		match context.borrow_mut().get_state() {
			libpulse_binding::context::State::Ready => {}
			_ => {
				mainloop.borrow_mut().unlock();
				return Err(Report::msg("Disconnected while working"))
			}
		}

		match msg {
			PAInternal::AskInfo(ident) => {
				callbacks::request_info(ident, &context, info_sx.clone());
			}
			PAInternal::Tick => {
				// remove failed monitors
				monitors.filter(&mainloop, &context, &last_targets);
			}
			PAInternal::Command(cmd) => {
				let cmd = cmd.deref();
				if pa_actions::handle_command(cmd.clone(), &context, &info_sx).is_none() {
					monitors.filter(&mainloop, &context, &HashMap::new());
					mainloop.borrow_mut().unlock();
					break;
				}

				if let PulseAudioAction::CreateMonitors(mons) = cmd.clone() {
					last_targets = mons;
					monitors.filter(&mainloop, &context, &last_targets);
				}
			}
		};
		mainloop.borrow_mut().unlock();
	}

	Ok(())
}
