//! Members every ASCOM device shares, plus the capability cache.
//!
//! Two practical reasons for the cache: every `CanXxx` read is a cross-process RPC,
//! and drivers are chatty, so the control loop must not walk them. The snapshot is
//! taken once after connecting and invalidated whenever `Connected` changes.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::actor::Actor;
use crate::com::collections;
use crate::com::variant::{AscomEnum, Variant, VariantKind};
use crate::error::{AscomError, AscomErrorKind, Result};

/// Which driver to instantiate.
///
/// Devices are constructed from a spec (a ProgID from config or from
/// [`crate::chooser`]), never from `Default`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSpec {
    pub prog_id: String,
}

impl DeviceSpec {
    pub fn new(prog_id: impl Into<String>) -> Self {
        Self { prog_id: prog_id.into() }
    }

    /// Reads the ProgID from an environment variable, which is how the live-driver
    /// tests pick a simulator without hard-coding one.
    pub fn from_env(var: &str) -> Result<Self> {
        std::env::var(var)
            .map(Self::new)
            .map_err(|_| AscomError::local(
                AscomErrorKind::NotFound,
                "DeviceSpec::from_env",
                format!("{var} is not set"),
            ))
    }
}

/// One `Name`/`Value` pair from a `DeviceState` list.
#[derive(Debug, Clone, PartialEq)]
pub struct StateValue {
    pub name: String,
    pub value: VariantKind,
}

/// Capabilities read once per connection and cached.
///
/// `flags` holds every `CanXxx`-style member the device type declares:
/// `Some(true)`/`Some(false)` is a real answer, `None` means the driver does not
/// implement that member at all, which is legal and must not be treated as false.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapabilitySnapshot {
    pub interface_version: i16,
    pub name: String,
    pub description: String,
    pub driver_info: String,
    pub driver_version: String,
    pub supported_actions: Vec<String>,
    pub flags: BTreeMap<String, Option<bool>>,
}

impl CapabilitySnapshot {
    /// `Some(true)`/`Some(false)` as the driver answered; `None` if unimplemented.
    pub fn flag(&self, member: &str) -> Option<bool> {
        self.flags.get(member).copied().flatten()
    }

    /// Treats an unimplemented capability as `false`, which is what a control loop
    /// usually wants.
    pub fn supports(&self, member: &str) -> bool {
        self.flag(member).unwrap_or(false)
    }

    /// `DeviceState` exists from `InterfaceVersion` 4 on.
    pub fn has_device_state(&self) -> bool {
        self.interface_version >= 4
    }

    /// Case-insensitive lookup, because action names are case insensitive by spec.
    pub fn has_action(&self, action: &str) -> bool {
        self.supported_actions.iter().any(|a| a.eq_ignore_ascii_case(action))
    }
}

/// `GuideDirections`, shared by `Telescope.PulseGuide` and `Camera.PulseGuide`.
///
/// These are equatorial-frame directions, not mechanical axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i32)]
pub enum GuideDirection {
    North = 0,
    South = 1,
    East = 2,
    West = 3,
}

impl AscomEnum for GuideDirection {
    fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            0 => Some(Self::North),
            1 => Some(Self::South),
            2 => Some(Self::East),
            3 => Some(Self::West),
            _ => None,
        }
    }

    fn to_raw(self) -> i32 {
        self as i32
    }
}

/// Members shared by `ITelescopeV4`, `IFocuserV4` and `ICameraV4`.
///
/// Everything here can raise: ASCOM allows any member to throw, so callers must
/// handle errors rather than unwrap.
pub trait AscomDevice {
    /// The COM thread this device lives on.
    fn actor(&self) -> &Actor;

