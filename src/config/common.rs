use glib::signal::Inhibit;
use crate::dynamic_string::DynamicString;
use crate::script::{Script, ScriptInput};
use crate::send;
use gtk::gdk::ScrollDirection;
use gtk::prelude::*;
use gtk::{GestureClick, Orientation, Revealer, RevealerTransitionType, Widget};
use serde::Deserialize;
use tokio::spawn;
use tracing::trace;

/// Common configuration options
/// which can be set on every module.
#[derive(Debug, Deserialize, Clone)]
pub struct CommonConfig {
    pub show_if: Option<ScriptInput>,
    pub transition_type: Option<TransitionType>,
    pub transition_duration: Option<u32>,

    pub on_click_left: Option<ScriptInput>,
    pub on_click_right: Option<ScriptInput>,
    pub on_click_middle: Option<ScriptInput>,
    pub on_scroll_up: Option<ScriptInput>,
    pub on_scroll_down: Option<ScriptInput>,
    pub on_mouse_enter: Option<ScriptInput>,
    pub on_mouse_exit: Option<ScriptInput>,

    pub tooltip: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum TransitionType {
    None,
    Crossfade,
    SlideStart,
    SlideEnd,
}

impl TransitionType {
    pub fn to_revealer_transition_type(&self, orientation: Orientation) -> RevealerTransitionType {
        match (self, orientation) {
            (TransitionType::SlideStart, Orientation::Horizontal) => {
                RevealerTransitionType::SlideLeft
            }
            (TransitionType::SlideStart, Orientation::Vertical) => RevealerTransitionType::SlideUp,
            (TransitionType::SlideEnd, Orientation::Horizontal) => {
                RevealerTransitionType::SlideRight
            }
            (TransitionType::SlideEnd, Orientation::Vertical) => RevealerTransitionType::SlideDown,
            (TransitionType::Crossfade, _) => RevealerTransitionType::Crossfade,
            _ => RevealerTransitionType::None,
        }
    }
}

impl CommonConfig {
    /// Configures the module's container according to the common config options.
    pub fn install<W: IsA<Widget>>(mut self, widget: &W, revealer: &Revealer) {
        self.install_show_if(widget, revealer);

        let left_click_script = self.on_click_left.map(Script::new_polling);
        let middle_click_script = self.on_click_middle.map(Script::new_polling);
        let right_click_script = self.on_click_right.map(Script::new_polling);

        let gesture = GestureClick::new();

        gesture.connect_pressed(move |_, event| {
            let script = match event.button() {
                1 => left_click_script.as_ref(),
                2 => middle_click_script.as_ref(),
                3 => right_click_script.as_ref(),
                _ => None,
            };

            if let Some(script) = script {
                trace!("Running on-click script: {}", event.button());
                script.run_as_oneshot(None);
            }
        });

        let scroll_up_script = self.on_scroll_up.map(Script::new_polling);
        let scroll_down_script = self.on_scroll_down.map(Script::new_polling);

        widget.connect_scroll_event(move |_, event| {
            let script = match event.direction() {
                ScrollDirection::Up => scroll_up_script.as_ref(),
                ScrollDirection::Down => scroll_down_script.as_ref(),
                _ => None,
            };

            if let Some(script) = script {
                trace!("Running on-scroll script: {}", event.direction());
                script.run_as_oneshot(None);
            }

            Inhibit(false)
        });

        macro_rules! install_oneshot {
            ($option:expr, $method:ident) => {
                $option.map(Script::new_polling).map(|script| {
                    widget.$method(move |_, _| {
                        script.run_as_oneshot(None);
                        Inhibit(false)
                    });
                })
            };
        }

        install_oneshot!(self.on_mouse_enter, connect_enter_notify_event);
        install_oneshot!(self.on_mouse_exit, connect_leave_notify_event);

        if let Some(tooltip) = self.tooltip {
            let container = widget.clone();
            DynamicString::new(&tooltip, move |string| {
                container.set_tooltip_text(Some(&string));
                Continue(true)
            });
        }
    }

    fn install_show_if<W: IsA<Widget>>(&mut self, widget: &W, revealer: &Revealer) {
        self.show_if.take().map_or_else(
            || {
                widget.set_visible(true)
            },
            |show_if| {
                let script = Script::new_polling(show_if);
                let widget = widget.clone();
                let (tx, rx) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);

                spawn(async move {
                    script
                        .run(None, |_, success| {
                            send!(tx, success);
                        })
                        .await;
                });

                {
                    let revealer = revealer.clone();
                    let container = container.clone();

                    rx.attach(None, move |success| {
                        if success {
                            container.show_all();
                        }
                        revealer.set_reveal_child(success);
                        Continue(true)
                    });
                }

                revealer.connect_child_revealed_notify(move |revealer| {
                    if !revealer.reveals_child() {
                        container.hide()
                    }
                });
            },
        );
    }
}
