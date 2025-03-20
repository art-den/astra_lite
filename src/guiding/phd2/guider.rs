use std::sync::{Arc, Mutex};

use super::{connection::*, super::external_guider::*};

struct Data {
    evt_handlers:   Vec<ExtGuiderEventFn>,
    phd2_evt_hndlr: Option<EventHandlerId>,
    app_state:      AppState,
}

pub struct ExternalGuiderPhd2 {
    phd2: Arc<Connection>,
    data: Arc<Mutex<Data>>,
}

impl ExternalGuiderPhd2 {
    pub fn new(phd2: &Arc<Connection>) -> Arc<Self> {
        let data = Data {
            evt_handlers: Vec::new(),
            phd2_evt_hndlr: None,
            app_state: AppState::Stopped,
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
            // Check for new phd2 application state

            if let Event::Object(obj) = &event {
                let new_app_state = match &**obj {
                    IncomingObject::AppState { state, .. } =>
                        Some(state.clone()),
                    IncomingObject::GuideStep {..} =>
                        Some(AppState::Guiding),
                    IncomingObject::Paused {..} =>
                        Some(AppState::Paused),
                    IncomingObject::StartCalibration {..} =>
                        Some(AppState::Calibrating),
                    IncomingObject::LoopingExposures {..} =>
                        Some(AppState::Looping),
                    IncomingObject::LoopingExposuresStopped {..} =>
                        Some(AppState::Stopped),
                    _ => None,
                };
                if let Some(new_app_state) = new_app_state {
                    let mut data = self_.data.lock().unwrap();
                    data.app_state = new_app_state;
                    dbg!(&data.app_state);
                }
            }

            // Events

            let evt = match event {
                Event::Object(obj) => {
                    match *obj {
                        IncomingObject::Resumed { .. } =>
                            ExtGuiderEvent::GuidingContinued,
                        IncomingObject::Paused { .. } =>
                            ExtGuiderEvent::GuidingPaused,
                        IncomingObject::SettleDone { .. } =>
                            ExtGuiderEvent::DitheringFinished,
                        _ =>
                            return,
                    }
                }
                Event::RpcResult(result) => {
                    match &*result {
                        RpcResult::Error { error, .. } =>
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

    fn is_guiding(&self) -> bool {
        let data = self.data.lock().unwrap();
        data.app_state == AppState::Guiding
    }

    fn start_guiding(&self) -> anyhow::Result<()> {
        let settle = Settle::default(); // TODO: take settle from options
        self.phd2.method_guide(&settle, None)?;
        Ok(())
    }

    fn pause_guiding(&self, _pause: bool) -> anyhow::Result<()> {
        self.phd2.method_pause(true, true)?;
        Ok(())
    }

    fn start_dithering(&self, pixels: i32) -> anyhow::Result<()> {
        let settle = Settle::default(); // TODO: take settle from options
        self.phd2.method_dither(pixels as f64, false, &settle)?;
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