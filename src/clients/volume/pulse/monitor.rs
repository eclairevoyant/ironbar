use std::convert::TryInto;

use libpulse_binding::stream::PeekResult;
use tracing::{debug, error, info, warn};

use super::{common::*, /*pa_interface::ACTIONS_SX*/};
// use crate::VARIABLES;
use color_eyre::{Report, Result};

pub struct Monitor {
    stream: Rc<RefCell<Stream>>,
    exit_sender: mpsc::UnboundedSender<u32>,
}

pub struct Monitors {
    monitors: HashMap<EntryIdentifier, Monitor>,
    errors: HashMap<EntryIdentifier, usize>,
}

impl Default for Monitors {
    fn default() -> Self {
        Self {
            monitors: HashMap::new(),
            errors: HashMap::new(),
        }
    }
}

impl Monitors {
    pub fn filter(
        &mut self,
        mainloop: &Rc<RefCell<Mainloop>>,
        context: &Rc<RefCell<PAContext>>,
        targets: &HashMap<EntryIdentifier, Option<u32>>,
    ) {
        // remove failed streams
        // then send exit signal if stream is unwanted
        self.monitors.retain(|ident, monitor| {
            match monitor.stream.borrow_mut().get_state() {
                libpulse_binding::stream::State::Terminated
                | libpulse_binding::stream::State::Failed => {
                    info!(
                        "[PAInterface] Disconnecting {} sink input monitor (failed state)",
                        ident.index
                    );
                    return false;
                }
                _ => {}
            };

            if targets.get(ident) == None {
                let _ = monitor.exit_sender.send(0);
            }

            true
        });

        targets.iter().for_each(|(ident, monitor_src)| {
            if self.monitors.get(ident).is_none() {
                self.create_monitor(mainloop, context, *ident, *monitor_src);
            }
        });
    }

    fn create_monitor(
        &mut self,
        mainloop: &Rc<RefCell<Mainloop>>,
        context: &Rc<RefCell<PAContext>>,
        ident: EntryIdentifier,
        monitor_src: Option<u32>,
    ) {
        if let Some(count) = self.errors.get(&ident) {
            if *count >= 5 {
                self.errors.remove(&ident);
                // (*ACTIONS_SX)
                //     .get()
                //     .send(EntryUpdate::EntryRemoved(ident))
                //     .unwrap();
            }
        }
        if self.monitors.contains_key(&ident) {
            return;
        }
        let (sx, rx) = mpsc::unbounded_channel();
        if let Ok(stream) = create(
            &mainloop,
            &context,
            &libpulse_binding::sample::Spec {
                format: libpulse_binding::sample::Format::FLOAT32NE,
                channels: 1,
                rate: /*(*VARIABLES).get().pa_rate*/ 20,
            },
            ident,
            monitor_src,
            rx,
        ) {
            self.monitors.insert(
                ident,
                Monitor {
                    stream,
                    exit_sender: sx,
                },
            );
            self.errors.remove(&ident);
        } else {
            self.error(&ident);
        }
    }

    fn error(&mut self, ident: &EntryIdentifier) {
        let count = match self.errors.get(&ident) {
            Some(x) => *x,
            None => 0,
        };
        self.errors.insert(*ident, count + 1);
    }
}

fn slice_to_4_bytes(slice: &[u8]) -> [u8; 4] {
    slice.try_into().expect("slice with incorrect length")
}

