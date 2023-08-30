enum PhdEvent {}

pub struct Phd2Api {

}

impl Phd2Api {
    pub fn new() -> Self {
        Self {}
    }

    pub fn connect(_host: &str, _port: u16) -> anyhow::Result<()> {
        todo!()
    }

    pub fn disconnect(&self) -> anyhow::Result<()> {
        todo!()
    }

    pub fn continue_guiding(&self) -> anyhow::Result<()> {
        todo!()
    }

    pub fn pause_guiding(&self) -> anyhow::Result<()> {
        todo!()
    }
}