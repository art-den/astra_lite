use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{prelude::*, BufWriter, Cursor};
use std::net::TcpStream;
use std::process::{Command, Child, Stdio};
use std::sync::{Mutex, Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use bitflags::bitflags;
use chrono::prelude::*;
use super::{sexagesimal::*, xml_reader::*, error::*, xml_helper::*};


#[derive(Clone)]
pub struct ConnSettings {
    pub remote: bool,
    pub host: String,
    pub server_exe: String,
    pub drivers: Vec<String>,
    pub activate_all_devices: bool,
}

impl Default for ConnSettings {
    fn default() -> Self {
        Self {
            remote: false,
            host: "localhost".to_string(),
            server_exe: "indiserver".to_string(),
            drivers: Vec::new(),
            activate_all_devices: true,
        }
    }
}

enum XmlSenderItem {
    Xml(String),
    Exit
}

struct ActiveConnData {
    indiserver:    Option<Child>,
    tcp_stream:    TcpStream,
    xml_sender:    XmlSender,
    events_thread: JoinHandle<()>,
    read_thread:   JoinHandle<()>,
    write_thread:  JoinHandle<()>,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Error(String)
}

#[derive(Clone)]
pub struct NewDeviceEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub interface:   DriverInterface,
}

pub struct DeviceConnectEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub connected:   bool,
    pub interface:   DriverInterface,
}

#[derive(Debug)]
pub struct PropChangeValue {
    pub elem_name:  Arc<String>,
    pub prop_value: PropValue,
}

#[derive(Debug)]
pub enum PropChange {
    New(PropChangeValue),
    Change{
        value:      PropChangeValue,
        prev_state: PropState,
        new_state:  PropState,
    },
    Delete,
}

#[derive(Debug)]
pub struct PropChangeEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub prop_name:   Arc<String>,
    pub change:      PropChange,
}

pub struct DeviceDeleteEvent {
    pub timestamp:     Option<DateTime<Utc>>,
    pub device_name:   Arc<String>,
    pub drv_interface: DriverInterface,
}

pub struct MessageEvent {
    pub timestamp:   Option<DateTime<Utc>>,
    pub device_name: Arc<String>,
    pub text:        Arc<String>,
}

pub struct BlobStartEvent {
    pub device_name: Arc<String>,
    pub prop_name:   Arc<String>,
    pub elem_name:   Arc<String>,
}

#[derive(Clone)]
pub enum Event {
    ConnChange(ConnState),
    NewDevice(NewDeviceEvent),
    DeviceConnected(Arc<DeviceConnectEvent>),
    PropChange(Arc<PropChangeEvent>),
    DeviceDelete(Arc<DeviceDeleteEvent>),
    ReadTimeOut,
    Message(Arc<MessageEvent>),
    BlobStart(Arc<BlobStartEvent>),
}

type EventFun = dyn Fn(Event) + Send + 'static;

#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct Subscription(u64);

struct Subscriptions {
    items: HashMap<Subscription, Box<EventFun>>,
    key:   u64,
}

impl Subscriptions {
    fn new() -> Self {
        Self {
            items: HashMap::new(),
            key:   0,
        }
    }

    fn inform_all(&self, event: Event) {
        for fun in self.items.values() {
            fun(event.clone());
        }
    }
}

#[derive(Debug, Clone)]
pub enum NumFormat {
    Float{ width: Option<u8>, prec: u8 },
    G,
    Sexagesimal { zero: bool, width: Option<u8>, frac: u8 },
    Unrecorgnized,
}

impl NumFormat {
    pub fn new_from_indi_format(format_str: &str) -> Self {
        use once_cell::sync::OnceCell;
        static FLOAT_RE: OnceCell<regex::Regex> = OnceCell::new();
        let float_re = FLOAT_RE.get_or_init(|| {
            regex::Regex::new(r"%(\d*)\.(\d*)[Ff]").unwrap()
        });
        if let Some(float_re_res) = float_re.captures(format_str) {
            let width: Option<u8> = float_re_res[1].parse().ok();
            let prec: u8 = float_re_res[2].parse().unwrap_or(0);
            return NumFormat::Float { width, prec };
        }
        static G_RE: OnceCell<regex::Regex> = OnceCell::new();
        let g_re = G_RE.get_or_init(|| {
            regex::Regex::new(r"%.*[Gg]").unwrap()
        });
        if g_re.is_match(format_str) {
            return NumFormat::G;
        }
        static SEX_RE: OnceCell<regex::Regex> = OnceCell::new();
        let sex_re = SEX_RE.get_or_init(|| {
            regex::Regex::new(r"%(\d*)\.(\d*)[Mm]").unwrap()
        });
        if let Some(sex_re_res) = sex_re.captures(format_str) {
            let width_str = &sex_re_res[1];
            let zero = width_str.starts_with("0");
            let width: Option<u8> = width_str.parse().ok();
            let frac: u8 = sex_re_res[2].parse().unwrap_or(0);
            return NumFormat::Sexagesimal { zero, width, frac };
        }
        NumFormat::Unrecorgnized
    }

