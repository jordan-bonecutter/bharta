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
struct Player(Arc<AtomicBool>, String, String);
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
        HashMap::from([
            (
                "xesam:title".into(),
                zbus::zvariant::Str::from(self.1.as_str()).into(),
            ),
            (
                "mpris:artUrl".into(),
                zbus::zvariant::Str::from(self.2.as_str()).into(),
            ),
        ])
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
    let mut connections = vec![];
    for (name, title, art) in [
        (
            "firefox",
            "A deliberately long video summary that should stay on one line",
            "BHARTA_TEST_FIREFOX_ART",
        ),
        ("spotify", "Test song", "BHARTA_TEST_SPOTIFY_ART"),
    ] {
        connections.push(
            zbus::blocking::connection::Builder::session()?
                .name(format!("org.mpris.MediaPlayer2.{name}"))?
                .serve_at(
                    "/org/mpris/MediaPlayer2",
                    Player(
                        playing.clone(),
                        title.into(),
                        std::env::var(art).unwrap_or_default(),
                    ),
                )?
                .build()?,
        );
    }
    for line in io::stdin().lock().lines() {
        playing.store(line? == "play", Ordering::SeqCst);
        for connection in &connections {
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
    }
    Ok(())
}
