use crate::core::core::*;

pub struct WaitingMode;

impl Mode for WaitingMode {
    fn get_type(&self) -> ModeType {
        ModeType::Waiting
    }

    fn progress_string(&self) -> String {
        "Waiting...".to_string()
    }

    fn can_be_stopped(&self) -> bool {
        false
    }
}