    /// `CanXxx`-style members to capture in [`AscomDevice::capabilities`].
    fn flag_members() -> &'static [&'static str];

    /// Identity of the driver behind this handle.
    fn spec(&self) -> DeviceSpec;

    /// Builds the snapshot on first use and returns the cached copy afterwards.
    ///
    /// Fails when a mandatory identity member cannot be read, so a driver that gates
    /// them on `Connected` has to be connected first.
    fn capabilities(&self) -> Result<CapabilitySnapshot> {
        self.actor().call(|device| {
            if let Some(cached) = device.capabilities() {
                return Ok(cached.clone());
            }
            let snapshot = read_snapshot(device.dispatch(), Self::flag_members())?;
            device.set_capabilities(snapshot.clone());
            Ok(snapshot)
        })
    }

    /// Drops the cache; the next [`AscomDevice::capabilities`] re-reads the driver.
    fn invalidate_capabilities(&self) -> Result<()> {
        self.actor().call(|device| {
            device.invalidate_capabilities();
            Ok(())
        })
    }

    fn connected(&self) -> Result<bool> {
        self.actor().call(|device| device.dispatch().get_bool("Connected"))
    }

    /// `Connecting` is the completion property of the V4 `Connect`/`Disconnect`.
    fn connecting(&self) -> Result<bool> {
        self.actor().call(|device| device.dispatch().get_bool("Connecting"))
    }

    /// Legacy synchronous connection path: every driver version supports the
    /// property, and setting it twice with the same value is a no-op.
    ///
    /// Invalidates the capability cache, because capabilities are only meaningful
    /// for the connection they were read on.
    fn set_connected(&self, value: bool) -> Result<()> {
        self.actor().call(move |device| {
            device.dispatch().set_bool("Connected", value)?;
            device.invalidate_capabilities();
            Ok(())
        })
    }

    /// V4 asynchronous `Connect()`, waited out through `Connecting`.
    fn connect(&self) -> Result<()> {
        self.connect_async()?;
        crate::wait::wait_flag_false(self.actor(), "Connecting", crate::wait::WaitSpec::default())
    }

    /// Initiates `Connect()` without waiting.
    fn connect_async(&self) -> Result<()> {
        let version = self.interface_version()?;
        if version < 4 {
            // V2/V3 drivers only have the property.
            return self.set_connected(true);
        }
        let outcome = self
            .actor()
            .call(|device| device.dispatch().call("Connect", &[]).map(|_| ()));
        match outcome {
            Ok(()) => Ok(()),
            // An unimplemented Connect() still means the legacy property path is valid.
            Err(err) if err.is_unsupported() => self.set_connected(true),
            Err(err) => Err(err),
        }
    }

    /// V4 asynchronous `Disconnect()`, waited out through `Connecting`.
    fn disconnect(&self) -> Result<()> {
        let outcome = self
            .actor()
            .call(|device| device.dispatch().call("Disconnect", &[]).map(|_| ()));
        match outcome {
            Ok(()) => {
                let waited = crate::wait::wait_flag_false(
                    self.actor(),
                    "Connecting",
                    crate::wait::WaitSpec::default(),
                );
                self.invalidate_capabilities()?;
                waited
            }
            Err(err) if err.is_unsupported() => {
                self.set_connected(false)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn name(&self) -> Result<String> {
        self.actor().call(|device| device.dispatch().get_string("Name"))
    }

    fn description(&self) -> Result<String> {
        self.actor().call(|device| device.dispatch().get_string("Description"))
    }

    fn driver_info(&self) -> Result<String> {
        self.actor().call(|device| device.dispatch().get_string("DriverInfo"))
    }

    fn driver_version(&self) -> Result<String> {
        self.actor().call(|device| device.dispatch().get_string("DriverVersion"))
    }

    fn interface_version(&self) -> Result<i16> {
        self.actor().call(|device| device.dispatch().get_i16("InterfaceVersion"))
    }

    /// Action names the driver advertises; empty when it implements none.
    fn supported_actions(&self) -> Result<Vec<String>> {
        self.actor().call(|device| {
            let value = match device.dispatch().try_get("SupportedActions")? {
                Some(v) => v,
                None => return Ok(Vec::new()),
            };
            collections::strings(&value)
        })
    }

    /// Aggregated operational state; `Ok(vec![])` when the driver exposes none.
    ///
    /// A driver that refuses to answer `InterfaceVersion` is not a pre-V4 driver, so
    /// that failure reaches the caller instead of looking like an empty state.
    fn device_state(&self) -> Result<Vec<StateValue>> {
        self.actor().call(|device| read_device_state(device.dispatch()))
    }

    /// Driver-specific extension point. `parameters` is a single opaque string;
    /// pass `""` when the action takes none.
    fn action(&self, name: &str, parameters: &str) -> Result<String> {
        let name = name.to_string();
        let parameters = parameters.to_string();
        self.actor().call(move |device| {
            let name_v = Variant::from_str(&name);
            let params_v = Variant::from_str(&parameters);
            let result = device.dispatch().call("Action", &[&name_v, &params_v])?;
            match result {
                Some(v) => v.as_str(),
                None => Ok(String::new()),
            }
        })
    }

    /// Deprecated in V4 in favour of [`AscomDevice::action`], still widely implemented.
    fn command_blind(&self, command: &str, raw: bool) -> Result<()> {
        let command = command.to_string();
        self.actor().call(move |device| {
            let cmd = Variant::from_str(&command);
            let raw_v = Variant::from_bool(raw);
            device.dispatch().call_void("CommandBlind", &[&cmd, &raw_v])
        })
    }

    /// Deprecated in V4 in favour of [`AscomDevice::action`].
    fn command_bool(&self, command: &str, raw: bool) -> Result<bool> {
        let command = command.to_string();
        self.actor().call(move |device| {
            let cmd = Variant::from_str(&command);
            let raw_v = Variant::from_bool(raw);
            let reply = device.dispatch().call("CommandBool", &[&cmd, &raw_v])?;
            expect_value("CommandBool", reply)?.as_bool()
        })
    }

    /// Deprecated in V4 in favour of [`AscomDevice::action`].
    ///
    /// A `VT_EMPTY` reply is [`AscomErrorKind::ValueNotSet`], not an empty string: a
    /// real empty answer arrives as a zero-length `VT_BSTR`, as in
    /// [`AscomDevice::command_bool`].
    fn command_string(&self, command: &str, raw: bool) -> Result<String> {
        let command = command.to_string();
        self.actor().call(move |device| call_command_string(device.dispatch(), &command, raw))
    }

    /// Opens the driver's own setup dialog.
    ///
    /// The dialog belongs to the driver process for the EXE local servers Platform 7
    /// installs, so the call simply blocks until the user closes it; the worker still
    /// pumps messages before and after, which is what in-process servers need.
    fn setup_dialog(&self) -> Result<()> {
        self.actor().call(|device| {
            device.dispatch().call_void("SetupDialog", &[])?;
            crate::com::apartment::pump_messages();
            Ok(())
        })
    }

    /// Reads any member by name and renders it, mapping "unimplemented" to a value.
    /// Intended for diagnostics and for walking an unfamiliar driver.
    fn probe(&self, member: &str) -> Result<String> {
        let member = member.to_string();
        self.actor().call(move |device| device.dispatch().probe(&member))
    }

    /// Reads any member as a UTC timestamp (`UTCDate` and friends).
    fn probe_date(&self, member: &str) -> Result<SystemTime> {
        let member = member.to_string();
        self.actor().call(move |device| {
            let value = device.dispatch().get(&member)?;
            value.as_date()
        })
    }
}

/// Reads `DeviceState`, gated on `InterfaceVersion`.
///
/// `Ok(vec![])` is the honest answer only when the driver has no state to report:
/// a pre-V4 driver, or one that does not expose `InterfaceVersion` at all. A driver
/// that refuses the version read (not connected, a COM failure) is an error, not a
/// driver without state.
pub(crate) fn read_device_state(dispatch: &crate::com::Dispatch) -> Result<Vec<StateValue>> {
    let Some(version) = dispatch.try_get("InterfaceVersion")? else {
        return Ok(Vec::new());
    };
    if version.as_i16()? < 4 {
        return Ok(Vec::new());
    }
    let Some(value) = dispatch.try_get("DeviceState")? else {
        return Ok(Vec::new());
    };
    read_state_values(&value)
}

/// Runs `CommandString` and returns the driver's answer.
pub(crate) fn call_command_string(
    dispatch: &crate::com::Dispatch,
    command: &str,
    raw: bool,
) -> Result<String> {
    let cmd = Variant::from_str(command);
    let raw_v = Variant::from_bool(raw);
    let reply = dispatch.call("CommandString", &[&cmd, &raw_v])?;
    expect_value("CommandString", reply)?.as_str()
}

/// The value a value-returning call answered with.
///
/// `None` is a `VT_EMPTY` result, i.e. the driver returned no value at all. That is an
/// error and never a default: a real empty answer arrives as a zero-length `VT_BSTR`,
/// so an absent result cannot be read as "the driver said empty".
fn expect_value(member: &str, reply: Option<Variant>) -> Result<Variant> {
    reply.ok_or_else(|| {
        AscomError::local(
            AscomErrorKind::ValueNotSet,
            member,
            "the driver returned no value",
        )
    })
}

/// Reads the common identity members plus the given capability members.
pub(crate) fn read_snapshot(
    dispatch: &crate::com::Dispatch,
    flag_members: &[&str],
) -> Result<CapabilitySnapshot> {
    let mut snapshot = CapabilitySnapshot {
        // Identity members are mandatory in every interface version, so a failure here
        // is a real error rather than a missing member: a driver that refuses them
        // (the OmniSim Camera gates them on `Connected`) must not be recorded as an
        // empty string.
        interface_version: dispatch.get_i16("InterfaceVersion")?,
        name: dispatch.get_string("Name")?,
        description: dispatch.get_string("Description")?,
        driver_info: dispatch.get_string("DriverInfo")?,
        driver_version: dispatch.get_string("DriverVersion")?,
        supported_actions: Vec::new(),
        flags: BTreeMap::new(),
    };
    // `try_get` softens only `Unsupported`, so `None` here is a driver whose type has
    // no `SupportedActions` at all — the normal answer of a pre-V4 driver, for which an
    // empty list is honest. A refusal or an unreadable collection is not a statement
    // about the actions, so it propagates, exactly like the live read in
    // `AscomDevice::supported_actions`: the snapshot is cached for the whole
    // connection, and an empty list here would make `has_action` answer `false` where
    // the truth is "the read failed".
    if let Some(actions) = dispatch.try_get("SupportedActions")? {
        snapshot.supported_actions = collections::strings(&actions)?;
    }
    for member in flag_members {
        // `Unsupported` is recorded as `None`, never silently coerced to `false`.
        let answer = match dispatch.get_bool(member) {
            Ok(value) => Some(value),
            Err(err) if err.is_unsupported() => None,
            // A driver may also refuse because it is not connected; that is not an
            // answer about the capability, so record it as unknown and move on.
            Err(_) => None,
        };
        snapshot.flags.insert((*member).to_string(), answer);
    }
    Ok(snapshot)
}

/// Turns a `List[StateValue]` into `(Name, Value)` pairs.
pub(crate) fn read_state_values(value: &Variant) -> Result<Vec<StateValue>> {
    let objects = collections::objects(value)?;
    let mut out = Vec::with_capacity(objects.len());
    for item in &objects {
        let name = item.get_string("Name").unwrap_or_default();
        let raw = item.get("Value");
        let value = match raw {
            Ok(v) => v.kind(),
            // A StateValue without a value is still a name worth reporting.
            Err(_) => VariantKind::Empty,
        };
        out.push(StateValue { name, value });
    }
    Ok(out)
}

/// `true` when an error means "this driver does not have that member".
pub fn is_unsupported(err: &AscomError) -> bool {
    err.is_unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::mock::{Element, Member, MockDevice};
    use windows::core::HRESULT;
    use windows::Win32::Foundation::E_FAIL;

    /// An ASCOM exception HRESULT, e.g. code `0x407` = NotConnected.
    fn ascom_hr(code: u16) -> HRESULT {
        HRESULT(0x8004_0000_u32 as i32 | i32::from(code))
    }

    /// A driver object that answers exactly the configured members. The returned
    /// `Variant` keeps the mock alive.
    fn mock_dispatch(members: Vec<(&'static str, Member)>) -> (Variant, crate::com::Dispatch) {
        let variant = MockDevice::new(members).into_variant();
        let dispatch =
            crate::com::Dispatch::from_variant(&variant).expect("the mock is dispatchable");
        (variant, dispatch)
    }

    #[test]
    fn snapshot_distinguishes_false_from_unimplemented() {
        let mut snapshot = CapabilitySnapshot::default();
        snapshot.flags.insert("CanSlew".to_string(), Some(true));
        snapshot.flags.insert("CanPark".to_string(), Some(false));
        snapshot.flags.insert("CanSetPierSide".to_string(), None);

        assert_eq!(snapshot.flag("CanSlew"), Some(true));
        assert_eq!(snapshot.flag("CanPark"), Some(false));
        assert_eq!(snapshot.flag("CanSetPierSide"), None);
        assert_eq!(snapshot.flag("NotEvenDeclared"), None);

        assert!(snapshot.supports("CanSlew"));
        assert!(!snapshot.supports("CanPark"));
        assert!(!snapshot.supports("CanSetPierSide"), "unknown is not supported");
    }

    #[test]
    fn interface_version_gates_device_state() {
        let mut snapshot = CapabilitySnapshot::default();
        assert!(!snapshot.has_device_state());
        snapshot.interface_version = 4;
        assert!(snapshot.has_device_state());
    }

    #[test]
    fn action_names_are_case_insensitive() {
        let snapshot = CapabilitySnapshot {
            supported_actions: vec!["SelectWheel".to_string()],
            ..Default::default()
        };
        assert!(snapshot.has_action("selectwheel"));
        assert!(snapshot.has_action("SELECTWHEEL"));
        assert!(!snapshot.has_action("Focus"));
    }

    #[test]
    fn guide_directions_use_the_spec_numbers() {
        assert_eq!(GuideDirection::to_raw(GuideDirection::North), 0);
        assert_eq!(GuideDirection::to_raw(GuideDirection::West), 3);
        assert_eq!(GuideDirection::from_raw(1), Some(GuideDirection::South));
        assert_eq!(GuideDirection::from_raw(4), None);
    }

    #[test]
    fn spec_from_env_reports_a_clear_error() {
        let missing = DeviceSpec::from_env("ASCOM_DEFINITELY_NOT_SET_IN_TESTS");
        let err = missing.unwrap_err();
        assert_eq!(err.kind, AscomErrorKind::NotFound);
        assert!(err.message.contains("ASCOM_DEFINITELY_NOT_SET_IN_TESTS"));
    }

    #[test]
    fn a_refused_version_read_is_not_a_driver_without_state() {
        // A driver that does not expose `InterfaceVersion` at all (a V1 driver) has no
        // `DeviceState` either, and that is a normal answer rather than a failure.
        let (_keep, dispatch) = mock_dispatch(Vec::new());
        assert!(
            read_device_state(&dispatch).expect("a member the driver lacks is not an error")
                .is_empty()
        );
        // A version below 4 is the same normal answer.
        let (_keep, dispatch) =
            mock_dispatch(vec![("InterfaceVersion", Member::Value(Element::Int(3)))]);
        assert!(read_device_state(&dispatch).expect("V3 has no DeviceState").is_empty());

        // Refusing the version read is the driver talking about the device, not about
        // its interface version, so it must not look like "no state to report".
        for (scode, wanted) in [
            (ascom_hr(0x407), AscomErrorKind::NotConnected),
            (ascom_hr(0x40B), AscomErrorKind::InvalidOperation),
            (E_FAIL, AscomErrorKind::Com),
        ] {
            let (_keep, dispatch) =
                mock_dispatch(vec![("InterfaceVersion", Member::Refuses(scode))]);
            let error = read_device_state(&dispatch)
                .expect_err("a refusal must not be reported as an empty state");
            assert_eq!(error.kind, wanted, "misclassified: {error}");
        }
    }

    /// The five mandatory identity members, with `refused` raising `NotConnected`
    /// instead of answering — the behaviour measured on a disconnected OmniSim Camera.
    fn identity_members(refused: Option<&str>) -> Vec<(&'static str, Member)> {
        [
            ("InterfaceVersion", Member::Value(Element::Int(3))),
            ("Name", Member::Value(Element::Str("OmniSim Camera"))),
            ("Description", Member::Value(Element::Str("Alpaca Camera Simulator"))),
            ("DriverInfo", Member::Value(Element::Str("CameraSimulator, Version=0.5"))),
            ("DriverVersion", Member::Value(Element::Str("0.5"))),
        ]
        .into_iter()
        .map(|(name, answer)| {
            let answer =
                if Some(name) == refused { Member::Refuses(ascom_hr(0x407)) } else { answer };
            (name, answer)
        })
        .collect()
    }

    #[test]
    fn a_refused_identity_member_fails_the_snapshot() {
        for refused in ["Name", "Description", "DriverInfo", "DriverVersion"] {
            let (_keep, dispatch) = mock_dispatch(identity_members(Some(refused)));
            let error = read_snapshot(&dispatch, &[])
                .expect_err("identity members are mandatory, so a refusal is an error");
            assert_eq!(error.kind, AscomErrorKind::NotConnected, "refusing {refused}: {error}");
            assert!(error.member.contains(refused), "the member is lost: {error}");
        }

        // A driver that answers everything still produces the full snapshot, and a
        // capability member it does not expose stays unknown rather than false.
        let (_keep, dispatch) = mock_dispatch(identity_members(None));
        let snapshot =
            read_snapshot(&dispatch, &["CanAsymmetricBin"]).expect("a cooperative driver");
        assert_eq!(snapshot.interface_version, 3);
        assert_eq!(snapshot.name, "OmniSim Camera");
        assert_eq!(snapshot.description, "Alpaca Camera Simulator");
        assert_eq!(snapshot.driver_info, "CameraSimulator, Version=0.5");
        assert_eq!(snapshot.driver_version, "0.5");
        assert_eq!(snapshot.flag("CanAsymmetricBin"), None);
        assert!(snapshot.supported_actions.is_empty(), "the driver exposes no member at all");
    }

    /// The action list lands in a snapshot cached for the whole connection, so a read
    /// that failed must not be recorded as "this device supports no actions".
    #[test]
    fn a_failed_supported_actions_read_fails_the_snapshot() {
        // A refusal is the driver talking about the device, not about its actions.
        for (scode, kind) in [
            (ascom_hr(0x407), AscomErrorKind::NotConnected),
            (ascom_hr(0x40B), AscomErrorKind::InvalidOperation),
            (E_FAIL, AscomErrorKind::Com),
        ] {
            let mut members = identity_members(None);
            members.push(("SupportedActions", Member::Refuses(scode)));
            let (_keep, dispatch) = mock_dispatch(members);
            let error = read_snapshot(&dispatch, &[])
                .expect_err("a refused action read is not an empty action list");
            assert_eq!(error.kind, kind, "misclassified: {error}");
            assert!(
                error.member.contains("SupportedActions"),
                "the member is lost: {error}"
            );
        }

        // A member that answers with something other than a collection is a driver
        // protocol violation, and `collections::strings` refuses it. `unwrap_or_default`
        // used to turn exactly that into an empty list.
        let mut members = identity_members(None);
        members.push(("SupportedActions", Member::Value(Element::Int(7))));
        let (_keep, dispatch) = mock_dispatch(members);
        let error = read_snapshot(&dispatch, &[])
            .expect_err("a non-collection answer is not an empty action list");
        assert_eq!(error.kind, AscomErrorKind::Com, "misclassified: {error}");
    }

    #[test]
    fn an_empty_command_string_reply_is_value_not_set() {
        // A real empty answer is a zero-length `VT_BSTR`, so it still reads as `""`.
        let (_keep, dispatch) =
            mock_dispatch(vec![("CommandString", Member::Value(Element::Str("")))]);
        assert_eq!(
            call_command_string(&dispatch, "GET", false).expect("an empty string is an answer"),
            ""
        );

        // `VT_EMPTY` is the absence of an answer, which `CommandBool` already refuses.
        let (_keep, dispatch) =
            mock_dispatch(vec![("CommandString", Member::Value(Element::Empty))]);
        let error = call_command_string(&dispatch, "GET", false)
            .expect_err("no value at all is not an empty answer");
        assert_eq!(error.kind, AscomErrorKind::ValueNotSet);
        assert_eq!(error.member, "CommandString");
    }
}