    pub fn value_to_string(&self, value: f64) -> String {
        match self {
            NumFormat::Float { width, prec } =>
                match width {
                    Some(width) => format!(
                        "{:width$.prec$}",
                        value,
                        width = *width as usize,
                        prec = *prec as usize
                    ),
                    None => format!(
                        "{:.prec$}",
                        value,
                        prec = *prec as usize
                    ),
                }
            NumFormat::G =>
                value.to_string(),
            NumFormat::Sexagesimal { zero, frac, .. } =>
                value_to_sexagesimal(value, *zero, *frac),
            NumFormat::Unrecorgnized =>
                format!("{:.7}", value),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropState { Idle, Ok, Busy, Alert }

impl PropState {
    fn from_str(text: &str) -> anyhow::Result<Self> {
        match text {
            "Idle"  => Ok(PropState::Idle),
            "Ok"    => Ok(PropState::Ok),
            "Busy"  => Ok(PropState::Busy),
            "Alert" => Ok(PropState::Alert),
            s       => Err(anyhow::anyhow!("Unknown property state: {}", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum PropPermition { RO, WO, RW }

impl PropPermition {
    fn from_str(text: Option<&str>) -> anyhow::Result<Self> {
        match text {
            Some("ro") => Ok(PropPermition::RO),
            Some("wo") => Ok(PropPermition::WO),
            Some("rw") => Ok(PropPermition::RW),
            Some(s)    => Err(anyhow::anyhow!("Unknown property permission: {}", s)),
            _          => Ok(PropPermition::RO),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SwitchRule { OneOfMany, AtMostOne, AnyOfMany }

impl SwitchRule {
    fn from_str(text: &str) -> anyhow::Result<Self> {
        match text {
            "OneOfMany" => Ok(SwitchRule::OneOfMany),
            "AtMostOne" => Ok(SwitchRule::AtMostOne),
            "AnyOfMany" => Ok(SwitchRule::AnyOfMany),
            s           => Err(anyhow::anyhow!("Unknown switch rule: {}", s)),
        }
    }
}

pub enum BlobEnable { Never, Also, Only }

#[derive(Clone, Copy, Debug)]
pub enum CamCcd { Primary, Secondary }

impl CamCcd {
    pub fn from_ccd_prop_name(name: &str) -> Self {
        match name {
            "CCD1"|"" => Self::Primary,
            "CCD2"    => Self::Secondary,
            _         => panic!("Wrong CCD property name ({})", name),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum PropType {
    Text,
    Num,
    Switch(SwitchRule),
    Light,
    Blob
}

impl PropType {
    fn to_str(&self) -> &'static str {
        match self {
            PropType::Text      => "Text",
            PropType::Num       => "Num",
            PropType::Switch(_) => "Switch",
            PropType::Light     => "Light",
            PropType::Blob      => "Blob",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlobPropValue {
    pub format:  String,
    pub data:    Vec<u8>,
    pub dl_time: f64,
}

impl PartialEq for BlobPropValue {
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format &&
        self.data == other.data
    }
}

#[derive(Debug, Clone)]
pub struct NumPropValue {
    pub value:  f64,
    pub min:    f64,
    pub max:    f64,
    pub step:   Option<f64>,
    pub format: Arc<String>,
}

#[derive(Debug, Clone)]
pub enum PropValue {
    Text(Arc<String>),
    Switch(bool),
    Light(Arc<String>),
    Blob(Arc<BlobPropValue>),
    Num(NumPropValue),
}

impl PartialEq for PropValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(l0), Self::Text(r0)) =>
                l0 == r0,
            (Self::Num(NumPropValue{value: l0, ..}), Self::Num(NumPropValue{value: r0, ..})) => {
                if l0.is_nan() && r0.is_nan() {
                    true
                } else if !l0.is_nan() && r0.is_nan() {
                    false
                } else if l0.is_nan() && !r0.is_nan() {
                    false
                } else {
                    l0 == r0
                }
            },
            (Self::Switch(l0), Self::Switch(r0)) =>
                l0 == r0,
            (Self::Light(l0), Self::Light(r0)) =>
                l0 == r0,
            (Self::Blob(l0), Self::Blob(r0)) =>
                l0 == r0,
            _ => false,
        }
    }
}

impl PropValue {
    pub fn to_i32(&self) -> Result<i32> {
        match self {
            Self::Num(NumPropValue{value, ..}) =>
                Ok(*value as i32),
                Self::Text(text) =>
                text.parse()
                    .map_err(|_| Error::CantConvertPropValue(
                        text.to_string(),
                        "Text".into(),
                        "i32".into()
                    )),
                    Self::Switch(value) =>
                Ok(if *value {1} else {0}),
                Self::Light(text) => Err(Error::CantConvertPropValue(
                text.to_string(),
                "light".into(),
                "i32".into()
            )),
            Self::Blob(_) => Err(Error::CantConvertPropValue(
                "[blob]".into(),
                "Blob".into(),
                "i32".into()
            )),
        }
    }

    pub fn to_f64(&self) -> Result<f64> {
        match self {
            Self::Num(NumPropValue{value, ..}) =>
                Ok(*value as f64),
            Self::Text(text) =>
                text.parse()
                    .map_err(|_| Error::CantConvertPropValue(
                        text.to_string(),
                        "Text".into(),
                        "f64".into()
                    )),
            Self::Switch(value) => Err(Error::CantConvertPropValue(
                value.to_string(),
                "switch".into(),
                "f64".into()
            )),
            Self::Light(text) => Err(Error::CantConvertPropValue(
                text.to_string(),
                "light".into(),
                "f64".into()
            )),
            Self::Blob(_) => Err(Error::CantConvertPropValue(
                "[blob]".into(),
                "Blob".into(),
                "f64".into()
            )),
        }
    }

    pub fn to_bool(&self) -> Result<bool> {
        match self {
            Self::Num(NumPropValue{value, ..}) => Err(Error::CantConvertPropValue(
                value.to_string(),
                "switch".into(),
                "f64".into()
            )),
            Self::Text(text) =>
                text.parse()
                    .map_err(|_| Error::CantConvertPropValue(
                        text.to_string(),
                        "Text".into(),
                        "f64".into()
                    )),
            Self::Switch(value) =>
                Ok(*value),
            Self::Light(text) => Err(Error::CantConvertPropValue(
                text.to_string(),
                "light".into(),
                "f64".into()
            )),
            Self::Blob(_) => Err(Error::CantConvertPropValue(
                "[blob]".into(),
                "Blob".into(),
                "f64".into()
            )),
        }
    }

    pub fn to_string_for_logging(&self) -> String {
        match self {
            Self::Blob(blob) =>
                format!("[BLOB len={}]", blob.data.len()),
            _ =>
                format!("{:?}", &self)
        }
    }

    pub fn to_string(&self) -> String {
        match self {
            Self::Num(NumPropValue{value, ..}) =>
               value.to_string(),
            Self::Text(text) =>
                text.to_string(),
            Self::Switch(value) =>
                value.to_string(),
            Self::Light(text) =>
                text.to_string(),
            Self::Blob(_) =>
                "[blob]".to_string(),
        }
    }

    pub fn type_str(&self) -> &'static str {
        match self {
            PropValue::Text(_) => "Text",
            PropValue::Switch(_) => "Switch",
            PropValue::Light(_) => "Light",
            PropValue::Blob(_) => "Blob",
            PropValue::Num(_) => "Num",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PropElement {
    pub name:  Arc<String>,
    pub label: Option<Arc<String>>,
    pub value: PropValue,
}

#[derive(Debug, Clone)]
pub struct Property {
    pub device:    Arc<String>,
    pub name:      Arc<String>,
    pub type_:     PropType,
    pub label:     Option<Arc<String>>,
    pub group:     Option<Arc<String>>,
    pub permition: PropPermition,
    pub state:     PropState,
    pub timeout:   Option<u32>,
    pub timestamp: Option<DateTime<Utc>>,
    pub message:   Option<Arc<String>>,
    pub elements:  Vec<PropElement>,
    pub change_id: u64,
}

impl Property {
    fn new_from_xml(
        xml:       xmltree::Element,
        dev_name:  &Arc<String>,
        prop_name: &str
    ) -> anyhow::Result<Self> {
        let mut xml = xml;
        let type_ = match xml.name.as_str() {
            "defTextVector" => PropType::Text,
            "defNumberVector" => PropType::Num,
            "defSwitchVector" => {
                let rule = SwitchRule::from_str(xml.attr_str_or_err("rule")?)?;
                PropType::Switch(rule)
            },
            "defBLOBVector" => PropType::Blob,
            "defLightVector" => PropType::Light,
            s => anyhow::bail!("Unknown vector: {}", s),
        };

        let label = xml.attributes.remove("label");
        let group = xml.attributes.remove("group");
        let permition = PropPermition::from_str(xml.attr_str_or_err("perm").ok())?;

        let state = PropState::from_str(xml.attr_str_or_err("state")?)?;
        let timeout = xml.attributes.get("timeout")
            .map(|to_str| to_str.parse::<u32>().unwrap_or(0));
        let message = xml.attributes.remove("message");
        let timestamp = xml.attr_time("timestamp");

        let mut items = Vec::new();
        for mut child in xml.into_elements(None) {
            let name = child.attr_string_or_err("name")?;
            let label = child.attributes.remove("label");

            let value = match child.name.as_str() {
                "defText" => {
                    let value = child
                        .get_text()
                        .unwrap_or_else(|| Cow::from(""))
                        .trim()
                        .to_string();
                    PropValue::Text(Arc::new(value))
                },
                "defNumber" => {
                    let format = child.attr_string_or_err("format")?;
                    let min = child.attr_str_or_err("min")?.parse::<f64>()?;
                    let max = child.attr_str_or_err("max")?.parse::<f64>()?;
                    let step = child.attributes.get("step").map(|v| v.parse::<f64>().unwrap_or(0.0));
                    let value = child
                        .get_text()
                        .ok_or_else(||anyhow::anyhow!("{} without value", child.name))?
                        .trim()
                        .parse::<f64>()?;
                    PropValue::Num(NumPropValue{ value, min, max, step, format: Arc::new(format) })
                },
                "defSwitch" => {
                    let value = child
                        .get_text()
                        .ok_or_else(||anyhow::anyhow!("{} without value", child.name))?
                        .trim()
                        .eq_ignore_ascii_case("On");
                    PropValue::Switch(value)
                },
                "defLight" => {
                    let value = child
                        .get_text()
                        .ok_or_else(||anyhow::anyhow!("{} without value", child.name))?
                        .trim()
                        .to_string();
                    PropValue::Light(Arc::new(value))
                },
                "defBLOB" => {
                    let value = BlobPropValue {
                        format:  String::new(),
                        data:    Vec::new(),
                        dl_time: 0.0,
                    };
                    PropValue::Blob(Arc::new(value))
                },
                other =>
                    anyhow::bail!("Unknown tag `{}`", other),
            };
            items.push(PropElement {
                name: Arc::new(name),
                label: label.map(|label| Arc::new(label)),
                value,
            });
        }
        Ok(Property {
            device: Arc::clone(dev_name),
            name: Arc::new(prop_name.to_string()),
            type_,
            label: label.map(|label| Arc::new(label)),
            group: group.map(|label| Arc::new(label)),
            permition,
            state,
            timeout,
            timestamp,
            message: message.map(|label| Arc::new(label)),
            elements: items,
            change_id: 0,
        })
    }

    fn update_data_from_xml_and_return_changes(
        &mut self,
        xml:         &mut xmltree::Element,
        mut blobs:   Vec<XmlStreamReaderBlob>,
        device_name: &str, // for error message
        prop_name:   &str, // same
    ) -> anyhow::Result<(bool, Vec<(Arc<String>, PropValue)>)> {
        let mut changed = false;
        if let Some(state_str) = xml.attributes.get("state") {
            let new_state = PropState::from_str(state_str)?;
            if new_state != self.state {
                self.state = new_state;
                changed = true;
            }
        }
        if let Some(timeout_str) = xml.attributes.get("timeout") {
            let new_timeout = Some(timeout_str.parse()?);
            if new_timeout != self.timeout {
                self.timeout = new_timeout;
                changed = true;
            }
        }
        let message = xml.attributes.remove("message");
        if self.message.as_deref() != message.as_ref() {
            self.message = message.map(|s| Arc::new(s));
            changed = true;
        }

        let mut changed_values = Vec::new();
        self.timestamp = xml.attr_time("timestamp");
        for child in xml.elements(None) {
            let elem_name = child.attr_str_or_err("name")?;
            if let Some(elem) = self.get_elem_mut(elem_name) {
                let mut elem_changed = false;
                match &mut elem.value {
                    PropValue::Text(text_value) => {
                        let new_value = child
                            .get_text()
                            .map(|s| s.into_owned())
                            .unwrap_or_default();
                        let new_value = new_value.trim();
                        if **text_value != new_value {
                            *text_value = Arc::new(new_value.to_string());
                            changed = true;
                            elem_changed = true;
                        }
                    },
                    PropValue::Num(NumPropValue{ value, .. }) => {
                        let new_value = child
                            .get_text()
                            .ok_or_else(||anyhow::anyhow!("{} without value", child.name))?
                            .trim()
                            .parse::<f64>()?;
                        if *value != new_value {
                            *value = new_value;
                            changed = true;
                            elem_changed = true;
                        }
                    },
                    PropValue::Switch(value) => {
                        let new_value = child
                            .get_text()
                            .ok_or_else(||anyhow::anyhow!("{} without value", child.name))?
                            .trim()
                            .eq_ignore_ascii_case("On");
                        if *value != new_value {
                            *value = new_value;
                            changed = true;
                            elem_changed = true;
                        }
                    },
                    PropValue::Light(value) => {
                        let new_value = child
                            .get_text()
                            .ok_or_else(||anyhow::anyhow!("{} without value", child.name))?;
                        let new_value = new_value.trim();
                        if **value != new_value {
                            *value = Arc::new(new_value.to_string());
                            changed = true;
                            elem_changed = true;
                        }
                    },
                    PropValue::Blob(blob) => {
                        if let Some(blob_pos) = blobs.iter_mut().position(|b| b.name == elem_name) {
                            let new_blob = blobs.remove(blob_pos);
                            let blob_size: usize = child.attributes
                                .get("size")
                                .map(|size_str| size_str.parse())
                                .transpose()?
                                .ok_or_else(|| anyhow::anyhow!(
                                    "`size` attribute of `{}` not found",
                                    elem.name
                                ))?;
                            if blob_size != new_blob.data.len() {
                                anyhow::bail!(
                                    "Declared size of blob ({}) is not equal real blob size ({})",
                                    blob_size, new_blob.data.len()
                                );
                            }
                            *blob = Arc::new(BlobPropValue {
                                format:  new_blob.format,
                                data:    new_blob.data,
                                dl_time: new_blob.dl_time,
                            });
                            changed = true;
                            elem_changed = true;
                        }
                    }
                };

                if elem_changed {
                    let changed_elem_value = elem.value.clone();
                    changed_values.push((Arc::clone(&elem.name), changed_elem_value));
                }
            } else {
                anyhow::bail!(
                    "Element `{}` of property {} of device `{}` not found",
                    elem_name, prop_name, device_name
                );
            }
        }
        Ok((changed, changed_values))
    }

    fn get_elem(&self, name: &str) -> Option<&PropElement> {
        self.elements.iter().find(|elem| *elem.name == name)
    }

    fn get_elem_mut(&mut self, name: &str) -> Option<&mut PropElement> {
        self.elements.iter_mut().find(|elem| *elem.name == name)
    }

    fn get_values(&self) -> Vec<(Arc<String>, PropValue)> {
        self
            .elements
            .iter()
            .map(|v| (Arc::clone(&v.name), v.value.clone()))
            .collect()
    }
}

struct Device {
    name: Arc<String>,
    props: Vec<Property>,
}

impl Device {
    fn new(name: &Arc<String>) -> Self {
        Self {
            name: Arc::clone(name),
            props: Vec::new(),
        }
    }

    fn get_property_opt(&self, prop_name: &str) -> Option<&Property> {
        self.props
            .iter()
            .find(|prop| *prop.name == prop_name)
    }

    fn get_property_element_opt(
        &self,
        prop_name: &str,
        elem_name: &str,
    ) -> Option<&PropElement> {
        self
            .get_property_opt(prop_name)?
            .get_elem(elem_name)
    }

    fn get_property_element(
        &self,
        prop_name: &str,
        elem_name: &str,
    ) -> Result<(&Property, &PropElement)> {
        let property = self
            .get_property_opt(prop_name)
            .ok_or_else(|| Error::PropertyNotExists(self.name.to_string(), prop_name.to_string()))?;
        let elem = property
            .get_elem(elem_name)
            .ok_or_else(|| Error::PropertyElemNotExists(
                self.name.to_string(),
                prop_name.to_string(),
                elem_name.to_string())
            )?;
        Ok((property, elem))
    }

    fn get_property_opt_mut(&mut self, prop_name: &str) -> Option<&mut Property> {
        self.props
            .iter_mut()
            .find(|prop| *prop.name == prop_name)
    }

    fn remove_property(&mut self, prop_name: &str) -> Option<Property> {
        let Some(index) = self.props
            .iter()
            .position(|prop| *prop.name == prop_name)
        else {
            return None;
        };
        Some(self.props.remove(index))
    }

    fn get_interface(&self) -> Option<DriverInterface> {
        let elem = self.get_property_element_opt("DRIVER_INFO", "DRIVER_INTERFACE")?;
        let i32_value = elem.value.to_i32().unwrap_or(0);
        Some(DriverInterface::from_bits_truncate(i32_value as u32))
    }
}

struct Devices {
    list:      Vec<Device>,
    change_id: u64,
}

impl Devices {
    fn new() -> Self {
        Self {
            list:      Vec::new(),
            change_id: 1,
        }
    }

    fn basic_check_device_and_prop_name(
        device_name: &str,
        prop_name:   &str
    ) -> Result<()> {
        if device_name.is_empty() {
            return Err(Error::WrongArgument("Device name is empty".into()));
        }
        if prop_name.is_empty() {
            return Err(Error::WrongArgument("Property name is empty".into()));
        }
        Ok(())
    }

    fn find_by_name_res(&self, device_name: &str) -> Result<&Device> {
        self.list
            .iter()
            .find(|device| *device.name == device_name)
            .ok_or_else(|| Error::DeviceNotExists(device_name.to_string()))
    }

    fn find_by_name_opt(&self, device_name: &str) -> Option<&Device> {
        self.list
            .iter()
            .find(|device| *device.name == device_name)
    }

    fn find_by_name_opt_mut(&mut self, device_name: &str) -> Option<&mut Device> {
        self.list
            .iter_mut()
            .find(|device| *device.name == device_name)
    }

    fn remove(&mut self, device_name: &str) -> Option<Device> {
        let index = self.list.iter().position(|device| *device.name == device_name)?;
        let removed = self.list.remove(index);
        Some(removed)
    }

    fn get_names(&self) -> Vec<Arc<String>> {
        self.list
            .iter()
            .map(|device| Arc::clone(&device.name))
            .collect()
    }

    fn get_list_iter(&self) -> Box<dyn Iterator<Item = ExportDevice> + '_> {
        Box::new(self.list
            .iter()
            .filter_map(|device| device.get_interface().map(|iface| (device, iface)))
            .map(|(device, interface)| ExportDevice { name: Arc::clone(&device.name), interface })
        )
    }

    fn get_properties_list(
        &self,
        device_name:   Option<&str>,
        changed_after: Option<u64>,
    ) -> Vec<Property> {
        self.list
            .iter()
            .filter(|device| {
                device_name.is_none() || Some(device.name.as_str()) == device_name
            })
            .flat_map(|device| {
                device.props.iter().filter_map(|prop| {
                    if let Some(changed_after) = changed_after {
                        if prop.change_id <= changed_after {
                            return None;
                        }
                    }
                    Some(prop.clone())
                })
            })
            .collect()
    }

    fn property_exists(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   Option<&str>
    ) -> Result<bool> {
        let device = self.find_by_name_res(device_name)?;
        if let Some(elem_name) = elem_name {
            let elem_opt = device.get_property_element_opt(prop_name, elem_name);
            Ok(elem_opt.is_some())
        } else {
            let property_opt = device.get_property_opt(prop_name);
            Ok(property_opt.is_some())
        }
    }

    fn check_property_ok_for_writing<'a>(
        &self,
        device_name:     &str,
        prop_name:       &str,
        elem_count:      usize,
        elem_check_type: fn (&PropType) -> bool,
        elem_get_name:   impl Fn(usize) -> &'a str,
        req_type_str:    &str,
    ) -> Result<()> {
        let device = self.find_by_name_res(device_name)?;
        let Some(property) = device.get_property_opt(prop_name)
        else {
            return Err(Error::PropertyNotExists(
                device_name.to_string(),
                prop_name.to_string()
            ));
        };
        if property.permition == PropPermition::RO {
            return Err(Error::PropertyIsReadOnly(
                device_name.to_string(),
                prop_name.to_string(),
            ));
        }
        if !elem_check_type(&property.type_) {
            return Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                property.type_.to_str().to_string(),
                req_type_str.to_string(),
            ));
        }
        for index in 0..elem_count {
            let elem_name = elem_get_name(index);
            let elem_exists = property
                .elements
                .iter()
                .any(|element| *element.name == elem_name);
            if !elem_exists {
                return Err(Error::PropertyElemNotExists(
                    device_name.to_string(),
                    prop_name.to_string(),
                    elem_name.to_string(),
                ));
            }
        }
        Ok(())
    }

    fn get_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<bool> {
        Self::basic_check_device_and_prop_name(device_name, prop_name)?;
        let device = self.find_by_name_res(device_name)?;
        let (property, elem_value) = device.get_property_element(prop_name, elem_name)?;
        if !matches!(property.type_, PropType::Switch(_)) {
            return Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                property.type_.to_str().to_string(),
                "Switch".to_string()
            ));
        }
        if let PropValue::Switch(value) = &elem_value.value {
            Ok(*value)
        } else {
            Err(Error::Internal(format!(
                "Switch property contains value of other type {:?}",
                elem_value.value.type_str()
            )))
        }
    }

    pub fn get_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<&NumPropValue> {
        Self::basic_check_device_and_prop_name(device_name, prop_name)?;
        let device = self.find_by_name_res(device_name)?;
        let (property, elem_value) = device.get_property_element(prop_name, elem_name)?;
        if property.type_ != PropType::Num {
            return Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                property.type_.to_str().to_string(),
                "Num".to_string()
            ));
        }
        if let PropValue::Num(value) = &elem_value.value {
            Ok(value)
        } else {
            Err(Error::Internal(format!(
                "Num property contains value of other type {:?}",
                elem_value.value.type_str()
            )))
        }
    }

    pub fn get_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<Arc<String>> {
        Self::basic_check_device_and_prop_name(device_name, prop_name)?;
        let device = self.find_by_name_res(device_name)?;
        let (property, elem_value) = device.get_property_element(prop_name, elem_name)?;
        if property.type_ != PropType::Text {
            return Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                property.type_.to_str().to_string(),
                "Text".to_string()
            ));
        }
        if let PropValue::Text(value) = &elem_value.value {
            Ok(Arc::clone(value))
        } else {
            Err(Error::Internal(format!(
                "Num property contains value of other type {:?}",
                elem_value.value.type_str()
            )))
        }
    }

    fn existing_prop_name<'a>(
        &self,
        device_name:   &str,
        prop_and_elem: &[(&'a str, &'a str)]
    ) -> Result<(&'a str, &'a str)> {
        let device = self.find_by_name_res(device_name)?;
        for &(prop_name, elem_name) in prop_and_elem {
            let Some(prop) = device.get_property_opt(prop_name) else {
                continue;
            };
            let elem_exists = prop.elements.iter().any(|e|
                elem_name.is_empty() || *e.name == elem_name
            );
            if elem_exists {
                return Ok((prop_name, elem_name));
            }
        }
        Err(Error::NoOnePropertyFound(device_name.to_string()))
    }

    fn get_driver_interface(&self, device_name: &str) -> Result<DriverInterface> {
        let (_, elem) = self
            .find_by_name_res(device_name)?
            .get_property_element("DRIVER_INFO", "DRIVER_INTERFACE")?;
        let interface_i32 = elem.value.to_i32().unwrap_or(0);
        let interface = DriverInterface::from_bits_truncate(interface_i32 as u32);
        Ok(interface)
    }

    fn get_property(
        &self,
        device_name: &str,
        prop_name:   &str,
    ) -> Result<&Property> {
        let device = self.find_by_name_res(device_name)?;
        let Some(property) = device.get_property_opt(prop_name) else {
            return Err(Error::PropertyNotExists(
                device_name.to_string(),
                prop_name.to_string()
            ));
        };
        Ok(property)
    }

    fn is_device_enabled(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "CONNECTION",
            "CONNECT"
        )
    }
}


bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct DriverInterface: u32 {
        const GENERAL       = 0;
        const TELESCOPE     = (1 << 0);
        const CCD           = (1 << 1);
        const GUIDER        = (1 << 2);
        const FOCUSER       = (1 << 3);
        const FILTER        = (1 << 4);
        const DOME          = (1 << 5);
        const GPS           = (1 << 6);
        const WEATHER       = (1 << 7);
        const AO            = (1 << 8);
        const DUSTCAP       = (1 << 9);
        const LIGHTBOX      = (1 << 10);
        const DETECTOR      = (1 << 11);
        const ROTATOR       = (1 << 12);
        const SPECTROGRAPH  = (1 << 13);
        const CORRELATOR    = (1 << 14);
        const AUX           = (1 << 15);
    }
}

