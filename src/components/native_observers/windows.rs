use std::{
    sync::mpsc::{self, TrySendError},
    thread,
    time::Duration,
};

pub fn observe_volume() -> mpsc::Receiver<()> {
    polling_observer("raylander-windows-volume", Duration::from_secs(1))
}

pub fn observe_media() -> mpsc::Receiver<()> {
    polling_observer("raylander-windows-media", Duration::from_secs(2))
}

fn polling_observer(name: &str, interval: Duration) -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            loop {
                thread::sleep(interval);
                match sender.try_send(()) {
                    Ok(()) | Err(TrySendError::Full(())) => {}
                    Err(TrySendError::Disconnected(())) => break,
                }
            }
        })
        .expect("failed to start Windows polling observer");
    receiver
}
