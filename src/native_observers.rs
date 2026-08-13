use std::{sync::mpsc, thread, time::Duration};

use libpulse_binding::{
    context::{Context, FlagSet, subscribe::InterestMaskSet},
    mainloop::standard::{IterateResult, Mainloop},
    proplist::Proplist,
};
use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator},
    message::Type,
};

pub fn observe_volume() -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::sync_channel(16);
    thread::Builder::new()
        .name("raylander-pulse".into())
        .spawn(move || {
            loop {
                if let Err(error) = run_pulse(&sender) {
                    tracing::warn!(%error, "native PulseAudio observer disconnected");
                    thread::sleep(Duration::from_secs(2));
                }
            }
        })
        .expect("failed to start PulseAudio observer");
    receiver
}

fn run_pulse(sender: &mpsc::SyncSender<()>) -> Result<(), String> {
    let sender = sender.clone();
    let mut proplist = Proplist::new().ok_or("failed to create PulseAudio properties")?;
    proplist
        .set_str(
            libpulse_binding::proplist::properties::APPLICATION_NAME,
            "Raylander",
        )
        .map_err(|_| "failed to set PulseAudio application name")?;
    let mut mainloop = Mainloop::new().ok_or("failed to create PulseAudio mainloop")?;
    let mut context = Context::new_with_proplist(&mainloop, "Raylander", &proplist)
        .ok_or("failed to create PulseAudio context")?;
    context
        .connect(None, FlagSet::NOFLAGS, None)
        .map_err(|error| {
            error
                .to_string()
                .unwrap_or_else(|| "unknown PulseAudio error".into())
        })?;
    loop {
        match mainloop.iterate(true) {
            IterateResult::Quit(_) | IterateResult::Err(_) => {
                return Err("PulseAudio connection failed".into());
            }
            IterateResult::Success(_) => {}
        }
        use libpulse_binding::context::State;
        match context.get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => {
                return Err("PulseAudio context unavailable".into());
            }
            _ => {}
        }
    }
    context.set_subscribe_callback(Some(Box::new(move |facility, operation, _| {
        use libpulse_binding::context::subscribe::{Facility, Operation};
        if matches!(facility, Some(Facility::Sink | Facility::Server))
            && operation != Some(Operation::Removed)
        {
            let _ = sender.try_send(());
        }
    })));
    context.subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, |_| {});
    loop {
        match mainloop.iterate(true) {
            IterateResult::Success(_) => {}
            _ => return Err("PulseAudio observer stopped".into()),
        }
    }
}

pub fn observe_media() -> mpsc::Receiver<()> {
    let (sender, receiver) = mpsc::sync_channel(16);
    thread::Builder::new()
        .name("raylander-mpris".into())
        .spawn(move || {
            loop {
                if let Err(error) = run_mpris(&sender) {
                    tracing::warn!(%error, "native MPRIS observer disconnected");
                    thread::sleep(Duration::from_secs(2));
                }
            }
        })
        .expect("failed to start MPRIS observer");
    receiver
}

fn run_mpris(sender: &mpsc::SyncSender<()>) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .path("/org/mpris/MediaPlayer2")?
        .build();
    let mut messages = MessageIterator::for_match_rule(rule, &connection, Some(32))?;
    for message in &mut messages {
        message?;
        let _ = sender.try_send(());
    }
    Ok(())
}