pub enum DeviceCap {
    CcdTemperature,
    CcdExposure,
    CcdGain,
    CcdOffset,
}

#[derive(Debug)]
pub struct ExportDevice {
    pub name:      Arc<String>,
    pub interface: DriverInterface,
}

pub enum FrameType {
    Light,
    Flat,
    Dark,
    Bias,
}

pub enum CaptureFormat {
    Rgb,
    Raw,
}

pub enum BinningMode {
    Add,
    Avg,
}

pub struct Connection {
    data:          Arc<Mutex<Option<ActiveConnData>>>,
    state:         Arc<Mutex<ConnState>>,
    devices:       Arc<Mutex<Devices>>,
    subscriptions: Arc<Mutex<Subscriptions>>,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(
                None
            )),
            state: Arc::new(Mutex::new(
                ConnState::Disconnected
            )),
            devices: Arc::new(Mutex::new(
                Devices::new()
            )),
            subscriptions: Arc::new(
                Mutex::new(Subscriptions::new())
            ),
        }
    }

    pub fn subscribe_events(
        &self,
        fun: impl Fn(Event) + Send + 'static
    ) -> Subscription {
        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.key += 1;
        let subscription = Subscription(subscriptions.key);
        subscriptions.items.insert(
            subscription,
            Box::new(fun)
        );
        subscription
    }

    pub fn unsubscribe(&self, subscription: Subscription) {
        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.items.remove(&subscription);
    }

    pub fn unsubscribe_all(&self) {
        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.items.clear();
    }

    fn start_indi_server(
        exe:     &str,
        drivers: &Vec<String>,
    ) -> anyhow::Result<Child> {
        // Start indiserver process
        let mut child = Command::new(exe)
            .args(drivers.clone())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        // Wait 1 seconds and check is it alive
        std::thread::sleep(Duration::from_millis(1000));
        if let Ok(Some(status)) = child.try_wait() {
            // kill zombie
            _ = child.kill();
            _ = child.wait();
            // read stderr of process and return error information
            let mut stderr_str = String::new();
            let stderr_ok = child.stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr_str).is_ok();
            if stderr_ok {
                let stderr_lines: Vec<_> = stderr_str
                    .split("\n")
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.ends_with("good bye"))
                    .collect();
                if !stderr_lines.is_empty() {
                    let mut err_text = *stderr_lines.last().unwrap();
                    if let Some(space_pos) = err_text.find(" ") {
                        err_text = &err_text[space_pos..];
                    }
                    anyhow::bail!(
                        "Process `{}` terminated with code `{}` and text `{}`",
                        exe, status.code().unwrap_or(0), err_text
                    );
                }
            }
            anyhow::bail!(
                "Process `{}` terminated with code `{}`",
                exe, status.code().unwrap_or(0)
            );
        }
        Ok(child)
    }

    pub fn connect(&self, settings: &ConnSettings) -> Result<()> {
        use std::net::ToSocketAddrs;

        let mut state = self.state.lock().unwrap();
        match *state {
            ConnState::Connecting =>
                return Err(Error::WrongSequense("Already connecting".to_string())),
            ConnState::Connected =>
                return Err(Error::WrongSequense("Already connected".to_string())),
            _ => {},
        }
        Self::set_new_conn_state(
            ConnState::Connecting,
            &mut state,
            &self.subscriptions.lock().unwrap()
        );
        let data = Arc::clone(&self.data);
        let state = Arc::clone(&self.state);
        let settings = settings.clone();
        let devices = Arc::clone(&self.devices);
        let subscriptions = Arc::clone(&self.subscriptions);
        std::thread::spawn(move || {
            let subscriptions = Arc::clone(&subscriptions);
            // Start indi drivers
            let mut indiserver = if !settings.remote {
                let start_result = Self::start_indi_server(
                    &settings.server_exe,
                    &settings.drivers,
                );
                match start_result {
                    Ok(child) => Some(child),
                    Err(err) => {
                        Self::set_new_conn_state(
                            ConnState::Error(err.to_string()),
                            &mut state.lock().unwrap(),
                            &subscriptions.lock().unwrap()
                        );
                        return;
                    }
                }
            } else {
                None
            };

            // Resolve host into IP addresses
            let mut addr = settings.host.clone();
            if !addr.contains(":") { addr += ":7624"; }
            let sock_addrs = match addr.to_socket_addrs() {
                Ok(sock_addrs) => sock_addrs,
                Err(err) => {
                    if let Some(indiserver) = &mut indiserver {
                        _ = indiserver.kill();
                        _ = indiserver.wait();
                    }
                    Self::set_new_conn_state(
                        ConnState::Error(err.to_string()),
                        &mut state.lock().unwrap(),
                        &subscriptions.lock().unwrap()
                    );
                    return;
                },
            };

            // Try to connect INDI server during 3 seconds
            let mut stream: Option<TcpStream> = None;
            'outer: for addr in sock_addrs {
                for _ in 0..3 {
                    let conn_try_res = TcpStream::connect_timeout(
                        &addr,
                        Duration::from_millis(1000)
                    );
                    if let Ok(res) = conn_try_res {
                        stream = Some(res);
                        break 'outer;
                    }
                }
            }

            // Failed to connect. Stop INDI server and exit
            let Some(stream) = stream else {
                if let Some(indiserver) = &mut indiserver {
                    _ = indiserver.kill();
                    _ = indiserver.wait();
                }
                Self::set_new_conn_state(
                    ConnState::Error(format!("Can't connect to {}", addr)),
                    &mut state.lock().unwrap(),
                    &subscriptions.lock().unwrap()
                );
                return;
            };

            // Subrscibers event thread for XML receiver
            let (events_sender, events_receiver) = mpsc::channel();
            let events_thread = {
                let subscriptions = Arc::clone(&subscriptions);
                std::thread::spawn(move || {
                    while let Ok(event) = events_receiver.recv() {
                        subscriptions.lock().unwrap().inform_all(event);
                    }
                })
            };

            // Start XML receiver thread
            let (xml_sender, xml_to_send) = mpsc::channel();
            let read_thread = {
                let xml_sender = xml_sender.clone();
                let stream = stream.try_clone().unwrap();
                let conn_state = Arc::clone(&state);
                std::thread::spawn(move || {
                    let mut receiver = XmlReceiver::new(
                        conn_state,
                        devices,
                        stream,
                        XmlSender { xml_sender },
                        settings.activate_all_devices,
                    );
                    receiver.main(events_sender);
                })
            };

            // Start XML sender thread
            let write_thread = {
                let stream = stream.try_clone().unwrap();
                std::thread::spawn(move || {
                    XmlSender::main(xml_to_send, stream);
                })
            };

            // take indiserver stderr
            let indiserver_stderr = indiserver
                .as_mut()
                .and_then(|v| v.stderr.take());

            // Assign active connection data
            *data.lock().unwrap() = Some(ActiveConnData{
                indiserver,
                tcp_stream: stream,
                xml_sender: XmlSender { xml_sender },
                events_thread,
                read_thread,
                write_thread,
            });

            // Read from indiserver's stderr and inform subscribers
            if let Some(mut indiserver_stderr) = indiserver_stderr {
                let mut stderr_data = Vec::new();
                let mut buffer = [0_u8; 256];
                while let Ok(read) = indiserver_stderr.read(&mut buffer) {
                    stderr_data.extend_from_slice(&buffer[..read]);
                    if read == 0 { break; }
                    // TODO: parce error text and inform subscribers
                }
            }
        });
        Ok(())
    }

    fn set_new_conn_state(
        new_state:    ConnState,
        state:        &mut ConnState,
        subscriptons: &Subscriptions
    ) {
        if new_state == *state { return; }
        *state = new_state;
        subscriptons.inform_all(Event::ConnChange(state.clone()));
    }

    pub fn disconnect_and_wait(&self) -> Result<()> {
        Self::set_new_conn_state(
            ConnState::Disconnecting,
            &mut self.state.lock().unwrap(),
            &self.subscriptions.lock().unwrap()
        );
        let mut data = self.data.lock().unwrap();
        if let Some(conn) = data.take() {
            drop(data);

            // Send exit command to xml_sender queue
            conn.xml_sender.send_exit_to_thread();

            // Shut down network connection
            _ = conn.tcp_stream.shutdown(std::net::Shutdown::Both);

            // Waiting for xml_sender and xml_reciever threads to terminate
            _ = conn.read_thread.join();
            _ = conn.write_thread.join();
            _ = conn.events_thread.join();

            // Killing indiserver
            if let Some(mut indiserver) = conn.indiserver {
                _ = indiserver.kill();
                _ = indiserver.wait();
            }

            // Clear devices properties
            self.devices.lock().unwrap().list.clear();

            // Setting new "disconnected" state
            Self::set_new_conn_state(
                ConnState::Disconnected,
                &mut self.state.lock().unwrap(),
                &self.subscriptions.lock().unwrap()
            );
        } else {
            return Err(Error::WrongSequense("Not connected".into()));
        }
        Ok(())
    }

    pub fn state(&self) -> ConnState {
        self.state.lock().unwrap().clone()
    }

    pub fn get_devices_list(&self) -> Vec<ExportDevice> {
        let devices = self.devices.lock().unwrap();
        devices.get_list_iter().collect()
    }

    pub fn get_devices_list_by_interface(&self, iface: DriverInterface) -> Vec<ExportDevice> {
        let devices = self.devices.lock().unwrap();
        devices
            .get_list_iter()
            .filter(|device| device.interface.intersects(iface))
            .collect()
    }

    pub fn get_driver_interface(&self, device_name: &str) -> Result<DriverInterface> {
        let devices = self.devices.lock().unwrap();
        devices.get_driver_interface(device_name)
    }

    pub fn get_properties_list(
        &self,
        device:        Option<&str>,
        changed_after: Option<u64>,
    ) -> Vec<Property> {
        let devices = self.devices.lock().unwrap();
        devices.get_properties_list(device, changed_after)
    }

    pub fn property_exists(
        &self,
        device_name: &str,
        prop_name: &str,
        elem_name: Option<&str>
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        devices.property_exists(device_name, prop_name, elem_name)
    }

    pub fn get_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        devices.get_switch_property(device_name, prop_name, elem_name)
    }

    pub fn get_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let elem = devices.get_num_property(device_name, prop_name, elem_name)?;
        Ok(elem.clone())
    }

    pub fn get_num_property_value(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<f64> {
        let devices = self.devices.lock().unwrap();
        let property = devices.get_num_property(device_name, prop_name, elem_name)?;
        Ok(property.value)
    }

    pub fn get_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<Arc<String>> {
        let devices = self.devices.lock().unwrap();
        devices.get_text_property(device_name, prop_name, elem_name)
    }

    fn with_conn_data_or_err(
        &self,
        fun: impl FnOnce(&ActiveConnData) -> Result<()>
    ) -> Result<()> {
        if let Some(ref conn_data) = *self.data.lock().unwrap() {
            fun(conn_data)
        } else {
            Err(Error::WrongSequense("Not connected".into()))
        }
    }

    pub fn command_get_properties(
        &self,
        device_name: Option<&str>,
        prop_name:   Option<&str>
    ) -> Result<()> {
        self.with_conn_data_or_err(move |data| {
            data.xml_sender.command_get_properties_impl(device_name, prop_name)
        })?;
        Ok(())
    }

    pub fn command_enable_blob(
        &self,
        device_name: &str,
        prop_name:   Option<&str>,
        mode:        BlobEnable
    ) -> Result<()> {
        self.with_conn_data_or_err(move |data| {
            data.xml_sender.command_enable_blob(device_name, prop_name, mode)
        })?;
        Ok(())
    }

    pub fn command_enable_device(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem = if enable {
            "CONNECT"
        } else {
            "DISCONNECT"
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CONNECTION",
            &[(elem, true)],
        )?;
        Ok(())
    }

    pub fn is_device_enabled(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "CONNECTION",
            "CONNECT"
        )
    }

    pub fn command_enable_all_devices(
        &self,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let dev_list = devices.get_list_iter().collect::<Vec<_>>();
        drop(devices);
        for dev in &dev_list {
            self.command_enable_device(
                &dev.name,
                enable,
                force_set,
                timeout_ms
            )?;
        }
        Ok(())
    }

    pub fn command_set_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, &str)]
    ) -> Result<()> {
        Devices::basic_check_device_and_prop_name(
            device_name,
            prop_name
        )?;
        self.devices.lock().unwrap().check_property_ok_for_writing(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Text),
            |index| elements[index].0,
            "Text",
        )?;
        self.with_conn_data_or_err(|data| {
            data.xml_sender.command_set_text_property(
                device_name,
                prop_name,
                elements
            )
        })?;
        Ok(())
    }

    pub fn command_set_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)]
    ) -> Result<()> {
        Devices::basic_check_device_and_prop_name(
            device_name,
            prop_name
        )?;
        self.devices.lock().unwrap().check_property_ok_for_writing(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Switch(_)),
            |index| elements[index].0,
            "Switch",
        )?;
        self.with_conn_data_or_err(|data| {
            data.xml_sender.command_set_switch_property(
                device_name,
                prop_name,
                elements
            )
        })?;
        Ok(())
    }

    pub fn check_switch_property_is_eq(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)]
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        for (elem_name, expected_value) in elements {
            let prop_value = devices.get_switch_property(
                device_name,
                prop_name,
                elem_name
            )?;
            if prop_value != *expected_value {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn command_set_switch_property_and_wait(
        &self,
        force_set:   bool,
        timeout_ms:  Option<u64>,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)],
    ) -> Result<()> {
        if !force_set
        && self.check_switch_property_is_eq(device_name, prop_name, elements)? {
            return Ok(());
        }
        self.command_set_switch_property(
            device_name,
            prop_name,
            elements
        )?;
        if let Some(mut timeout_ms) = timeout_ms {
            const TIME_QUANT_MS: u64 = 100;
            loop {
                let prop_eq = self.check_switch_property_is_eq(
                    device_name,
                    prop_name,
                    elements
                )?;
                if prop_eq || timeout_ms < TIME_QUANT_MS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(TIME_QUANT_MS));
                timeout_ms -= TIME_QUANT_MS;
                log::debug!("Waiting to set {}.{} property...", device_name, prop_name);
            }
        }
        Ok(())
    }

    pub fn command_set_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<()> {
        Devices::basic_check_device_and_prop_name(
            device_name,
            prop_name
        )?;
        self.devices.lock().unwrap().check_property_ok_for_writing(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Num),
            |index| elements[index].0,
            "Num",
        )?;
        self.with_conn_data_or_err(|data| {
            data.xml_sender.command_set_num_property(
                device_name,
                prop_name,
                elements
            )
        })?;
        Ok(())
    }

    fn f64_prop_values_equal(value1: f64, value2: f64) -> bool {
        if value1.is_nan() && value2.is_nan() {
            return true;
        }
        if value1.is_nan() != value2.is_nan() {
            return false;
        }
        if value1 == value2 {
            return true;
        }
        let aver = (value1.abs() + value2.abs()) / 2.0;
        let min_diff = aver / 1e6;
        f64::abs(value1 - value2) < min_diff
    }

    fn check_num_property_is_eq(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        for (elem_name, expected_value) in elements {
            let prop = devices.get_num_property(
                device_name,
                prop_name,
                elem_name
            )?;
            if !Self::f64_prop_values_equal(prop.value, *expected_value) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn command_set_num_property_and_wait(
        &self,
        force_set:   bool,
        timeout_ms:  Option<u64>,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)],
    ) -> Result<()> {
        if !force_set
        && self.check_num_property_is_eq(device_name, prop_name, elements)? {
            return Ok(());
        }
        self.command_set_num_property(
            device_name,
            prop_name,
            elements
        )?;
        if let Some(mut timeout_ms) = timeout_ms {
            const TIME_QUANT_MS: u64 = 100;
            loop {
                let prop_eq = self.check_num_property_is_eq(
                    device_name,
                    prop_name,
                    elements
                )?;
                if prop_eq || timeout_ms < TIME_QUANT_MS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(TIME_QUANT_MS));
                timeout_ms -= TIME_QUANT_MS;
                log::debug!("Waiting to set {}.{} property...", device_name, prop_name);
            }
        }
        Ok(())
    }

    fn is_device_support_any_of_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let result = devices.existing_prop_name(
            device_name,
            props
        );
        if let Err(Error::NoOnePropertyFound(_)) = result {
            Ok(false)
        } else if let Err(err) = result {
            Err(err)
        } else {
            Ok(true)
        }
    }

    pub fn device_get_prop_elem(
        &self,
        device_name: &str,
        props:       PropsNamePairs
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        let value = devices.get_num_property(
            device_name,
            prop_name, elem_name
        )?;
        Ok(value.clone())
    }

    pub fn device_set_any_of_num_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
        value:       f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        drop(devices);
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name,
            &[(elem_name, value)]
        )
    }

    pub fn set_any_of_switch_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
        value:       bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name,
            &[(elem_name, value)]
        )
    }

    pub fn device_get_num_prop(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        let property = devices.get_num_property(
            device_name,
            prop_name,
            elem_name
        )?;
        Ok(property.clone())
    }

    pub fn device_get_num_prop_value(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
    ) -> Result<f64> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        let property = devices.get_num_property(
            device_name,
            prop_name,
            elem_name
        )?;
        Ok(property.value)
    }

    pub fn device_get_any_of_swicth_props(
        &self,
        device_name: &str,
        props:       PropsNamePairs,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name(
            device_name,
            props
        )?;
        devices.get_switch_property(
            device_name,
            prop_name,
            elem_name
        )
    }

    // Crash device

    pub fn device_is_simu_chash_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_DEVICE_CRASH
        )
    }

    pub fn device_crash(
        &self,
        device_name: &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.set_any_of_switch_props(
            device_name,
            PROP_DEVICE_CRASH,
            true,
            force_set,
            timeout_ms
        )
    }

    // Device polling period

    pub fn device_is_polling_period_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(device_name, "POLLING_PERIOD", None)
    }

    pub fn device_get_polling_period(
        &self,
        device_name: &str,
    ) -> Result<usize> {
        let result = self.get_num_property(
            device_name,
            "POLLING_PERIOD",
            "PERIOD_MS"
        )?;
        Ok(result.value as usize)
    }

    pub fn device_set_polling_period(
        &self,
        device_name:    &str,
        polling_period: usize,
        force_set:      bool,
        timeout_ms:     Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "POLLING_PERIOD",
            &[("PERIOD_MS", polling_period as f64)]
        )
    }

    // Fast toggle capability

    pub fn camera_is_fast_toggle_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_FAST_TOGGLE",
            None
        )
    }

    pub fn camera_enable_fast_toggle(
        &self,
        device_name: &str,
        enabled:     bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_FAST_TOGGLE", &[
            ("INDI_ENABLED", enabled),
            ("INDI_DISABLED", !enabled)
        ])
    }

    pub fn camera_is_fast_toggle_enabled(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "CCD_FAST_TOGGLE",
            "INDI_ENABLED"
        )
    }

    pub fn camera_get_fast_frames_count_prop_info(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let property = devices.get_num_property(
            device_name,
            "CCD_FAST_COUNT",
            "FRAMES"
        )?;
        Ok(property.clone())
    }

    pub fn camera_set_fast_frames_count(
        &self,
        device_name: &str,
        frames:      usize,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_FAST_COUNT",
            &[("FRAMES", frames as f64)]
        )
    }

    // Exposure

    pub fn camera_get_exposure_prop_value(
        &self,
        device_name: &str,
        ccd:         CamCcd
    ) -> Result<NumPropValue> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        let property = devices.get_num_property(
            device_name,
            prop_name,
            prop_elem,
        )?;
        Ok(property.clone())
    }

    pub fn camera_is_exposure_property(
        prop_name: &str,
        elem_name: &str,
        ccd:       CamCcd
    ) -> bool {
        let (name, elem) = Self::exposure_prop_name(ccd);
        prop_name == name && elem_name == elem
    }

    pub fn camera_get_exposure(
        &self,
        device_name: &str,
        ccd:         CamCcd
    ) -> Result<f64> {
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        self.get_num_property_value(
            device_name,
            prop_name,
            prop_elem
        )
    }

    pub fn camera_start_exposure(
        &self,
        device_name: &str,
        ccd:         CamCcd,
        exposure:    f64
    ) -> Result<()> {
        let (prop_name, prop_elem) = Self::exposure_prop_name(ccd);
        self.command_set_num_property(
            device_name,
            prop_name,
            &[(prop_elem, exposure)]
        )
    }

    pub fn camera_abort_exposure(
        &self,
        device_name: &str,
        ccd:         CamCcd,
    ) -> Result<()> {
        let prop_name = match ccd {
            CamCcd::Primary   => "CCD_ABORT_EXPOSURE",
            CamCcd::Secondary => "GUIDER_ABORT_EXPOSURE",
        };
        self.command_set_switch_property(
            device_name,
            prop_name,
            &[("ABORT", true)]
        )
    }

    fn exposure_prop_name(ccd: CamCcd) -> (&'static str, &'static str) {
        match ccd {
            CamCcd::Primary   => ("CCD_EXPOSURE", "CCD_EXPOSURE_VALUE"),
            CamCcd::Secondary => ("GUIDER_EXPOSURE", "GUIDER_EXPOSURE_VALUE"),
        }
    }

    // Cooler

    pub fn camera_is_cooler_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(device_name, "CCD_COOLER", None)
    }

    pub fn camera_enable_cooler(
        &self,
        device_name: &str,
        enabled:     bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_COOLER", &[
            ("COOLER_ON",  enabled),
            ("COOLER_OFF", !enabled)
        ])
    }

    // CCD temperature

    pub fn camera_is_temperature_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_TEMPERATURE
        )
    }

    pub fn camera_get_temperature_prop_value(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            PROP_CAM_TEMPERATURE
        )
    }

    pub fn camera_set_temperature(
        &self,
        device_name: &str,
        temperature: f64
    ) -> Result<()> {
        self.device_set_any_of_num_props(
            device_name,
            PROP_CAM_TEMPERATURE,
            temperature,
            true,
            None
        )
    }

    // Camera cooling power

    pub fn camera_is_cooler_pwr_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_COOLING_PWR
        )
    }

    pub fn camera_get_cooler_pwr_str(
        &self,
        device_name: &str
    ) -> Result<String> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, prop_elem) = devices.existing_prop_name(device_name, PROP_CAM_COOLING_PWR)?;
        let property = devices.get_property(device_name, prop_name)?;
        let elem = property.get_elem(prop_elem).unwrap();
        Ok(elem.value.to_string())
    }

    pub fn camera_is_cooler_pwr_property(
        prop_name: &str,
        elem_name: &str
    ) -> bool {
        PROP_CAM_COOLING_PWR.iter().any(|(prop, elem)|
            *prop == prop_name && *elem == elem_name
        )
    }

    // Camera gain

    pub fn camera_is_gain_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_GAIN
        )
    }

    pub fn camera_get_gain_prop_value(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            PROP_CAM_GAIN
        )
    }

    pub fn camera_set_gain(
        &self,
        device_name: &str,
        gain:        f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.device_set_any_of_num_props(
            device_name,
            PROP_CAM_GAIN,
            gain,
            force_set,
            timeout_ms,
        )
    }

    // Camera offset

    pub fn camera_is_offset_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_OFFSET
        )
    }

    pub fn camera_get_offset_prop_value(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            PROP_CAM_OFFSET
        )
    }

    pub fn camera_set_offset(
        &self,
        device_name: &str,
        offset:      f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.device_set_any_of_num_props(
            device_name,
            PROP_CAM_OFFSET,
            offset,
            force_set,
            timeout_ms
        )
    }

    // Camera capture format

    pub fn camera_is_capture_format_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_CAPTURE_FORMAT",
            Some("INDI_RAW")
        )
    }

    pub fn camera_set_video_format(
        &self,
        device_name: &str,
        format:      CaptureFormat,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = match format {
            CaptureFormat::Rgb =>
                devices.existing_prop_name(device_name, PROP_CAM_VIDEO_FORMAT_RGB)?,
            CaptureFormat::Raw =>
                devices.existing_prop_name(device_name, PROP_CAM_VIDEO_FORMAT_RAW)?,
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    pub fn camera_set_capture_format(
        &self,
        device_name: &str,
        format:      CaptureFormat,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let cap_elem = match format {
            CaptureFormat::Rgb => "INDI_RGB",
            CaptureFormat::Raw => "INDI_RAW",
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_CAPTURE_FORMAT",
            &[(cap_elem, true)]
        )?;
        Ok(())
    }

    // Camera resolution

    pub fn camera_is_resolution_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_RESOLUTION",
            None
        )
    }

    pub fn camera_get_supported_resolutions(
        &self,
        device_name: &str,
    ) -> Result<Vec<Arc<String>>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt("CCD_RESOLUTION") else {
            return Ok(Vec::new());
        };
        Ok(prop.elements
            .iter()
            .map(|e| Arc::clone(&e.name))
            .collect())
    }

    pub fn camera_set_resolution(
        &self,
        device_name: &str,
        resolution:  &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_RESOLUTION",
            &[(resolution, true)]
        )
    }

    pub fn camera_get_resolution(
        &self,
        device_name: &str,
    ) -> Result<Option<Arc<String>>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt("CCD_RESOLUTION") else {
            return Ok(None);
        };
        Ok(prop.elements
            .iter()
            .find(|e| e.value.to_i32().unwrap_or(0) != 0)
            .map(|e| Arc::clone(&e.name))
        )
    }

    pub fn camera_select_max_resolution(
        &self,
        device_name: &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let items = self.camera_get_supported_resolutions(device_name)?;
        if items.is_empty() { return Ok(false); }
        let values = items.iter().map(|s|{
            let mut splitted = s.split(|c| c == 'x' || c == 'X');
            let width: usize = splitted.next().map(|s| s.trim().parse().unwrap_or(0)).unwrap_or(0);
            let height: usize = splitted.next().map(|s| s.trim().parse().unwrap_or(0)).unwrap_or(0);
            (width + height, s)
        });
        let Some(max) = values.max_by_key(|item| item.0) else { return Ok(false); };
        self.camera_set_resolution(device_name, max.1, force_set, timeout_ms)?;
        Ok(true)
    }

    // Camera frame size and information

    pub fn camera_get_pixel_size_um(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(f64/*x*/, f64/*y*/)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_info_prop_name(cam_ccd);
        let size_x = devices.get_num_property(device_name, prop_name, "CCD_PIXEL_SIZE_X")?.value;
        let size_y = devices.get_num_property(device_name, prop_name, "CCD_PIXEL_SIZE_Y")?.value;
        Ok((size_x, size_y))
    }

    // CCD_FRAME

    pub fn camera_is_frame_supported(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let res = devices.get_property(
            device_name,
            Self::ccd_frame_prop_name(cam_ccd)
        );
        match res {
            Err(e @ Error::DeviceNotExists(_)) => Err(e),
            Err(_) => Ok(false),
            Ok(s) => Ok(s.permition != PropPermition::RO),
        }
    }

    pub fn camera_set_frame_size(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
        x:           usize,
        y:           usize,
        width:       usize,
        height:      usize,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            Self::ccd_frame_prop_name(cam_ccd), &[
            ("X",      x as f64),
            ("Y",      y as f64),
            ("WIDTH",  width as f64),
            ("HEIGHT", height as f64),
        ])
    }

    pub fn camera_get_max_frame_size(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_info_prop_name(cam_ccd);
        let width = devices.get_num_property(device_name, prop_name, "CCD_MAX_X")?.value;
        let height = devices.get_num_property(device_name, prop_name, "CCD_MAX_Y")?.value;
        Ok((width as usize, height as usize))
    }

    fn ccd_frame_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Primary => "CCD_FRAME",
            CamCcd::Secondary => "GUIDER_FRAME",
        }
    }

    fn ccd_info_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Primary => "CCD_INFO",
            CamCcd::Secondary => "GUIDER_INFO",
        }
    }

    // Camera binning

    pub fn camera_is_binning_supported(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            Self::ccd_bin_prop_name(cam_ccd),
            None
        )
    }

    pub fn camera_get_max_binning(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_bin_prop_name(cam_ccd);
        if devices.property_exists(device_name, prop_name, None)? {
            let max_hor = devices.get_num_property(device_name, prop_name, "HOR_BIN")?.max;
            let max_vert = devices.get_num_property(device_name, prop_name, "VER_BIN")?.max;
            Ok((max_hor as usize, max_vert as usize))
        } else {
            Ok((1, 1))
        }
    }

    pub fn camera_set_binning(
        &self,
        device_name:    &str,
        cam_ccd:        CamCcd,
        horiz_binnging: usize,
        vert_binnging:  usize,
        force_set:      bool,
        timeout_ms:     Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            Self::ccd_bin_prop_name(cam_ccd), &[
            ("HOR_BIN", horiz_binnging as f64),
            ("VER_BIN", vert_binnging as f64),
        ])
    }

    pub fn camera_get_binning(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let prop_name = Self::ccd_bin_prop_name(cam_ccd);
        if devices.property_exists(device_name, prop_name, None)? {
            let max_hor = devices.get_num_property(device_name, prop_name, "HOR_BIN")?.value;
            let max_vert = devices.get_num_property(device_name, prop_name, "VER_BIN")?.value;
            Ok((max_hor as usize, max_vert as usize))
        } else {
            Ok((1, 1))
        }
    }


    pub fn camera_is_binning_mode_supported(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            Self::ccd_bin_mode_prop_name(cam_ccd),
            None
        )
    }

    pub fn camera_set_binning_mode(
        &self,
        device_name:  &str,
        binning_mode: BinningMode,
        force_set:    bool,
        timeout_ms:   Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = match binning_mode {
            BinningMode::Add =>
                devices.existing_prop_name(device_name, PROP_CAM_BIN_ADD)?,
            BinningMode::Avg =>
                devices.existing_prop_name(device_name, PROP_CAM_BIN_AVG)?,
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    pub fn camera_get_binning_mode(
        &self,
        device_name: &str,
    ) -> Result<Option<BinningMode>> {
        let is_add_mode = self.device_get_any_of_swicth_props(
            device_name,
            PROP_CAM_BIN_ADD
        )?;
        let is_avg_mode = self.device_get_any_of_swicth_props(
            device_name,
            PROP_CAM_BIN_AVG
        )?;
        if is_add_mode && !is_avg_mode {
            Ok(Some(BinningMode::Add))
        } else if !is_add_mode && is_avg_mode {
            Ok(Some(BinningMode::Avg))
        } else {
            Ok(None)
        }
    }

    fn ccd_bin_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Primary => "CCD_BINNING",
            CamCcd::Secondary => "GUIDER_BINNING",
        }
    }

    fn ccd_bin_mode_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Primary => "CCD_BINNING_MODE",
            CamCcd::Secondary => "GUIDER_BINNING_MODE",
        }
    }

    // Frame type (light, dark etc)

    pub fn camera_set_frame_type(
        &self,
        device_name: &str,
        cam_ccd:     CamCcd,
        frame_type:  FrameType,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem_name = match frame_type {
            FrameType::Light => "FRAME_LIGHT",
            FrameType::Flat => "FRAME_FLAT",
            FrameType::Dark => "FRAME_DARK",
            FrameType::Bias => "FRAME_BIAS",
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            Self::ccd_frame_type_prop_name(cam_ccd),
            &[(elem_name, true)]
        )
    }

    fn ccd_frame_type_prop_name(cam_ccd: CamCcd) -> &'static str {
        match cam_ccd {
            CamCcd::Primary => "CCD_FRAME_TYPE",
            CamCcd::Secondary => "GUIDER_FRAME_TYPE",
        }
    }


    // Camera fan

    pub fn camera_is_fan_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_FAN_ON
        )
    }

    pub fn camera_control_fan(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = if enable {
            devices.existing_prop_name(device_name, PROP_CAM_FAN_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_FAN_OFF)?
        };
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    // Camera window heater

    pub fn camera_is_heater_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_HEAT_ON
        )
    }

    pub fn camera_is_heater_property(
        prop_name: &str
    ) -> bool {
        PROP_CAM_HEAT_ON.iter().any(|(prop, _)|
            *prop == prop_name
        )
    }

    pub fn camera_get_heater_items(
        &self,
        device_name: &str
    ) -> Result<Option<Vec<(Arc<String>, String)>>> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, _) = devices.existing_prop_name(
            device_name,
            PROP_CAM_HEAT_ON
        )?;
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt(prop_name) else {
            return Ok(None);
        };
        Ok(Some(prop.elements
            .iter()
            .map(|e| (Arc::clone(&e.name), e.label.as_ref().unwrap_or(&e.name.clone()).to_string()))
            .collect()
        ))
    }

    pub fn camera_control_heater(
        &self,
        device_name: &str,
        value:       &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, _) = devices.existing_prop_name(
            device_name,
            PROP_CAM_HEAT_ON
        )?;
        drop(devices);
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop_name, &[(value, true)]
        )?;
        Ok(())
    }


    // Camera low noise mode

    pub fn camera_is_low_noise_ctrl_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_LOW_NOISE_ON
        )
    }

    pub fn camera_control_low_noise(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop, elem) = if enable {
            devices.existing_prop_name(device_name, PROP_CAM_LOW_NOISE_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_LOW_NOISE_OFF)?
        };
        drop(devices);

        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            prop,
            &[(elem, true)]
        )?;
        Ok(true)
    }

    // Camera's telescope info

    pub fn camera_is_telescope_info_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(device_name, "SCOPE_INFO", None)
    }

    pub fn camera_set_telescope_info(
        &self,
        device_name:  &str,
        focal_length: f64,
        aperture:     f64,
        force_set:    bool,
        timeout_ms:   Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "SCOPE_INFO",
            &[
                ("FOCAL_LENGTH", focal_length),
                ("APERTURE",     aperture),
            ]
        )?;
        Ok(())
    }

    // Focuser absolute position

    pub fn focuser_get_abs_value_prop_info(
        &self,
        device_name: &str
    ) -> Result<NumPropValue> {
        self.device_get_num_prop(
            device_name,
            &[("ABS_FOCUS_POSITION", "FOCUS_ABSOLUTE_POSITION")]
        )
    }


    pub fn focuser_get_abs_value(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "ABS_FOCUS_POSITION",
            "FOCUS_ABSOLUTE_POSITION"
        )
    }

    pub fn focuser_set_abs_value(
        &self,
        device_name: &str,
        value:       f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "ABS_FOCUS_POSITION",
            &[("FOCUS_ABSOLUTE_POSITION", value)]
        )
    }

    pub fn mount_abort_motion(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_ABORT_MOTION",
            &[("ABORT", true)]
        )
    }

    pub fn mount_get_eq_dec(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "EQUATORIAL_EOD_COORD",
            "DEC"
        )
    }

    pub fn mount_get_eq_ra(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "EQUATORIAL_EOD_COORD",
            "RA"
        )
    }

    pub fn mount_get_eq_ra_and_dec(&self, device_name: &str) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let ra = devices.get_num_property(device_name, "EQUATORIAL_EOD_COORD", "RA")?.value;
        let dec = devices.get_num_property(device_name, "EQUATORIAL_EOD_COORD", "DEC")?.value;
        Ok((ra, dec))
    }

    pub fn mount_set_eq_coord(
        &self,
        device_name: &str,
        ra: f64,
        dec: f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "EQUATORIAL_EOD_COORD", &[
            ("RA",  ra),
            ("DEC", dec),
        ])
    }

    pub fn mount_start_move_north(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_NS",
            &[("MOTION_NORTH", true)]
        )
    }

    pub fn mount_start_move_south(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_NS",
            &[("MOTION_SOUTH", true)]
        )
    }

    pub fn mount_start_move_west(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_WE",
            &[("MOTION_WEST", true)]
        )
    }

    pub fn mount_start_move_east(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_WE",
            &[("MOTION_EAST", true)]
        )
    }

    pub fn mount_stop_move(&self, device_name: &str) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_NS", &[
            ("MOTION_NORTH", false),
            ("MOTION_SOUTH", false),
        ])?;

        self.command_set_switch_property(
            device_name,
            "TELESCOPE_MOTION_WE", &[
            ("MOTION_WEST", false),
            ("MOTION_EAST", false)
        ])?;

        Ok(())
    }

    pub fn mount_reverse_motion(
        &self,
        device_name: &str,
        reverse_ns:  bool,
        reverse_we:  bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_REVERSE_MOTION", &[
            ("REVERSE_NS", reverse_ns),
            ("REVERSE_WE", reverse_we),
        ])
    }

    pub fn mount_get_slew_speed_list(
        &self,
        device_name: &str
    ) -> Result<Vec<(Arc<String>, Option<Arc<String>>)>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.find_by_name_res(device_name)?;
        let Some(prop) = device.get_property_opt("TELESCOPE_SLEW_RATE") else {
            return Ok(Vec::new());
        };
        let result = prop.elements
            .iter()
            .map(|e| (Arc::clone(&e.name), e.label.clone()))
            .collect();
        Ok(result)
    }

    pub fn mount_set_slew_speed(
        &self,
        device_name: &str,
        speed_name:  &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_SLEW_RATE",
            &[(speed_name, true)]
        )
    }

    pub fn mount_get_tracking(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "TELESCOPE_TRACK_STATE",
            "TRACK_ON"
        )
    }

    pub fn mount_set_tracking(
        &self,
        device_name: &str,
        tracking:    bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem_name = if tracking {
            "TRACK_ON"
        } else {
            "TRACK_OFF"
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_TRACK_STATE",
            &[(elem_name, true)]
        )
    }

    pub fn mount_get_parked(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "TELESCOPE_PARK",
            "PARK"
        )
    }

    pub fn mount_set_parked(
        &self,
        device_name: &str,
        parked:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let elem_name = if parked {
            "PARK"
        } else {
            "UNPARK"
        };
        self.command_set_switch_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "TELESCOPE_PARK",
            &[(elem_name, true)]
        )
    }

    pub fn mount_get_timed_guide_max(
        &self,
        device_name: &str
    ) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let ns_items = &devices.get_property(device_name, "TELESCOPE_TIMED_GUIDE_NS")?.elements;
        let we_items = &devices.get_property(device_name, "TELESCOPE_TIMED_GUIDE_WE")?.elements;
        if ns_items.is_empty() || we_items.is_empty() {
            return Err(Error::Internal("Wrong prop elem len".into()));
        }
        let PropValue::Num(NumPropValue{max: ns_max, ..}) = &ns_items[0].value else {
            return Err(Error::Internal("Wrong prop elem type".into()));
        };
        let PropValue::Num(NumPropValue{max: we_max, ..}) = &we_items[0].value else {
            return Err(Error::Internal("Wrong prop elem type".into()));
        };
        Ok((*ns_max, *we_max))
    }

    pub fn mount_timed_guide(
        &self,
        device_name: &str,
        north_south: f64,
        west_east:   f64,
    ) -> Result<()> {
        let (north, south) = if north_south > 0.0 {
            (north_south, 0.0)
        } else {
            (0.0, -north_south)
        };
        let (west, east) = if west_east > 0.0 {
            (west_east, 0.0)
        } else {
            (0.0, -west_east)
        };
        self.command_set_num_property(
            device_name,
            "TELESCOPE_TIMED_GUIDE_NS", &[
            ("TIMED_GUIDE_N", north),
            ("TIMED_GUIDE_S", south),
        ])?;
        self.command_set_num_property(
            device_name,
            "TELESCOPE_TIMED_GUIDE_WE",&[
            ("TIMED_GUIDE_W", west),
            ("TIMED_GUIDE_E", east),
        ])?;
        Ok(())
    }

    pub fn mount_is_guide_rate_supported(
        &self,
        device_name: &str
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "GUIDE_RATE",
            None
        )
    }

    pub fn mount_get_guide_rate_prop_data(
        &self,
        device_name: &str
    ) -> Result<Property> {
        let devices = self.devices.lock().unwrap();
        let property = devices.get_property(device_name, "GUIDE_RATE")?;
        Ok(property.clone())
    }

    pub fn mount_get_guide_rate_ns(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "GUIDE_RATE",
            "GUIDE_RATE_NS"
        )
    }

    pub fn mount_get_guide_rate_we(&self, device_name: &str) -> Result<f64> {
        self.get_num_property_value(
            device_name,
            "GUIDE_RATE",
            "GUIDE_RATE_WE"
        )
    }

    pub fn mount_get_guide_rate(
        &self,
        device_name: &str,
    ) -> Result<(f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let ns = devices.get_num_property(device_name, "GUIDE_RATE", "GUIDE_RATE_NS")?.value;
        let we = devices.get_num_property(device_name, "GUIDE_RATE", "GUIDE_RATE_WE")?.value;
        Ok((ns, we))
    }

    pub fn mount_set_guide_rate(
        &self,
        device_name: &str,
        rate_ns:     f64,
        rate_we:     f64,
        force_set:   bool,
        timeout_ms:  Option<u64>
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "GUIDE_RATE", &[
            ("GUIDE_RATE_NS", rate_ns),
            ("GUIDE_RATE_WE", rate_we),
        ])?;
        Ok(())
    }

    pub fn get_geo_lat_long_elev(&self, device_name: &str) -> Result<(f64, f64, f64)> {
        let devices = self.devices.lock().unwrap();
        let latitude = devices.get_num_property(device_name, "GEOGRAPHIC_COORD", "LAT")?.value;
        let longitude = devices.get_num_property(device_name, "GEOGRAPHIC_COORD", "LONG")?.value;
        let elevation = devices.get_num_property(device_name, "GEOGRAPHIC_COORD", "ELEV")?.value;
        Ok((latitude, longitude, elevation))
    }
}

