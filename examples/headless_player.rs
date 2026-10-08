//! MPRIS fixture for tests/headless_ui.py; never use against a real session.
use std::{
    collections::HashMap,
    io::{self, BufRead},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use zbus::zvariant::OwnedValue;
struct Player(Arc<AtomicBool>);
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(property)]
    fn playback_status(&self) -> &str {
        if self.0.load(Ordering::SeqCst) {
            "Playing"
        } else {
            "Paused"
        }
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        HashMap::from([(
            "xesam:title".into(),
            zbus::zvariant::Str::from("Test song").into(),
        )])
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var("BHARTA_HEADLESS").as_deref() == Ok("1"),
        "Only run from the headless harness"
    );
    let playing = Arc::new(AtomicBool::new(true));
    let connection = zbus::blocking::connection::Builder::session()?
        .name("org.mpris.MediaPlayer2.bharta_test")?
        .serve_at("/org/mpris/MediaPlayer2", Player(playing.clone()))?
        .build()?;
    for line in io::stdin().lock().lines() {
        playing.store(line? == "play", Ordering::SeqCst);
        connection.emit_signal(
            None::<&str>,
            "/org/mpris/MediaPlayer2",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                "org.mpris.MediaPlayer2.Player",
                HashMap::<String, OwnedValue>::new(),
                vec!["PlaybackStatus"],
            ),
        )?;
    }
    Ok(())
}
