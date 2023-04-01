use crate::clients::volume::VolumeClient;
use libpulse_binding::context::State;
use libpulse_binding::{
    context::{Context, FlagSet},
    mainloop::threaded::Mainloop,
    proplist::{properties, Proplist},
};
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use tracing::{debug, error};

pub fn test() {
    let mut prop_list = Proplist::new().unwrap();
    prop_list
        .set_str(properties::APPLICATION_NAME, "ironbar")
        .unwrap();

    let mainloop = Rc::new(RefCell::new(Mainloop::new().unwrap()));

    let context = Rc::new(RefCell::new(
        Context::new_with_proplist(mainloop.borrow().deref(), "ironbar_context", &prop_list)
            .unwrap(),
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
                if matches!(state, State::Ready | State::Failed | State::Terminated) {
                    unsafe { (*ml_ref.as_ptr()).signal(false) };
                }
            })));
    }

    if let Err(err) = context.borrow_mut().connect(None, FlagSet::NOFLAGS, None) {
        error!("{err:?}");
    }

    println!("{:?}", context.borrow().get_server());

    mainloop.borrow_mut().lock();
    if let Err(err) = mainloop.borrow_mut().start() {
        error!("{err:?}");
    }

    debug!("[PAInterface] Waiting for context to be ready...");
    println!("[PAInterface] Waiting for context to be ready...");
    // wait for context to be ready
    loop {
        match context.borrow().get_state() {
            State::Ready => {
                break;
            }
            State::Failed | State::Terminated => {
                mainloop.borrow_mut().unlock();
                mainloop.borrow_mut().stop();
                error!("[PAInterface] Connection failed or context terminated");
            }
            _ => {
                mainloop.borrow_mut().wait();
            }
        }
    }
    debug!("[PAInterface] PAContext ready");
    println!("[PAInterface] PAContext ready");

    context.borrow_mut().set_state_callback(None);

    println!("jfgjfgg");

    let introspector = context.borrow().introspect();

    println!("jfgjfgg2");

    introspector.get_sink_info_list(|result| {
        println!("boo: {result:?}");
    });

    println!("fjgjfgf??");
}

struct PulseVolumeClient {}

impl VolumeClient for PulseVolumeClient {}