struct XmlSender {
    xml_sender: mpsc::Sender<XmlSenderItem>,
}

impl XmlSender {
    fn main(receiver: mpsc::Receiver<XmlSenderItem>, tcp_stream: TcpStream) {
        fn send_xml<T: Write>(
            writer: &mut T,
            xml:    String
        ) -> std::result::Result<(), std::io::Error> {
            writer.write_all(xml.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            Ok(())
        }
        let mut writer = BufWriter::new(tcp_stream);
        while let Ok(item) = receiver.recv() {
            match item {
                XmlSenderItem::Xml(xml) => {
                    let res = send_xml(&mut writer, xml);
                    if res.is_err() { break; }
                },
                XmlSenderItem::Exit => {
                    break;
                }
            }
        }
    }

    fn send_exit_to_thread(&self) {
        _ = self.xml_sender.send(XmlSenderItem::Exit);
    }

    fn send_xml(
        &self,
        xml: &xmltree::Element
    ) -> Result<()> {
        let mut mem_buf = Cursor::new(Vec::new());
        let mut xml_conf = xmltree::EmitterConfig::new();
        xml_conf.write_document_declaration = false;
        xml.write_with_config(&mut mem_buf, xml_conf)
            .map_err(|e| Error::Internal(e.to_string()))?;
        let xml_text = String::from_utf8(mem_buf.into_inner())
            .map_err(|e| Error::Internal(e.to_string()))?;
        if log::log_enabled!(log::Level::Trace) {
            log::trace!("indi_api: outgoing xml =\n{}", xml_text);
        }
        self.xml_sender.send(XmlSenderItem::Xml(xml_text))
            .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    fn command_set_property_impl<'a>(
        &self,
        device_name:    &str,
        prop_name:      &str,
        command_tag:    &str,
        elem_tag:       &str,
        elem_count:     usize,
        elem_get_name:  impl Fn(usize) -> &'a str,
        elem_get_value: impl Fn(usize) -> String,
    ) -> Result<()> {
        // Send XML with new property data
        let mut xml_command = xmltree::Element::new(command_tag);
        xml_command.attributes.insert("device".to_string(), device_name.to_string());
        xml_command.attributes.insert("name".to_string(), prop_name.to_string());
        for index in 0..elem_count {
            let mut xml_elem = xmltree::Element::new(elem_tag);
            xml_elem.attributes.insert("name".to_string(), elem_get_name(index).to_string());
            xml_elem.children.push(xmltree::XMLNode::Text(elem_get_value(index)));
            xml_command.children.push(xmltree::XMLNode::Element(xml_elem));
        }
        self.send_xml(&xml_command)?;
        Ok(())
    }

    fn command_get_properties_impl(
        &self,
        device_name: Option<&str>,
        prop_name:   Option<&str>
    ) -> Result<()> {
        let mut xml_command = xmltree::Element::new("getProperties");
        xml_command.attributes.insert("version".to_string(), "1.7".to_string());
        if let Some(device_name) = device_name {
            xml_command.attributes.insert("device".to_string(), device_name.to_string());
        }
        if let Some(prop_name) = prop_name {
            xml_command.attributes.insert("name".to_string(), prop_name.to_string());
        }
        self.send_xml(&xml_command)?;
        Ok(())
    }

    fn command_enable_blob(
        &self,
        device_name: &str,
        prop_name:   Option<&str>,
        mode:        BlobEnable
    ) -> Result<()> {
        let mut xml_command = xmltree::Element::new("enableBLOB");
        xml_command.attributes.insert("device".to_string(), device_name.to_string());
        if let Some(prop_name) = prop_name {
            xml_command.attributes.insert("name".to_string(), prop_name.to_string());
        }
        let mode_str = match mode {
            BlobEnable::Never => "Never",
            BlobEnable::Also => "Also",
            BlobEnable::Only => "Only",
        };
        xml_command.children.push(xmltree::XMLNode::Text(mode_str.to_string()));
        self.send_xml(&xml_command)?;
        Ok(())
    }

    fn command_set_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, &str)]
    ) -> Result<()> {
        self.command_set_property_impl(
            device_name,
            prop_name,
            "newTextVector",
            "oneText",
            elements.len(),
            |index| elements[index].0,
            |index| elements[index].1.to_string(),
        )
    }

    fn command_set_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, bool)]
    ) -> Result<()> {
        self.command_set_property_impl(
            device_name,
            prop_name,
            "newSwitchVector",
            "oneSwitch",
            elements.len(),
            |index| elements[index].0,
            |index| if elements[index].1 { "On".to_string() } else { "Off".to_string() },
        )
    }

    fn command_set_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<()> {
        self.command_set_property_impl(
            device_name,
            prop_name,
            "newNumberVector",
            "oneNumber",
            elements.len(),
            |index| elements[index].0,
            |index| elements[index].1.to_string(),
        )
    }

    fn command_enable_device(
        &self,
        device_name: &str,
        enable:      bool,
    ) -> Result<()> {
        let elem = if enable {
            "CONNECT"
        } else {
            "DISCONNECT"
        };
        self.command_set_switch_property(
            device_name,
            "CONNECTION",
            &[(elem, true)]
        )?;
        Ok(())
    }
}


