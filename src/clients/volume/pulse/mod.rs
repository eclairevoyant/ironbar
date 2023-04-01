mod callbacks;
pub mod common;
mod errors;
mod monitor;
mod pa_actions;
mod pa_interface;

use common::*;
use lazy_static::lazy_static;
pub use pa_interface::start;

#[derive(Debug)]
pub enum PAInternal {
	Tick,
	Command(Box<PulseAudioAction>),
	AskInfo(EntryIdentifier),
}

lazy_static! {
	pub static ref SPEC: libpulse_binding::sample::Spec = libpulse_binding::sample::Spec {
		format: libpulse_binding::sample::Format::FLOAT32NE,
		channels: 1,
		rate: 1024,
	};
}

#[derive(PartialEq, Clone, Debug)]
pub struct CardProfile {
	pub name: String,
	pub description: String,
	#[cfg(any(feature = "pa_v13"))]
	pub available: bool,
	// pub area: Rect,
	pub is_selected: bool,
}
impl Eq for CardProfile {}