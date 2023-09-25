use crate::{arc_rw, read_lock, send_async, write_lock};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::{Arc, RwLock};
// use std::thread::sleep;
use std::time::Duration;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;
use tokio::time::sleep;
use tracing::trace;

pub trait Cacheable: Debug + Clone + Send + Sync {
    type Key: Debug + Clone + Send + Sync + Eq;

    fn get_key(&self) -> Self::Key;
}

pub type Sender<T> = mpsc::Sender<Event<T>>;
pub type Receiver<T> = mpsc::Receiver<Event<T>>;

pub struct CachedBroadcastChannel<T>
where
    T: Cacheable,
{
    capacity: usize,
    data: Vec<T>,
    channels: Arc<RwLock<Vec<mpsc::Sender<Event<T>>>>>,
    base_tx: mpsc::Sender<Event<T>>,
}

#[derive(Debug, Clone)]
pub enum Event<T>
where
    T: Cacheable,
{
    Add(T),
    Remove(T::Key),
    Replace(T::Key, T),
}

impl<T> CachedBroadcastChannel<T>
where
    T: Cacheable + 'static,
{
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel::<Event<T>>(capacity);
        let mut rx = DropDetector(rx);

        // spawn_blocking(move || loop {
        //     let ev = rx.0.try_recv();
        //     println!("{ev:?}");
        //     sleep(Duration::from_secs(1))
        // });

        let channels = arc_rw!(Vec::<Sender<T>>::new());

        let channels = Arc::clone(&channels);
        spawn(async move {
            println!("hello");

            while let Some(event) = rx.0.recv().await {
                println!("ev");
                // trace!("{event:?}");
                // let iter = read_lock!(channels).clone().into_iter();
                // for channel in iter {
                //     send_async!(channel, event.clone());
                // }
            }
            println!("goodbye");
        });

        Self {
            capacity,
            data: vec![],
            channels,
            base_tx: tx,
        }
    }

    pub async fn send(&mut self, event: Event<T>) {
        match event.clone() {
            Event::Add(data) => {
                self.data.push(data);
            }
            Event::Remove(key) => {
                let Some(index) = self.data.iter().position(|t| t.get_key() == key) else {
                    return;
                };
                self.data.remove(index);
            }
            Event::Replace(key, data) => {
                let Some(index) = self.data.iter().position(|t| t.get_key() == key) else {
                    return;
                };
                let _ = std::mem::replace(&mut self.data[index], data);
            }
        }

        send_async!(self.base_tx, event);

        // let mut closed = vec![];
        // for (i, channel) in read_lock!(self.channels).iter().enumerate() {
        //     if channel.is_closed() {
        //         closed.push(i);
        //     } else {
        //         send_async!(channel, event.clone());
        //     }
        // }
        //
        // for channel in closed.into_iter().rev() {
        //     write_lock!(self.channels).remove(channel);
        // }
    }

    pub fn sender(&self) -> mpsc::Sender<Event<T>> {
        self.base_tx.clone()
    }

    pub fn receiver(&mut self) -> mpsc::Receiver<Event<T>> {
        let (tx, rx) = mpsc::channel(self.capacity);
        write_lock!(self.channels).push(tx);

        rx
    }

    pub fn data(&self) -> &Vec<T> {
        &self.data
    }
}

#[derive(Debug)]
struct DropDetector<T>(T);

impl<T> Drop for DropDetector<T> {
    fn drop(&mut self) {
        println!("DROPPED")
    }
}

impl<T: Cacheable> Drop for CachedBroadcastChannel<T> {
    fn drop(&mut self) {
        println!("Channel DROPPED")
    }
}