enum XmlReceiverState {
    Undef,
    WaitForDevicesList,
    WaitForDevicesOn,
    Working
}

struct XmlReceiver {
    conn_state:    Arc<Mutex<ConnState>>,
    devices:       Arc<Mutex<Devices>>,
    stream:        TcpStream,
    reader:        XmlStreamReader,
    xml_sender:    XmlSender,
    state:         XmlReceiverState,
    activate_devs: bool,
}

impl XmlReceiver {
    fn new(
        conn_state:    Arc<Mutex<ConnState>>,
        devices:       Arc<Mutex<Devices>>,
        stream:        TcpStream,
        xml_sender:    XmlSender,
        activate_devs: bool,
    ) -> Self {
        Self {
            conn_state,
            devices,
            stream,
            reader: XmlStreamReader::new(),
            xml_sender,
            state: XmlReceiverState::Undef,
            activate_devs,
        }
    }

    fn main(&mut self, events_sender: mpsc::Sender<Event>) {
        self.stream.set_read_timeout(Some(Duration::from_millis(1000))).unwrap(); // TODO: check error

        self.xml_sender.command_get_properties_impl(None, None).unwrap(); // TODO: check error
        self.state = XmlReceiverState::WaitForDevicesList;

        let mut timeout_processed = false;
        loop {
            let xml_res = self.reader.receive_xml(&mut self.stream);
            match xml_res {
                Ok(XmlStreamReaderResult::BlobBegin { device_name, prop_name, elem_name, .. }) => {
                    let device_name = Arc::new(device_name);
                    let prop_name = Arc::new(prop_name);
                    let elem_name = Arc::new(elem_name);
                    self.notify_subcribers_about_blob_start(
                        &device_name,
                        &prop_name,
                        &elem_name,
                        &events_sender
                    );
                }
                Ok(XmlStreamReaderResult::Xml{ xml, blobs }) => {
                    if log::log_enabled!(log::Level::Trace) {
                        log::trace!("indi_api: incoming xml =\n{}", xml);
                    }
                    timeout_processed = false;
                    let process_xml_res = self.process_xml(&xml, blobs, &events_sender);
                    if let Err(err) = process_xml_res {
                        log::error!("indi_api: '{}' for XML\n{}", err, xml);
                    } else {
                        let mut state = self.conn_state.lock().unwrap();
                        if *state == ConnState::Connecting {
                            *state = ConnState::Connected;
                            drop(state);
                            events_sender.send(Event::ConnChange(
                                ConnState::Connected
                            )).unwrap();
                        }
                    }
                }
                Ok(XmlStreamReaderResult::Disconnected) => {
                    log::debug!("indi_api: Disconnected");
                    break;
                }
                Ok(XmlStreamReaderResult::TimeOut) => {
                    if !timeout_processed {
                        timeout_processed = true;
                        events_sender.send(Event::ReadTimeOut).unwrap();
                        let to_res = self.process_time_out();
                        if let Err(err) = to_res {
                            log::error!("indi_api: {}", err.to_string());
                        }
                    }
                }
                Err(err) => {
                    self.reader.recover_after_error();
                    log::error!("indi_api: {}", err.to_string());
                },
            }
        }
    }

