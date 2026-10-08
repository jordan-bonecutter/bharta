use crate::{launcher, media, network, status, volume, workspace_preview};
use std::{sync::mpsc, time::Duration};

pub enum Job {
    Scan(bool),
    Radio(bool),
    Connect(network::Network, String),
    Disconnect(String),
}
pub enum Event {
    Network(u64, Result<network::Snapshot, String>),
    Apps(u64, Vec<launcher::Entry>),
    Launched(u64, Result<(), String>),
}
pub struct Services {
    pub statuses: mpsc::Receiver<status::Status>,
    pub popup_events: mpsc::Receiver<u128>,
    pub status: mpsc::Sender<status::Update>,
    pub media: mpsc::Sender<media::Request>,
    pub media_events: mpsc::Receiver<media::Update>,
    pub volume: mpsc::Sender<volume::Request>,
    pub volume_events: mpsc::Receiver<volume::Update>,
    pub preview: mpsc::Sender<Option<String>>,
    pub preview_events: mpsc::Receiver<workspace_preview::Snapshot>,
    pub events: mpsc::Receiver<Event>,
    events_tx: mpsc::Sender<Event>,
}
impl Services {
    pub fn new(output: Option<String>) -> Self {
        let (status_tx, statuses) = mpsc::channel();
        let (status, requests) = mpsc::channel();
        status::watch(status.clone());
        let (popup_tx, popup_events) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                if status_tx
                    .send(status::Status::read(output.as_deref()))
                    .is_err()
                {
                    break;
                }
                match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(status::Update::Focus(name)) => {
                        if let Err(e) = status::focus(&name) {
                            eprintln!("{e}");
                        }
                    }
                    Ok(status::Update::AnnouncePopup(claim)) => {
                        let _ = status::ipc(10, &format!("bharta-popup:{claim}"));
                    }
                    Ok(status::Update::PopupOpened(claim)) => {
                        let _ = popup_tx.send(claim);
                    }
                    Ok(status::Update::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        let (tx, media_events) = mpsc::channel();
        let media = media::watch(tx);
        let (tx, volume_events) = mpsc::channel();
        let volume = volume::watch(tx);
        let (tx, preview_events) = mpsc::channel();
        let preview = workspace_preview::watch(tx);
        let (events_tx, events) = mpsc::channel();
        Self {
            statuses,
            popup_events,
            status,
            media,
            media_events,
            volume,
            volume_events,
            preview,
            preview_events,
            events,
            events_tx,
        }
    }
    pub fn network(&self, id: u64, job: Job) {
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let result = match job {
                Job::Scan(rescan) => network::scan(rescan),
                Job::Radio(on) => network::radio(on),
                Job::Connect(n, password) => network::connect(&n, password),
                Job::Disconnect(device) => network::disconnect(&device),
            }
            .map_err(|e| e.to_string());
            let _ = tx.send(Event::Network(id, result));
        });
    }
    pub fn apps(&self, id: u64) {
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Event::Apps(id, launcher::entries()));
        });
    }
    pub fn launch(&self, id: u64, path: std::path::PathBuf) {
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Event::Launched(
                id,
                launcher::launch(&path).map_err(|e| e.to_string()),
            ));
        });
    }
}
