use std::sync::{Arc, Mutex};

use super::{connection::*, super::external_guider::*};

struct Data {
    evt_handlers:   Vec<ExtGuiderEventFn>,
    phd2_evt_hndlr: Option<EventHandlerId>,
    app_state:      AppState,
    state:          ExtGuiderState,
}

pub struct ExternalGuiderPhd2 {
    phd2: Arc<Connection>,
    data: Arc<Mutex<Data>>,
}

impl ExternalGuiderPhd2 {
    pub fn new(phd2: &Arc<Connection>) -> Arc<Self> {
        let data = Data {
            evt_handlers:   Vec::new(),
            phd2_evt_hndlr: None,
            app_state:      AppState::Stopped,
            state:          ExtGuiderState::Stopped,
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
        let self_ = Arc::clone(self);
        data.phd2_evt_hndlr = Some(self.phd2.connect_event_handler(move |event| {
            // Check for new phd2 application state

            let mut evt = None;

            if let Event::Object(obj) = &event {
                let mut new_app_state = None;
                match &**obj {
                    IncomingObject::AppState { state, .. } =>
                        new_app_state = Some(*state),
                    IncomingObject::GuideStep {..} =>
                        new_app_state = Some(AppState::Guiding),
                    IncomingObject::Paused {..} =>
                        new_app_state = Some(AppState::Paused),
                    IncomingObject::StartCalibration {..} =>
                        new_app_state = Some(AppState::Calibrating),
                    IncomingObject::LoopingExposures {..} =>
                        new_app_state = Some(AppState::Looping),
                    IncomingObject::LoopingExposuresStopped {..} =>
                        new_app_state = Some(AppState::Stopped),
                    IncomingObject::SettleDone { status: 0, .. } =>
                        evt = Some(ExtGuiderEvent::DitheringFinished),
                    IncomingObject::SettleDone { error: Some(err_str), .. } =>
                        evt = Some(ExtGuiderEvent::DitheringFinishedWithErr(err_str.clone())),
                    _ => {},
                };
                if let Some(new_app_state) = new_app_state {
                    let mut data = self_.data.lock().unwrap();
                    if data.app_state != new_app_state {
                        data.state = match new_app_state {
                            AppState::Stopped     => ExtGuiderState::Stopped,
                            AppState::Selected    => ExtGuiderState::Other,
                            AppState::Calibrating => ExtGuiderState::Calibrating,
                            AppState::Guiding     => ExtGuiderState::Guiding,
                            AppState::LostLock    => ExtGuiderState::Other,
                            AppState::Paused      => ExtGuiderState::Paused,
                            AppState::Looping     => ExtGuiderState::Looping,
                        };
                        evt = Some(ExtGuiderEvent::State(data.state));

                    }
                    data.app_state = new_app_state;
                }
            }
            if let Event::RpcResult(result) = &event {
                if let RpcResult::Error { error, .. } = &**result {
                    // .. error event
                    evt = Some(ExtGuiderEvent::Error(error.message.clone()));
                }
            }
            match event {
                Event::Connected =>
                    evt = Some(ExtGuiderEvent::Connected),
                Event::Disconnected =>
                    evt = Some(ExtGuiderEvent::Disconnected),
                _ => {}
            }

            if let Some(evt) = evt {
                dbg!(&evt);
                let data = self_.data.lock().unwrap();
                for hndlr in &data.evt_handlers {
                    hndlr(evt.clone());
                }
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
        let data = self.data.lock().unwrap();
        data.state
    }

    fn connect(&self) -> anyhow::Result<()> {
        self.phd2.work("127.0.0.1", 4400)?;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.phd2.is_connected()
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