    fn notify_subcribers_about_new_prop(
        &self,
        timestamp:      Option<DateTime<Utc>>,
        device_name:    &Arc<String>,
        prop_name:      &Arc<String>,
        changed_values: Vec<(Arc<String>, PropValue)>,
        events_sender:  &mpsc::Sender<Event>
    ) {
        for (name, value) in changed_values {
            let prop_change_value = PropChangeValue {
                elem_name:  Arc::clone(&name),
                prop_value: value.clone(),
            };
            events_sender.send(Event::PropChange(Arc::new(PropChangeEvent {
                timestamp,
                device_name: Arc::clone(device_name),
                prop_name:   Arc::clone(prop_name),
                change:      PropChange::New(prop_change_value),
            }))).unwrap();

            if prop_name.as_str() == "DRIVER_INFO"
            && name.as_str() == "DRIVER_INTERFACE" {
                let flag_bits = value.to_i32().unwrap_or(0);
                let interface = DriverInterface::from_bits_truncate(flag_bits as u32);
                let event_data = NewDeviceEvent {
                    device_name: Arc::clone(device_name),
                    interface,
                    timestamp,
                };
                events_sender.send(Event::NewDevice(event_data)).unwrap();
            }
        }
    }

    fn notify_subcribers_about_prop_change(
        &self,
        timestamp:      Option<DateTime<Utc>>,
        device_name:    &Arc<String>,
        prop_name:      &Arc<String>,
        prev_state:     PropState,
        new_state:      PropState,
        changed_values: Vec<(Arc<String>, PropValue)>,
        events_sender:  &mpsc::Sender<Event>
    ) {
        for (name, prop_value) in changed_values {
            let value = PropChangeValue {
                elem_name:  Arc::clone(&name),
                prop_value: prop_value.clone(),
            };
            let change = PropChange::Change{
                value,
                prev_state: prev_state.clone(),
                new_state: new_state.clone(),
            };
            events_sender.send(Event::PropChange(Arc::new(PropChangeEvent {
                timestamp,
                device_name: Arc::clone(device_name),
                prop_name: Arc::clone(prop_name),
                change,
            }))).unwrap();

            if prop_name.as_str() == "CONNECTION"
            && name.as_str() == "CONNECT" {
                let connected = prop_value.to_bool().unwrap_or(false);
                let devices = self.devices.lock().unwrap();
                let interface = devices.get_driver_interface(&device_name);
                drop(devices);

                let event_data = DeviceConnectEvent {
                    device_name: Arc::clone(device_name),
                    interface: interface.unwrap_or(DriverInterface::empty()),
                    timestamp,
                    connected,
                };

                events_sender.send(Event::DeviceConnected(
                    Arc::new(event_data)
                )).unwrap();
            }
        }
    }