fn create(
    p_mainloop: &Rc<RefCell<Mainloop>>,
    p_context: &Rc<RefCell<PAContext>>,
    p_spec: &libpulse_binding::sample::Spec,
    ident: EntryIdentifier,
    source_index: Option<u32>,
    mut close_rx: mpsc::UnboundedReceiver<u32>,
) -> Result<Rc<RefCell<Stream>>> {
    info!("[PADataInterface] Attempting to create new monitor stream");

    let stream_index = if ident.entry_type == EntryType::SinkInput {
        Some(ident.index)
    } else {
        None
    };

    let stream = Rc::new(RefCell::new(
        match Stream::new(&mut p_context.borrow_mut(), "RsMixer monitor", p_spec, None) {
            Some(stream) => stream,
            None => return Err(Report::msg("Error creating stream for monitoring volume")),
        },
    ));

    // Stream state change callback
    {
        debug!("[PADataInterface] Registering stream state change callback");
        let ml_ref = Rc::clone(&p_mainloop);
        let stream_ref = Rc::downgrade(&stream);
        stream
            .borrow_mut()
            .set_state_callback(Some(Box::new(move || {
                let state = unsafe { (*(*stream_ref.as_ptr()).as_ptr()).get_state() };
                match state {
                    libpulse_binding::stream::State::Ready
                    | libpulse_binding::stream::State::Failed
                    | libpulse_binding::stream::State::Terminated => {
                        unsafe { (*ml_ref.as_ptr()).signal(false) };
                    }
                    _ => {}
                }
            })));
    }

    // for sink inputs we want to set monitor stream to sink
    if let Some(index) = stream_index {
        stream.borrow_mut().set_monitor_stream(index).unwrap();
    }

    let x;
    let mut s = None;
    if let Some(i) = source_index {
        x = i.to_string();
        s = Some(x.as_str());
    }

    debug!("[PADataInterface] Connecting stream");
    match stream.borrow_mut().connect_record(
        s,
        Some(&libpulse_binding::def::BufferAttr {
            maxlength: std::u32::MAX,
            tlength: std::u32::MAX,
            prebuf: std::u32::MAX,
            minreq: 0,
            fragsize: /*(*VARIABLES).get().pa_frag_size*/ 48,
        }),
        libpulse_binding::stream::FlagSet::PEAK_DETECT
            | libpulse_binding::stream::FlagSet::ADJUST_LATENCY,
    ) {
        Ok(_) => {}
        Err(err) => {
            return Err(Report::new(err).wrap_err("while connecting stream for monitoring volume"));
        }
    };

    debug!("[PADataInterface] Waiting for stream to be ready");
    loop {
        match stream.borrow_mut().get_state() {
            libpulse_binding::stream::State::Ready => {
                break;
            }
            libpulse_binding::stream::State::Failed
            | libpulse_binding::stream::State::Terminated => {
                error!("[PADataInterface] Stream state failed/terminated");
                return Err(Report::msg("Stream terminated"))
            }
            _ => {
                p_mainloop.borrow_mut().wait();
            }
        }
    }

    stream.borrow_mut().set_state_callback(None);

    {
        info!("[PADataInterface] Registering stream read callback");
        let ml_ref = Rc::clone(&p_mainloop);
        let stream_ref = Rc::downgrade(&stream);
        stream.borrow_mut().set_read_callback(Some(Box::new(move |_size: usize| {
            let remove_failed = || {
                error!("[PADataInterface] Monitor failed or terminated");
            };
            let disconnect_stream = || {
                warn!("[PADataInterface] {:?} Monitor existed while the sink (input)/source (output) was already gone", ident);
                unsafe {
                    (*(*stream_ref.as_ptr()).as_ptr()).disconnect().unwrap();
                    (*ml_ref.as_ptr()).signal(false);
                };
            };

            if close_rx.try_recv().is_ok() {
                disconnect_stream();
                return;
            }

            match unsafe {(*(*stream_ref.as_ptr()).as_ptr()).get_state() }{
                libpulse_binding::stream::State::Failed => {
                    remove_failed();
                },
                libpulse_binding::stream::State::Terminated => {
                    remove_failed();
                },
                libpulse_binding::stream::State::Ready => {
                    match unsafe{ (*(*stream_ref.as_ptr()).as_ptr()).peek() } {
                        Ok(res) => match res {
                            PeekResult::Data(data) => {
                                let count = data.len() / 4;
                                let mut peak = 0.0;
                                for c in 0..count {
                                    let data_slice = slice_to_4_bytes(&data[c * 4 .. (c + 1) * 4]);
                                    peak += f32::from_ne_bytes(data_slice).abs();
                                }
                                peak = peak / count as f32;

                                // if (*ACTIONS_SX).get().send(EntryUpdate::PeakVolumeUpdate(ident, peak)).is_err() {
                                //     disconnect_stream();
                                // }

                                unsafe { (*(*stream_ref.as_ptr()).as_ptr()).discard().unwrap(); };
                            },
                            PeekResult::Hole(_) => {
                                unsafe { (*(*stream_ref.as_ptr()).as_ptr()).discard().unwrap(); };
                            },
                            _ => {},
                        },
                        Err(_) => {
                            remove_failed();
                        },
                    }
                },
                _ => {},
            };
        })));
    }

    Ok(stream)
}
