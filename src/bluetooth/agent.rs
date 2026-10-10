//! Per-operation agent: only the device selected in the panel may authenticate.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use zbus::zvariant::OwnedObjectPath;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prompt {
    Confirm(String),
    Pin,
    Passkey,
    Display(String),
}
#[derive(Default)]
pub struct State {
    pub prompt: Option<Prompt>,
    reply: Option<async_channel::Sender<Option<String>>>,
}
pub type Shared = Arc<Mutex<State>>;
impl State {
    pub fn answer(&mut self, answer: Option<String>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(answer);
        }
        self.prompt = None;
    }
}
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum Error {
    Rejected(String),
    Canceled(String),
}
pub struct Agent {
    pub device: String,
    pub state: Shared,
    pub canceled: Arc<AtomicBool>,
}
impl Agent {
    fn selected(&self, device: &OwnedObjectPath) -> Result<(), Error> {
        if self.canceled.load(Ordering::SeqCst) {
            return Err(Error::Canceled("Pairing canceled".into()));
        }
        if device.as_str() == self.device {
            Ok(())
        } else {
            Err(Error::Rejected(
                "Device was not selected for pairing".into(),
            ))
        }
    }
    async fn ask(&self, device: OwnedObjectPath, prompt: Prompt) -> Result<String, Error> {
        self.selected(&device)?;
        let (tx, rx) = async_channel::bounded(1);
        {
            let mut state = self.state.lock().unwrap();
            self.selected(&device)?;
            state.answer(None);
            state.prompt = Some(prompt);
            state.reply = Some(tx);
        }
        rx.recv()
            .await
            .ok()
            .flatten()
            .ok_or_else(|| Error::Rejected("Pairing canceled".into()))
    }
    fn display(&self, device: OwnedObjectPath, text: String) -> Result<(), Error> {
        self.selected(&device)?;
        self.state.lock().unwrap().prompt = Some(Prompt::Display(text));
        Ok(())
    }
}
#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {
        self.cancel();
    }
    fn cancel(&self) {
        self.state.lock().unwrap().answer(None);
    }
    async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, Error> {
        let pin = self.ask(device, Prompt::Pin).await?;
        if pin.is_empty() || pin.len() > 16 {
            return Err(Error::Rejected("PIN must contain 1–16 characters".into()));
        }
        Ok(pin)
    }
    async fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, Error> {
        let pin = self.ask(device, Prompt::Passkey).await?;
        pin.parse::<u32>()
            .ok()
            .filter(|n| *n <= 999999)
            .ok_or_else(|| Error::Rejected("Passkey must be a number from 000000 to 999999".into()))
    }
    fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) -> Result<(), Error> {
        self.display(
            device,
            format!("Type {pincode} on the device, then press Enter"),
        )
    }
    fn display_passkey(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        entered: u16,
    ) -> Result<(), Error> {
        self.display(
            device,
            format!("Type {passkey:06} on the device, then press Enter ({entered}/6)"),
        )
    }
    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> Result<(), Error> {
        self.ask(
            device,
            Prompt::Confirm(format!("Confirm matching code {passkey:06}")),
        )
        .await?;
        Ok(())
    }
    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), Error> {
        self.ask(device, Prompt::Confirm("Allow pairing?".into()))
            .await?;
        Ok(())
    }
    fn authorize_service(&self, device: OwnedObjectPath, _uuid: String) -> Result<(), Error> {
        self.selected(&device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_is_limited_to_selected_device_and_cancellation_revokes_it() {
        let canceled = Arc::new(AtomicBool::new(false));
        let agent = Agent {
            device: "/org/bluez/hci0/dev_selected".into(),
            state: Arc::new(Mutex::new(State::default())),
            canceled: canceled.clone(),
        };
        assert!(
            agent
                .selected(&agent.device.as_str().try_into().unwrap())
                .is_ok()
        );
        assert!(matches!(
            agent.selected(&"/org/bluez/hci0/dev_other".try_into().unwrap()),
            Err(Error::Rejected(_))
        ));
        canceled.store(true, Ordering::SeqCst);
        assert!(matches!(
            agent.selected(&agent.device.as_str().try_into().unwrap()),
            Err(Error::Canceled(_))
        ));
    }
    #[test]
    fn cancel_unblocks_an_outstanding_prompt_without_accepting_it() {
        let (tx, rx) = async_channel::bounded(1);
        let mut state = State {
            prompt: Some(Prompt::Confirm("123456".into())),
            reply: Some(tx),
        };
        state.answer(None);
        assert_eq!(rx.recv_blocking().unwrap(), None);
        assert!(state.prompt.is_none());
        assert!(state.reply.is_none());
    }
}