    fn notify_subcribers_about_prop_delete(
        &self,
        time:          Option<DateTime<Utc>>,
        device_name:   &Arc<String>,
        prop_name:     &Arc<String>,
        events_sender: &mpsc::Sender<Event>
    ) {
        events_sender.send(Event::PropChange(Arc::new(PropChangeEvent {
            timestamp:   time,
            device_name: Arc::clone(device_name),
            prop_name:   Arc::clone(prop_name),
            change:      PropChange::Delete,
        }))).unwrap();
    }

    fn notify_subcribers_about_device_delete(
        &self,
        time:          Option<DateTime<Utc>>,
        device_name:   &Arc<String>,
        events_sender: &mpsc::Sender<Event>,
        drv_interface: DriverInterface,
    ) {
        events_sender.send(Event::DeviceDelete(Arc::new(DeviceDeleteEvent {
            timestamp:   time,
            device_name: Arc::clone(device_name),
            drv_interface
        }))).unwrap();
    }

    fn notify_subcribers_about_message(
        &self,
        timestamp:     Option<DateTime<Utc>>,
        device_name:   &Arc<String>,
        message:       &Arc<String>,
        events_sender: &mpsc::Sender<Event>
    ) {
        events_sender.send(Event::Message(Arc::new(MessageEvent {
            timestamp,
            device_name: Arc::clone(device_name),
            text:        Arc::clone(message),
        }))).unwrap();
    }

