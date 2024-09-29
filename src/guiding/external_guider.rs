#![allow(dead_code)]

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

pub trait ExternalGuider {
    fn get_type(&self) -> ExtGuiderType;
    fn connect(&self) -> anyhow::Result<()>;
    fn is_active(&self) -> bool;
    fn pause_guiding(&self, pause: bool) -> anyhow::Result<()>;
    fn start_dithering(&self, pixels: i32) -> anyhow::Result<()>;
    fn disconnect(&self) -> anyhow::Result<()>;
    fn connect_event_handler(&self, handler: ExtGuiderEventFn);
}
