#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use super::phd2;

pub enum ExtGuiderType {
    Phd2,
}

#[derive(Debug, Clone)]
pub enum ExtGuiderEvent {
    GuidingPaused,
    GuidingContinued,
    DitheringFinished,
    Error(String),
}

pub type ExtGuiderEventFn = Box<dyn Fn(ExtGuiderEvent) + Send + Sync + 'static>;

#[derive(Copy, Clone)]
pub enum ExtGuiderState {
    Guiding,
    Other,
}

pub trait ExternalGuider {
    fn get_type(&self) -> ExtGuiderType;
    fn state(&self) -> ExtGuiderState;
    fn connect(&self) -> anyhow::Result<()>;
    fn is_connected(&self) -> bool;
    fn start_guiding(&self) -> anyhow::Result<()>;
    fn pause_guiding(&self, pause: bool) -> anyhow::Result<()>;
    fn start_dithering(&self, pixels: i32) -> anyhow::Result<()>;
    fn disconnect(&self) -> anyhow::Result<()>;
    fn connect_events_handler(&self, handler: ExtGuiderEventFn);
}

pub struct ExternalGuiderCtrl {
    phd2:          Arc<phd2::Connection>,
    ext_guider:    Mutex<Option<Arc<dyn ExternalGuider + Send + Sync>>>,
    event_handler: Mutex<Option<ExtGuiderEventFn>>,
}

impl ExternalGuiderCtrl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            phd2:          Arc::new(phd2::Connection::new()),
            ext_guider:    Mutex::new(None),
            event_handler: Mutex::new(None),
        })
    }

    pub fn set_events_handler(&self, handler: ExtGuiderEventFn) {
        *self.event_handler.lock().unwrap() = Some(handler);
    }

    pub fn phd2_conn(&self) -> &Arc<phd2::Connection> {
        &self.phd2
    }

    pub fn create_and_connect(self: &Arc<Self>, guider: ExtGuiderType) -> anyhow::Result<()> {
        let mut ext_guider = self.ext_guider.lock().unwrap();

        // Disconect previous one

        if let Some(ext_guider) = &mut *ext_guider {
            ext_guider.disconnect()?;
        }

        // Create new guider

        let guider: Arc<dyn ExternalGuider + Send + Sync> = match guider {
            ExtGuiderType::Phd2 =>
                phd2::ExternalGuiderPhd2::new(&self.phd2),
        };

        // Connect to guider

        guider.connect()?;

        // Connect guider events

        let self_ = Arc::clone(self);
        guider.connect_events_handler(Box::new(move |event| {
            log::info!("External guider event = {:?}", event);
            let mut events_handler = self_.event_handler.lock().unwrap();
            if let Some(events_handler) = &mut *events_handler {
                events_handler(event);
            }
        }));

        // Assign guider

        *ext_guider = Some(guider);
        drop(ext_guider);

        Ok(())
    }

    pub fn disconnect(&self) -> anyhow::Result<()> {
        let mut ext_guider = self.ext_guider.lock().unwrap();
        if let Some(guider) = ext_guider.take() {
            guider.disconnect()?;
        } else {
            return Err(anyhow::anyhow!("Not connected"));
        }
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        let ext_guider = self.ext_guider.lock().unwrap();
        if let Some(ext_guider) = &*ext_guider {
            ext_guider.is_connected()
        } else {
            false
        }
    }

    pub fn start_dithering(&self, pixels: i32) -> anyhow::Result<()> {
        let ext_guider = self.ext_guider.lock().unwrap();
        let Some(ext_guider) = &*ext_guider else {
            anyhow::bail!("External guider is not created");
        };
        ext_guider.start_dithering(pixels)?;
        Ok(())
    }

    pub fn start_guiding(&self) -> anyhow::Result<()> {
        let ext_guider = self.ext_guider.lock().unwrap();
        let Some(ext_guider) = &*ext_guider else {
            anyhow::bail!("External guider is not created");
        };
        ext_guider.start_guiding()?;
        Ok(())
    }
}
