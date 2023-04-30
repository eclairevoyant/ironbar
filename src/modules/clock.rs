use crate::config::CommonConfig;
use crate::modules::{Module, ModuleInfo, ModuleUpdateEvent, ModuleWidget, WidgetContext};
use crate::popup::Popup;
use crate::{send_async, try_send};
use chrono::{DateTime, Local};
use color_eyre::Result;
use glib::Continue;
use gtk::prelude::*;
use gtk::{Align, Button, Calendar, Label, Orientation};
use serde::Deserialize;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio::time::sleep;

#[derive(Debug, Deserialize, Clone)]
pub struct ClockModule {
    /// Date/time format string.
    /// Default: `%d/%m/%Y %H:%M`
    ///
    /// Detail on available tokens can be found here:
    /// <https://docs.rs/chrono/latest/chrono/format/strftime/index.html>
    #[serde(default = "default_format")]
    format: String,

    #[serde(flatten)]
    pub common: Option<CommonConfig>,
}

fn default_format() -> String {
    String::from("%d/%m/%Y %H:%M")
}

impl Module<Button> for ClockModule {
    type SendMessage = DateTime<Local>;
    type ReceiveMessage = ();

    fn name() -> &'static str {
        "clock"
    }

    fn spawn_controller(
        &self,
        _info: &ModuleInfo,
        tx: mpsc::Sender<ModuleUpdateEvent<Self::SendMessage>>,
        _rx: mpsc::Receiver<Self::ReceiveMessage>,
    ) -> Result<()> {
        spawn(async move {
            loop {
                let date = Local::now();
                send_async!(tx, ModuleUpdateEvent::Update(date));
                sleep(tokio::time::Duration::from_millis(500)).await;
            }
        });

        Ok(())
    }

    fn into_widget(
        self,
        context: WidgetContext<Self::SendMessage, Self::ReceiveMessage>,
        info: &ModuleInfo,
    ) -> Result<ModuleWidget<Button>> {
        let button = Button::new();
        let label = Label::new(None);
        label.set_angle(info.bar_position.get_angle());
        button.append(&label);

        let orientation = info.bar_position.get_orientation();
        button.connect_clicked(move |button| {
            try_send!(
                context.tx,
                ModuleUpdateEvent::TogglePopup(Popup::widget_geometry(button, orientation))
            );
        });

        let format = self.format.clone();
        {
            context.widget_rx.attach(None, move |date| {
                let date_string = format!("{}", date.format(&format));
                label.set_label(&date_string);
                Continue(true)
            });
        }

        let popup = self.into_popup(context.controller_tx, context.popup_rx, info);

        Ok(ModuleWidget {
            widget: button,
            popup,
        })
    }

    fn into_popup(
        self,
        _tx: mpsc::Sender<Self::ReceiveMessage>,
        rx: glib::Receiver<Self::SendMessage>,
        _info: &ModuleInfo,
    ) -> Option<gtk::Box> {
        let container = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .name("popup-clock")
            .build();

        let clock = Label::builder()
            .name("calendar-clock")
            .halign(Align::Center)
            .build();
        let format = "%H:%M:%S";

        container.append(&clock);

        let calendar = Calendar::builder().name("calendar").build();
        container.append(&calendar);

        {
            rx.attach(None, move |date| {
                let date_string = format!("{}", date.format(format));
                clock.set_label(&date_string);
                Continue(true)
            });
        }

        Some(container)
    }
}
