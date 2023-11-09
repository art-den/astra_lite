use std::sync::{Arc, Mutex};

use super::{phd2_conn, external_guider::*};

pub struct ExternalGuiderPhd2 {
    phd2:           Arc<phd2_conn::Connection>,
    evt_handlers:   Arc<Mutex<Vec<ExtGuiderEventFn>>>,
    phd2_evt_hndlr: phd2_conn::EventHandlerId,
}

impl ExternalGuiderPhd2 {
    pub fn new(phd2: &Arc<phd2_conn::Connection>) -> Self {
        let evt_handlers = Arc::new(Mutex::new(Vec::<ExtGuiderEventFn>::new()));
        let phd2_evt_hndlr = {
            let evt_handlers = Arc::clone(&evt_handlers);
            phd2.connect_event_handler(move |event| {
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
                let evt_handlers = evt_handlers.lock().unwrap();
                for hndlr in &*evt_handlers {
                    hndlr(evt.clone());
                }
            })
        };
        Self {
            phd2: Arc::clone(phd2),
            evt_handlers,
            phd2_evt_hndlr
        }
    }
}

impl Drop for ExternalGuiderPhd2 {
    fn drop(&mut self) {
        self.phd2.diconnect_event_handler(&self.phd2_evt_hndlr);
    }
}

impl ExternalGuider for ExternalGuiderPhd2 {
    fn get_type(&self) -> ExtGuiderType {
        ExtGuiderType::Phd2
    }

    fn connect(&self) -> anyhow::Result<()> {
        self.phd2.work("127.0.0.1", 4400)?;
        Ok(())
    }

    fn is_active(&self) -> bool {
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

    fn connect_event_handler(&self, handler: ExtGuiderEventFn) {
        let mut evt_handlers = self.evt_handlers.lock().unwrap();
        evt_handlers.push(handler);

    }
}