    fn notify_subcribers_about_blob_start(
        &self,
        device_name:   &Arc<String>,
        prop_name:     &Arc<String>,
        elem_name:     &Arc<String>,
        events_sender: &mpsc::Sender<Event>
    ) {
        events_sender.send(Event::BlobStart(Arc::new(BlobStartEvent {
            device_name: Arc::clone(device_name),
            prop_name:   Arc::clone(prop_name),
            elem_name:   Arc::clone(elem_name),
        }))).unwrap();
    }

    fn process_xml(
        &mut self,
        xml_text:      &str,
        blobs:         Vec<XmlStreamReaderBlob>,
        events_sender: &mpsc::Sender<Event>
    ) -> anyhow::Result<()> {
        let mut xml_elem = xmltree::Element::parse(xml_text.as_bytes())?;
        if xml_elem.name.starts_with("def") { // defXXXXVector
            // New property from INDI server
            let device_name = xml_elem.attr_string_or_err("device")?;
            if device_name.is_empty() {
                anyhow::bail!("Empty device name");
            }
            let mut devices = self.devices.lock().unwrap();
            let change_id = devices.change_id;
            let device_name = Arc::new(device_name);
            let device = if let Some(device) = devices.find_by_name_opt_mut(&device_name) {
                device
            } else {
                devices.list.push(Device::new(&device_name));
                devices.list.last_mut().unwrap()
            };
            let prop_name = xml_elem.attr_string_or_err("name")?;
            if device.get_property_opt(&prop_name).is_some() {
                // simple ignore if INDI server sends defXXXXVector command
                // for already existing property
                return Ok(());
            }
            let timestamp = xml_elem.attr_time("timestamp");
            let mut property = Property::new_from_xml(
                xml_elem,
                &device.name,
                &prop_name
            )?;
            let values = property.get_values();
            property.change_id = change_id;
            let prop_name = Arc::clone(&property.name);
            device.props.push(property);

            devices.change_id += 1;
            drop(devices);
            self.notify_subcribers_about_new_prop(
                timestamp,
                &device_name,
                &prop_name,
                values,
                events_sender,
            );
        } else if xml_elem.name.starts_with("set") { // setXXXXVector
            // Changed property data from INDI server
            let device_name = xml_elem.attr_string_or_err("device")?;
            let prop_name = xml_elem.attr_string_or_err("name")?;
            let timestamp = xml_elem.attr_time("timestamp");
            let mut devices = self.devices.lock().unwrap();
            devices.change_id += 1;
            let change_id = devices.change_id;
            let Some(device) = devices.find_by_name_opt_mut(&device_name) else {
                anyhow::bail!(Error::DeviceNotExists(device_name));
            };
            let device_name = Arc::clone(&device.name);
            let Some(property) = device.get_property_opt_mut(&prop_name) else {
                anyhow::bail!(Error::PropertyNotExists(
                    device_name.to_string(),
                    prop_name
                ));
            };
            property.change_id = change_id;
            let prev_state = property.state.clone();
            let (prop_changed, mut values) = property.update_data_from_xml_and_return_changes(
                &mut xml_elem,
                blobs,
                &device_name,
                &prop_name,
            )?;
            if prop_changed {
                let prop_name = Arc::clone(&property.name);
                let cur_state = property.state.clone();
                if values.is_empty() && prev_state != cur_state {
                    values = property.get_values();
                }
                drop(devices);
                self.notify_subcribers_about_prop_change(
                    timestamp,
                    &device_name,
                    &prop_name,
                    prev_state,
                    cur_state,
                    values,
                    events_sender,
                );
            }
        } else if xml_elem.name == "delProperty" { // delProperty
            let device_name = xml_elem.attr_string_or_err("device")?;
            let timestamp = xml_elem.attr_time("timestamp");
            let mut devices = self.devices.lock().unwrap();
            if let Some(prop_name) = xml_elem.attributes.remove("name") {
                let Some(device) = devices.find_by_name_opt_mut(&device_name) else {
                    anyhow::bail!(Error::DeviceNotExists(device_name));
                };
                let dev_name_arc = Arc::clone(&device.name);
                let removed_prop = device.remove_property(&prop_name)
                    .ok_or_else(
                        || Error::PropertyNotExists(device_name.clone(), prop_name.clone())
                    )?;
                self.notify_subcribers_about_prop_delete(
                    timestamp,
                    &dev_name_arc,
                    &removed_prop.name,
                    events_sender
                );
            } else {
                let drv_interface = devices.get_driver_interface(&device_name)?;
                let Some(removed) = devices.remove(&device_name) else {
                    anyhow::bail!(Error::DeviceNotExists(device_name));
                };
                self.notify_subcribers_about_device_delete(
                    timestamp,
                    &removed.name,
                    events_sender,
                    drv_interface
                );
            }
        // message
        } else if xml_elem.name == "message" {
            let message = xml_elem.attr_string_or_err("message")?;
            let device = xml_elem.attr_string_or_err("device")?;
            let timestamp = xml_elem.attr_time("timestamp");
            let device = Arc::new(device);
            let message = Arc::new(message);
            self.notify_subcribers_about_message(timestamp, &device, &message, events_sender);
        } else if !matches!(xml_elem.name.as_str(), "newTextVector"|"newNumberVector"|"newSwitchVector"|"newBLOBVector") {
            log::error!("Unknown tag: {}, xml=\n{}", xml_elem.name, xml_text);
        }
        Ok(())
    }

    fn process_time_out(&mut self) -> anyhow::Result<()> {
        match self.state {
            XmlReceiverState::WaitForDevicesList => {
                if self.activate_devs {
                    let devices = self.devices.lock().unwrap();
                    let names = devices.get_names();
                    for device_name in names {
                        self.xml_sender.command_enable_device(
                            &device_name,
                            true
                        )?;
                    }
                    self.state = XmlReceiverState::WaitForDevicesOn;
                } else {
                    self.state = XmlReceiverState::Working;
                }
            },
            XmlReceiverState::WaitForDevicesOn => {
                self.state = XmlReceiverState::Working;
            }
            _ => {}
        }
        Ok(())
    }
}


pub type PropsNamePair = (&'static str, &'static str);
pub type PropsNamePairs = &'static [PropsNamePair];

const PROP_CAM_TEMPERATURE: PropsNamePairs = &[
    ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE"),
];
const PROP_CAM_COOLING_PWR: PropsNamePairs = &[
    ("COOLER_POWER", "COOLER_POWER"),
];
const PROP_CAM_GAIN: PropsNamePairs = &[
    ("CCD_GAIN",     "GAIN"),
    ("CCD_CONTROLS", "Gain"),
];
const PROP_CAM_OFFSET: PropsNamePairs = &[
    ("CCD_OFFSET",   "OFFSET"),
    ("CCD_CONTROLS", "Offset"),
];
const PROP_CAM_FAN_ON: PropsNamePairs = &[
    ("TC_FAN_CONTROL", "TC_FAN_ON"),
    ("TC_FAN_SPEED",   "INDI_ENABLED"),
];
const PROP_CAM_FAN_OFF: PropsNamePairs = &[
    ("TC_FAN_CONTROL", "TC_FAN_OFF"),
    ("TC_FAN_SPEED",   "INDI_DISABLED"),
];
const PROP_CAM_HEAT_ON: PropsNamePairs = &[
    ("TC_HEAT_CONTROL", ""),
];
const PROP_CAM_LOW_NOISE_ON: PropsNamePairs = &[
    ("TC_LOW_NOISE_CONTROL", "INDI_ENABLED"),
    ("TC_LOW_NOISE",         "INDI_ENABLED"),
];
const PROP_CAM_LOW_NOISE_OFF: PropsNamePairs = &[
    ("TC_LOW_NOISE_CONTROL", "INDI_DISABLED"),
    ("TC_LOW_NOISE",         "INDI_DISABLED"),
];
const PROP_CAM_VIDEO_FORMAT_RGB: PropsNamePairs = &[
    ("CCD_VIDEO_FORMAT", "TC_VIDEO_COLOR_RGB"),
];
const PROP_CAM_VIDEO_FORMAT_RAW: PropsNamePairs = &[
    ("CCD_VIDEO_FORMAT", "TC_VIDEO_COLOR_RAW"),
];
const PROP_CAM_BIN_AVG: PropsNamePairs = &[
    ("CCD_BINNING_MODE", "TC_BINNING_AVG"),
];
const PROP_CAM_BIN_ADD: PropsNamePairs = &[
    ("CCD_BINNING_MODE", "TC_BINNING_ADD"),
];
const PROP_DEVICE_CRASH: PropsNamePairs = &[
    ("CCD_SIMULATE_CRASH", "CRASH"),
];