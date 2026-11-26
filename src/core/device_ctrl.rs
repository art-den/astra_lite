use std::sync::Arc;

use eyre::Context as _;

use crate::core::commands::{DeviceCommand, DeviceKind};
use crate::core::engine::Engine;

/// Target of a relative move, clamped to the device range
fn clamped_relative_target(abs_position: f64, offset: i32, range: &std::ops::RangeInclusive<f64>) -> f64 {
    f64::clamp(abs_position + offset as f64, *range.start(), *range.end())
}

impl Engine {
    /// Executes a device command on the `engine_cmd` thread.
    /// State-dedup guards suppress phantom commands (a widget programmatically
    /// set to the current value); `MountMoveStart`/`MountMoveStop` are never
    /// deduped: every press must reach the device.
    /// Device absence or HAL failure is reported via `Event::Error` (not `process_error`).
    pub(crate) fn exec_device_command(self: &Arc<Self>, cmd: DeviceCommand) {
        let result = self.exec_device_command_inner(cmd);
        if let Err(e) = result {
            Self::report_command_error(self, "Device command", e);
        }
    }

    fn exec_device_command_inner(self: &Arc<Self>, cmd: DeviceCommand) -> eyre::Result<()> {
        match cmd {
            DeviceCommand::MountTrack(enabled) => {
                let telescope = self.cur_devices.telescope_or_err().context("Mount")?;
                if telescope.is_tracking()? == enabled { return Ok(()); }
                telescope.track(enabled)?;
            }
            DeviceCommand::MountPark => {
                let telescope = self.cur_devices.telescope_or_err().context("Mount")?;
                if telescope.is_parked()? { return Ok(()); }
                telescope.park()?;
            }
            DeviceCommand::MountUnpark => {
                let telescope = self.cur_devices.telescope_or_err().context("Mount")?;
                if !telescope.is_parked()? { return Ok(()); }
                telescope.unpark()?;
            }
            DeviceCommand::MountMoveStart { dir } => {
                let telescope = self.cur_devices.telescope_or_err().context("Mount")?;
                let (inv_ns, inv_we, speed) = {
                    let options = self.options.read().unwrap();
                    (options.mount.inv_ns, options.mount.inv_we, options.mount.speed.clone())
                };
                telescope.revert_motion(inv_ns, inv_we)?;
                if let Some(speed) = speed {
                    telescope.set_slew_speed(&speed)?;
                }
                telescope.move_(dir)?;
            }
            DeviceCommand::MountMoveStop => {
                let telescope = self.cur_devices.telescope_or_err().context("Mount")?;
                telescope.abort_motion()?;
            }
            DeviceCommand::FocuserSetPosition { value } => {
                let focuser = self.cur_devices.focuser_or_err().context("Focuser")?;
                if focuser.abs_position()? == value { return Ok(()); }
                focuser.set_abs_position(value)?;
            }
            DeviceCommand::FocuserMoveRelative { offset } => {
                let focuser = self.cur_devices.focuser_or_err().context("Focuser")?;
                let abs_position = focuser.abs_position()?;
                let range = focuser.abs_position_range()?;
                let target = clamped_relative_target(abs_position, offset, &range);
                if target == abs_position { return Ok(()); }
                focuser.set_abs_position(target)?;
            }
            DeviceCommand::FilterWheelSelect { index } => {
                let filter_wheel = self.cur_devices.filter_wheel_or_err().context("Filter wheel")?;
                let (_, active) = filter_wheel.list_and_active()?;
                if active == index { return Ok(()); }
                filter_wheel.set_active(index)?;
            }
            DeviceCommand::SelectDevice { kind, device_id, force } => {
                match kind {
                    DeviceKind::Camera =>
                        if force { self.cur_devices.apply_camera(&device_id); }
                        else { self.cur_devices.change_camera(&device_id); },
                    DeviceKind::Mount =>
                        if force { self.cur_devices.apply_telescope(&device_id); }
                        else { self.cur_devices.change_telescope(&device_id); },
                    DeviceKind::Focuser =>
                        if force { self.cur_devices.apply_focuser(&device_id); }
                        else { self.cur_devices.change_focuser(&device_id); },
                    DeviceKind::FilterWheel =>
                        if force { self.cur_devices.apply_filter_wheel(&device_id); }
                        else { self.cur_devices.change_filter_wheel(&device_id); },
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamped_relative_target_moves_and_clamps() {
        let range = 0.0..=100.0;
        assert_eq!(clamped_relative_target(50.0, 10, &range), 60.0);
        assert_eq!(clamped_relative_target(50.0, -10, &range), 40.0);
        assert_eq!(clamped_relative_target(95.0, 10, &range), 100.0);
        assert_eq!(clamped_relative_target(5.0, -10, &range), 0.0);
        // At the boundary the target equals the current position (the executor no-ops)
        assert_eq!(clamped_relative_target(100.0, 10, &range), 100.0);
    }
}
