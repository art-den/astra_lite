use std::sync::{Arc, Mutex};

use super::{phd2_conn, external_guider::*};

struct Data {
    evt_handlers:   Vec<ExtGuiderEventFn>,
    phd2_evt_hndlr: Option<phd2_conn::EventHandlerId>,
}

pub struct ExternalGuiderPhd2 {
    phd2: Arc<phd2_conn::Connection>,
    data: Arc<Mutex<Data>>,
}

impl ExternalGuiderPhd2 {
    pub fn new(phd2: &Arc<phd2_conn::Connection>) -> Arc<Self> {
        let data = Data {
            evt_handlers: Vec::new(),
            phd2_evt_hndlr: None,
        };
        let result = Arc::new(Self {
            phd2: Arc::clone(phd2),
            data: Arc::new(Mutex::new(data)),
        });
        result.connect_events();
        result
    }

    fn connect_events(self: &Arc<Self>) {
        let mut data = self.data.lock().unwrap();
        let self_ = Arc::clone(&self);
        data.phd2_evt_hndlr = Some(self.phd2.connect_event_handler(move |event| {
            let evt = match event {
                phd2_conn::Event::Object(obj) => {
                    match *obj {
                        phd2_conn::IncomingObject::Resumed { .. } =>
                            ExtGuiderEvent::GuidingContinued,
                        phd2_conn::IncomingObject::Paused { .. } =>
                            ExtGuiderEvent::GuidingPaused,
                        phd2_conn::IncomingObject::SettleDone { .. } =>
                            ExtGuiderEvent::DitheringFinished,
                        _ =>
                            return,
                    }
                }
                phd2_conn::Event::RpcResult(result) => {
                    match &*result {
                        phd2_conn::RpcResult::Error { error, .. } =>
                            ExtGuiderEvent::Error(error.message.clone()),
                        _ => return,
                    }
                }
                _ => return,
            };

            let data = self_.data.lock().unwrap();
            for hndlr in &data.evt_handlers {
                hndlr(evt.clone());
            }
        }));
    }
}

impl Drop for ExternalGuiderPhd2 {
    fn drop(&mut self) {
        let mut data = self.data.lock().unwrap();
        if let Some(phd2_evt_hndlr) = data.phd2_evt_hndlr.take() {
            self.phd2.diconnect_event_handler(&phd2_evt_hndlr);
        }
    }
}

impl ExternalGuider for ExternalGuiderPhd2 {
    fn get_type(&self) -> ExtGuiderType {
        ExtGuiderType::Phd2
    }

    fn state(&self) -> ExtGuiderState {
        ExtGuiderState::Stopped
    }

    fn connect(&self) -> anyhow::Result<()> {
        self.phd2.work("127.0.0.1", 4400)?;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.phd2.is_connected()
    }

    fn pause_guiding(&self, _pause: bool) -> anyhow::Result<()> {
        self.phd2.command_pause(true, true)?;
        Ok(())
    }

    fn start_dithering(&self, pixels: i32) -> anyhow::Result<()> {
        let settle = phd2_conn::Settle::default();
        self.phd2.command_dither(pixels as f64, false, &settle)?; // TODO: take settle from options
        Ok(())
    }

    fn disconnect(&self) -> anyhow::Result<()> {
        self.phd2.stop()?;
        Ok(())
    }

    fn connect_events_handler(&self, handler: ExtGuiderEventFn) {
        let mut data = self.data.lock().unwrap();
        data.evt_handlers.push(handler);
    }
}