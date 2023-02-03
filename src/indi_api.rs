use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{prelude::*, BufWriter, Cursor, ErrorKind};
use std::net::{TcpStream, SocketAddr};
use std::path::Path;
use std::process::{Command, Child};
use std::sync::{Mutex, Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use itertools::Itertools;
use bitflags::bitflags;
use chrono::prelude::*;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("IO error: `{0}`")]
    IO(#[from] std::io::Error),

    #[error("Internal error: `{0}`")]
    Internal(String),

    #[error("XML error: `{0}`")]
    Xml(String),

    #[error("Device `{0}` not found")]
    DeviceNotExists(String),

    #[error("Property `{1}` of device `{0}` not found")]
    PropertyNotExists(String, String),

    #[error("Property `{1}` of device `{0}` is read only")]
    PropertyIsReadOnly(String, String),

    #[error("Element `{2}` of property `{1}` of device `{0}` not found")]
    PropertyElemNotExists(String, String, String),

    #[error("Property `{1}` of device `{0}` has type {2} but {3} required")]
    WrongPropertyType(String, String, String, String),

    #[error("{0}")]
    WrongArgument(String),

    #[error("Wrong sequense: {0}")]
    WrongSequense(String),

    #[error("Can't convert property value {0} of type {1} into type {2}")]
    CantConvertPropValue(String, String, String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct ConnSettings {
    pub remote: bool,
    pub host: String,
    pub port: u16,
    pub drivers: Vec<String>,
    pub activate_all_devices: bool,
}

impl Default for ConnSettings {
    fn default() -> Self {
        Self {
            remote: false,
            host: "127.0.0.1".to_string(),
            port: 7624,
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
    settings:      ConnSettings,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Error(String)
}

pub struct PropChangeValue {
    pub elem_name:  String,
    pub prop_value: PropValue,
}

pub enum PropChange {
    New(PropChangeValue),
    Change(PropChangeValue),
    Delete,
}

pub struct PropChangeEvent {
    pub timestamp:   DateTime<Utc>,
    pub device_name: String,
    pub prop_name:   String,
    pub change:      PropChange,
}

pub struct DeviceDeleteEvent {
    pub timestamp:   DateTime<Utc>,
    pub device_name: String,
}

pub struct MessageEvent {
    pub timestamp:   DateTime<Utc>,
    pub device_name: String,
    pub text:        String,
}

pub struct BlobStartEvent {
    pub device_name: String,
    pub prop_name:   String,
    pub elem_name:   String,
}

#[derive(Clone)]
pub enum Event {
    ConnChange(ConnState),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropPerm { RO, WO, RW }

impl PropPerm {
    fn from_str(text: Option<&str>) -> anyhow::Result<Self> {
        match text {
            Some("ro") => Ok(PropPerm::RO),
            Some("wo") => Ok(PropPerm::WO),
            Some("rw") => Ok(PropPerm::RW),
            Some(s)    => Err(anyhow::anyhow!("Unknown property permission: {}", s)),
            _          => Err(anyhow::anyhow!("Property permission not defined")),
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

#[derive(Debug, PartialEq, Clone)]
pub struct NumPropElemInfo {
    pub name:   String,
    pub min:    f64,
    pub max:    f64,
    pub step:   Option<f64>,
    pub format: String,
}

#[derive(Debug, PartialEq, Clone)]
pub enum PropType {
    Text,
    Num(Vec<Arc<NumPropElemInfo>>),
    Switch(Option<SwitchRule>),
    Blob
}

impl PropType {
    fn to_str(&self) -> &'static str {
        match self {
            PropType::Text      => "Text",
            PropType::Num(_)    => "Num",
            PropType::Switch(_) => "Switch",
            PropType::Blob      => "Blob",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PropStaticData {
    pub tp:    PropType,
    pub label: Option<String>,
    pub group: Option<String>,
    pub perm:  PropPerm,
}

impl PropStaticData {
    fn from_xml(mut xml: xmltree::Element) -> anyhow::Result<(Self, xmltree::Element)> {
        let tp = match xml.name.as_str() {
            "defTextVector" =>
                PropType::Text,
            "defNumberVector" => {
                let mut elem_info = Vec::new();
                for elem in xml.elements_mut(None) {
                    let format = elem.attr_string_or_err("format")?;
                    let name = elem.attr_str_or_err("name")?;
                    let min = elem.attr_str_or_err("min")?.parse::<f64>()?;
                    let max = elem.attr_str_or_err("max")?.parse::<f64>()?;
                    let step = elem.attributes.get("step").map(|v| v.parse::<f64>().unwrap_or(0.0));
                    elem_info.push(Arc::new(NumPropElemInfo {name: name.to_string(), min, max, step, format}))
                }
                PropType::Num(elem_info)
            },
            "defSwitchVector" => {
                let rule = xml.attributes
                    .get("rule")
                    .map(|rule_str|SwitchRule::from_str(rule_str))
                    .transpose();
                PropType::Switch(rule?)
            },
            "defBLOBVector" => {
                PropType::Blob
            },
            s => {
                anyhow::bail!("Unknown vector: {}", s);
            },
        };
        let label = xml.attributes.remove("label");
        let group = xml.attributes.remove("group");
        let perm = PropPerm::from_str(xml.attributes.get("perm").map(String::as_str))?;
        Ok((PropStaticData{ tp, label, group, perm }, xml))
    }
}

#[derive(Debug, Clone)]
pub struct PropDynamicData {
    pub state:     PropState,
    pub timeout:   Option<u32>,
    pub timestamp: Option<String>, // TODO: normal timestamp instead of string
    pub message:   Option<String>,
}

impl PropDynamicData {
    fn from_xml(mut xml: xmltree::Element) -> anyhow::Result<(Self, xmltree::Element)> {
        let state = PropState::from_str(xml.attr_str_or_err("state")?)?;
        let timeout = xml.attributes.get("timeout")
            .map(|to_str| to_str.parse::<u32>().unwrap_or(0));
        let message = xml.attributes.remove("message");
        let timestamp = xml.attributes.remove("timestamp");
        Ok((PropDynamicData { state, timeout, timestamp, message }, xml))
    }
}

#[derive(Debug)]
struct Property {
    static_data: Arc<PropStaticData>,
    dynamic_data: PropDynamicData,
    elements: Vec<PropElement>,
}

impl Property {
    fn new_from_xml(xml: xmltree::Element) -> anyhow::Result<Self> {
        let (static_data, xml) = PropStaticData::from_xml(xml)?;
        let (dynamic_data, xml) = PropDynamicData::from_xml(xml)?;
        let mut elements = Vec::new();
        for mut child in xml.into_elements(None) {
            let name = child.attr_string_or_err("name")?;
            let label = child.attributes.remove("label");
            let value = match child.name.as_str() {
                "defText" => {
                    Self::get_str_value_from_xml_elem(&child)?
                },
                "defNumber" => {
                    Self::get_num_value_from_xml_elem(&child)?
                },
                "defSwitch" => {
                    Self::get_switch_value_from_xml_elem(&child)?
                },
                "defBLOB" => {
                    PropValue::Blob(Arc::new(BlobPropValue {
                        format: String::new(),
                        data:    Vec::new(),
                        dl_time: 0.0,
                    }))
                },
                other =>
                    anyhow::bail!("Unknown tag `{}`", other),
            };
            elements.push(PropElement {
                name,
                label,
                value,
                change_cnt: 0,
                changed: false
            });
        }
        Ok(Property {
            static_data: Arc::new(static_data),
            dynamic_data,
            elements
        })
    }

    fn update_dyn_data_from_xml(
        &mut self,
        xml:         &mut xmltree::Element,
        mut blob:    Option<Vec<u8>>,
        device_name: &str, // for error message
        prop_name:   &str, // same
        dl_time:     f64,
    ) -> anyhow::Result<()> {
        if let Some(state_str) = xml.attributes.get("state") {
            self.dynamic_data.state = PropState::from_str(state_str)?;
        }
        if let Some(timeout_str) = xml.attributes.get("timeout") {
            self.dynamic_data.timeout = Some(timeout_str.parse()?);
        }
        if let Some(message) = xml.attributes.remove("message") {
            self.dynamic_data.message = Some(message);
        }
        if let Some(timestamp) = xml.attributes.remove("timestamp") {
            self.dynamic_data.timestamp = Some(timestamp);
        }
        for elem in &mut self.elements {
            elem.changed = false;
        }
        for child in xml.elements(None) {
            let elem_name = child.attr_str_or_err("name")?;
            if let Some(elem) = self.get_elem_mut(elem_name) {
                match elem.value {
                    PropValue::Text(_) => {
                        elem.value = Self::get_str_value_from_xml_elem(child)?;
                    },
                    PropValue::Num(_) => {
                        elem.value = Self::get_num_value_from_xml_elem(child)?;
                    },
                    PropValue::Switch(_) => {
                        elem.value = Self::get_switch_value_from_xml_elem(child)?;
                    },
                    PropValue::Blob(_) => {
                        if let Some(data) = blob.take() {
                            let blob_size: usize = child.attributes
                                .get("size")
                                .map(|size_str| size_str.parse())
                                .transpose()?
                                .ok_or_else(|| anyhow::anyhow!(
                                    "size attribute of `{}` not found",
                                    elem.name
                                ))?;
                            if blob_size != data.len() {
                                anyhow::bail!(
                                    "Declated size of blob ({}) is not equal real blob size ({})",
                                    blob_size, data.len()
                                );
                            }
                            let format = child.attr_str_or_err("format")?;
                            elem.value = PropValue::Blob(Arc::new(BlobPropValue {
                                format: format.to_string(),
                                data,
                                dl_time,
                            }));
                        }
                    }
                }
                elem.changed = true;
                elem.change_cnt += 1;
            } else {
                anyhow::bail!(
                    "Element `{}` of property {} of device `{}` not found",
                    elem_name, prop_name, device_name
                );
            }
        }
        Ok(())
    }

    fn get_elem(&self, name: &str) -> Option<&PropElement> {
        self.elements.iter().find(|elem| elem.name == name)
    }

    fn get_elem_mut(&mut self, name: &str) -> Option<&mut PropElement> {
        self.elements.iter_mut().find(|elem| elem.name == name)
    }

    fn get_str_value_from_xml_elem(xml: &xmltree::Element) -> anyhow::Result<PropValue> {
        Ok(PropValue::Text(xml
            .get_text()
            .unwrap_or(Cow::from(""))
            .trim()
            .to_string()
        ))
    }

    fn get_num_value_from_xml_elem(xml: &xmltree::Element) -> anyhow::Result<PropValue> {
        Ok(PropValue::Num(xml
            .get_text()
            .ok_or_else(||anyhow::anyhow!("{} without value", xml.name))?
            .trim()
            .parse::<f64>()?
        ))
    }

    fn get_switch_value_from_xml_elem(xml: &xmltree::Element) -> anyhow::Result<PropValue> {
        Ok(PropValue::Switch(xml
            .get_text()
            .ok_or_else(||anyhow::anyhow!("{} without value", xml.name))?
            .trim()
            .eq_ignore_ascii_case("On")
        ))
    }

    fn get_values(&self, only_changed: bool) -> Vec<(String, PropValue)> {
        self
            .elements
            .iter()
            .filter(|v| v.changed || !only_changed)
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct PropElement {
    pub name:       String,
    pub label:      Option<String>,
    pub value:      PropValue,
    pub change_cnt: u64,
    changed:        bool,
}

#[derive(Debug, Clone)]
pub enum PropValue {
    Text(String),
    Num(f64),
    Switch(bool),
    Blob(Arc<BlobPropValue>),
}

impl PropValue {
    pub fn as_i32(&self) -> Result<i32> {
        match self {
            PropValue::Num(num) =>
                Ok(*num as i32),
            PropValue::Text(text) =>
                text.parse()
                    .map_err(|_| Error::CantConvertPropValue(
                        text.into(),
                        "Text".into(),
                        "i32".into()
                    )),
            PropValue::Switch(value) =>
                Ok(if *value {1} else {0}),
            PropValue::Blob(_) => Err(Error::CantConvertPropValue(
                "[blob]".into(),
                "Blob".into(),
                "i32".into()
            ))
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlobPropValue {
    pub format:  String,
    pub data:    Vec<u8>,
    pub dl_time: f64,
}

type Device = HashMap<String, Property>;
struct Devices(HashMap<String, Device>);

impl Devices {
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

    fn get_names(&self) -> Vec<String> {
        self.0
            .keys()
            .map(String::clone)
            .collect()
    }

    fn get_list(&self) -> Vec<ExportDevice> {
        self.get_names()
            .into_iter()
            .map(|name| {
                let interface_i32 = self.get_property_value(
                    &name,
                    "DRIVER_INFO",
                    "DRIVER_INTERFACE",
                    |_| true,
                    ""
                ).map(|prop| prop.as_i32().unwrap_or(0)).unwrap_or(0);
                let interface = DriverInterface::from_bits_truncate(interface_i32 as u32);
                ExportDevice { name, interface }
            })
            .collect()
    }

    fn get_device_by_driver(&self, driver_name: &str) -> Option<String> {
        self.0.keys()
            .unique()
            .find(|&device| {
                let exec_driver_prop = self.get_text_property(
                    device,
                    "DRIVER_INFO",
                    "DRIVER_EXEC"
                ).ok();
                exec_driver_prop.as_deref() == Some(driver_name)
            })
            .map(|device_cow| device_cow.to_string())
    }

    fn get_device_by_name(&self, device_name: &str) -> Result<&Device> {
        self.0
            .get(device_name)
            .ok_or_else(|| Error::DeviceNotExists(device_name.to_string()))
    }

    fn get_properties_list(&self, device: Option<&str>) -> Vec<ExportProperty> {
        self.0
            .iter()
            .filter(|(k, _)|
                device.is_none() || Some(k.as_str()) == device
            )
            .flat_map(|(device, props)| {
                props.iter().map(|(prop_name, prop)| {
                    ExportProperty {
                        device: device.to_string(),
                        name: prop_name.to_string(),
                        static_data: Arc::clone(&prop.static_data),
                        dynamic_data: prop.dynamic_data.clone(),
                        elements: prop.elements.clone()
                    }
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
        let device = self.get_device(device_name)?;
        let Some(property) = device.get(prop_name)
        else { return Ok(false); };
        if let Some(elem_name) = elem_name {
            Ok(property.elements.iter().any(|e| e.name == elem_name))
        } else {
            Ok(true)
        }
    }

    fn check_property_exists<'a>(
        &self,
        device_name:     &str,
        prop_name:       &str,
        elem_count:      usize,
        elem_check_type: fn (&PropType) -> bool,
        elem_get_name:   impl Fn(usize) -> &'a str,
        req_type_str:    &str,
    ) -> Result<()> {
        let Some(device) = self.0.get(device_name)
        else {
            return Err(Error::DeviceNotExists(device_name.to_string()));
        };
        let Some(property) = device.get(prop_name)
        else {
            return Err(Error::PropertyNotExists(
                device_name.to_string(),
                prop_name.to_string()
            ));
        };
        if property.static_data.perm == PropPerm::RO {
            return Err(Error::PropertyIsReadOnly(
                device_name.to_string(),
                prop_name.to_string(),
            ));
        }
        if !elem_check_type(&property.static_data.tp) {
            return Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                property.static_data.tp.to_str().to_string(),
                req_type_str.to_string(),
            ));
        }
        for index in 0..elem_count {
            let elem_name = elem_get_name(index);
            let elem_exists = property
                .elements
                .iter()
                .any(|element| element.name == elem_name);
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

    pub fn get_switch_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<bool> {
        Self::basic_check_device_and_prop_name(device_name, prop_name)?;
        let prop_value = self.get_property_value(
            device_name,
            prop_name,
            elem_name,
            |tp| matches!(*tp, PropType::Switch(_)),
            "Switch"
        )?;
        if let PropValue::Switch(v) = prop_value {
            Ok(*v)
        } else {
            Err(Error::Internal(format!(
                "Swicth property contains value of other type {:?}",
                prop_value
            )))
        }
    }

    pub fn get_num_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<f64> {
        Self::basic_check_device_and_prop_name(device_name, prop_name)?;
        let prop_value = self.get_property_value(
            device_name,
            prop_name,
            elem_name,
            |tp| matches!(*tp, PropType::Num(_)),
            "Num"
        )?;
        if let PropValue::Num(v) = prop_value {
            Ok(*v)
        } else {
            Err(Error::Internal(format!(
                "Num property contains value of other type {:?}",
                prop_value
            )))
        }
    }

    pub fn get_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<String> {
        Self::basic_check_device_and_prop_name(device_name, prop_name)?;
        let prop_value = self.get_property_value(
            device_name,
            prop_name,
            elem_name,
            |tp| *tp == PropType::Text,
            "Text"
        )?;
        if let PropValue::Text(v) = prop_value {
            Ok(v.clone())
        } else {
            Err(Error::Internal(format!(
                "Text property contains value of other type {:?}",
                prop_value
            )))
        }
    }

    fn existing_prop_name<'a>(
        &self,
        device_name:   &str,
        prop_and_elem: &[(&'a str, &'a str)]
    ) -> Result<Option<(&'a str, &'a str)>> {
        let device = self.get_device(device_name)?;
        for &(prop_name, elem_name) in prop_and_elem {
            let Some(prop) = device.get(prop_name) else {
                continue;
            };
            let elem_exists = prop.elements.iter().any(|e| e.name == elem_name);
            if elem_exists {
                return Ok(Some((prop_name, elem_name)));
            }
        }
        Ok(None)
    }

    fn existing_prop_name_or_err<'a>(
        &self,
        device_name:   &str,
        prop_and_elem: &[(&'a str, &'a str)]
    ) -> Result<(&'a str, &'a str)> {
        let device = self.get_device(device_name)?;
        for &(prop_name, elem_name) in prop_and_elem {
            let Some(prop) = device.get(prop_name) else {
                continue;
            };
            let elem_exists = prop.elements.iter().any(|e| e.name == elem_name);
            if elem_exists {
                return Ok((prop_name, elem_name));
            }
        }
        Err(Error::PropertyNotExists(
            device_name.to_string(),
            prop_and_elem.iter().map(|(p, e)| format!("{}.{}", p, e)).join(", "),
        ))
    }

    fn get_property_static_data(
        &self,
        device_name: &str,
        prop_name:   &str,
    ) -> Result<Arc<PropStaticData>> {
        let prop = self.get_property(device_name, prop_name)?;
        Ok(Arc::clone(&prop.static_data))
    }

    pub fn get_num_prop_elem_info(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<Arc<NumPropElemInfo>> {
        let prop = self.get_property(device_name, prop_name)?;
        if let PropType::Num(num) = &prop.static_data.tp {
            let elem = num.iter()
                .find(|e| e.name == elem_name)
                .ok_or_else(|| Error::PropertyElemNotExists(
                    device_name.to_string(),
                    prop_name.to_string(),
                    elem_name.to_string(),
                ))?;

            Ok(Arc::clone(elem))
        } else {
            Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                prop.static_data.tp.to_str().to_string(),
                "Num".to_string(),
            ))
        }
    }

    fn get_device<'a>(
        &'a self,
        device_name: &str
    ) -> Result<&'a Device> {
        let Some(device) = self.0.get(device_name) else {
            return Err(Error::DeviceNotExists(
                device_name.to_string()
            ));
        };
        Ok(device)
    }

    fn get_property<'a>(
        &'a self,
        device_name:     &str,
        prop_name:       &str,
    ) -> Result<&'a Property> {
        let device = self.get_device(device_name)?;
        let Some(property) = device.get(prop_name) else {
            return Err(Error::PropertyNotExists(
                device_name.to_string(),
                prop_name.to_string()
            ));
        };
        Ok(property)
    }

    fn get_property_value<'a>(
        &'a self,
        device_name:     &str,
        prop_name:       &str,
        elem_name:       &str,
        elem_check_type: fn (&PropType) -> bool,
        req_type_str:    &str,
    ) -> Result<&'a PropValue> {
        let property = self.get_property(device_name, prop_name)?;
        if !elem_check_type(&property.static_data.tp) {
            return Err(Error::WrongPropertyType(
                device_name.to_string(),
                prop_name.to_string(),
                property.static_data.tp.to_str().to_string(),
                req_type_str.to_string(),
            ));
        }
        let Some(elem) = property
            .elements
            .iter()
            .find(|elem|elem.name == elem_name)
        else {
            return Err(Error::PropertyElemNotExists(
                device_name.to_string(),
                prop_name.to_string(),
                elem_name.to_string(),
            ));
        };
        Ok(&elem.value)
    }

    fn is_device_enabled(&self, device_name: &str) -> Result<bool> {
        self.get_switch_property(
            device_name,
            "CONNECTION",
            "CONNECT"
        )
    }

}

#[derive(Debug)]
pub struct ExportProperty {
    pub device:       String,
    pub name:         String,
    pub static_data:  Arc<PropStaticData>,
    pub dynamic_data: PropDynamicData,
    pub elements:     Vec<PropElement>,
}

bitflags! { pub struct DriverInterface: u32 {
    const GENERAL_INTERFACE       = 0;
    const TELESCOPE_INTERFACE     = (1 << 0);
    const CCD_INTERFACE           = (1 << 1);
    const GUIDER_INTERFACE        = (1 << 2);
    const FOCUSER_INTERFACE       = (1 << 3);
    const FILTER_INTERFACE        = (1 << 4);
    const DOME_INTERFACE          = (1 << 5);
    const GPS_INTERFACE           = (1 << 6);
    const WEATHER_INTERFACE       = (1 << 7);
    const AO_INTERFACE            = (1 << 8);
    const DUSTCAP_INTERFACE       = (1 << 9);
    const LIGHTBOX_INTERFACE      = (1 << 10);
    const DETECTOR_INTERFACE      = (1 << 11);
    const ROTATOR_INTERFACE       = (1 << 12);
    const SPECTROGRAPH_INTERFACE  = (1 << 13);
    const CORRELATOR_INTERFACE    = (1 << 14);
    const AUX_INTERFACE           = (1 << 15);
}}

pub enum DeviceCap {
    CcdTemperature,
    CcdExposure,
    CcdGain,
    CcdOffset,
}

#[derive(Debug)]
pub struct ExportDevice {
    pub name:      String,
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
                Devices(HashMap::new())
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

    pub fn connect(&self, settings: &ConnSettings) -> Result<()> {
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
                let start_indiserver_result = Command::new("indiserver")
                    .args(settings.drivers.clone())
                    .spawn();
                match start_indiserver_result {
                    Ok(child) =>
                        Some(child),
                    Err(err) => {
                        Self::set_new_conn_state(
                            ConnState::Error(err.to_string()),
                            &mut state.lock().unwrap(),
                            &subscriptions.lock().unwrap()
                        );
                        return;
                    },
                }
            } else {
                None
            };

            // Parse indi driver address
            let addr = format!("{}:{}", settings.host, settings.port);
            let sock_addr: SocketAddr = match addr.parse() {
                Ok(sock_addr) => sock_addr,
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

            // Try to connect indi driver during 3 seconds
            let mut try_cnt = 30;
            let stream = loop {
                match TcpStream::connect_timeout(&sock_addr, Duration::from_millis(10)) {
                    Ok(stream) =>
                        break stream,
                    Err(err) => {
                        if try_cnt == 0 {
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
                        }
                        try_cnt -= 1;
                        std::thread::sleep(Duration::from_millis(90));
                    }
                };
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
                        XmlSender { xml_sender }
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

            let mut data = data.lock().unwrap();
            *data = Some(ActiveConnData{
                indiserver,
                tcp_stream: stream,
                xml_sender: XmlSender { xml_sender },
                events_thread,
                read_thread,
                write_thread,
                settings,
            });
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
            self.devices.lock().unwrap().0.clear();

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
        devices.get_list()
    }

    pub fn get_device_by_driver(&self, driver_name: &str) -> Option<String> {
        let devices = self.devices.lock().unwrap();
        devices.get_device_by_driver(driver_name)
    }

    pub fn get_properties_list(&self, device: Option<&str>) -> Vec<ExportProperty> {
        let devices = self.devices.lock().unwrap();
        let mut result = devices.get_properties_list(device);
        result.sort_by(|d1, d2| {
            let res = d1.device.cmp(&d2.device);
            if res != Ordering::Equal { return res; }
            d1.name.cmp(&d2.name)
        });
        result
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
    ) -> Result<f64> {
        let devices = self.devices.lock().unwrap();
        devices.get_num_property(device_name, prop_name, elem_name)
    }

    pub fn get_text_property(
        &self,
        device_name: &str,
        prop_name:   &str,
        elem_name:   &str
    ) -> Result<String> {
        let devices = self.devices.lock().unwrap();
        devices.get_text_property(device_name, prop_name, elem_name)
    }

    pub fn get_property_static_data(&self,
        device_name: &str,
        prop_name:   &str,
    ) -> Result<Arc<PropStaticData>> {
        let devices = self.devices.lock().unwrap();
        devices.get_property_static_data(device_name, prop_name)
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

    pub fn command_enable_all_devices(
        &self,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let dev_list = devices.get_list();
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
        self.devices.lock().unwrap().check_property_exists(
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
        self.devices.lock().unwrap().check_property_exists(
            device_name,
            prop_name,
            elements.len(),
            |tp| matches!(*tp, PropType::Num(_)),
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

    fn check_num_property_is_eq(
        &self,
        device_name: &str,
        prop_name:   &str,
        elements:    &[(&str, f64)]
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        for (elem_name, expected_value) in elements {
            let prop_value = devices.get_num_property(
                device_name,
                prop_name,
                elem_name
            )?;
            let diff = f64::abs(prop_value - *expected_value);
            if diff > 0.001 { return Ok(false); }
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
            }
        }
        Ok(())
    }

    fn is_device_support_any_of_props(
        &self,
        device_name: &str,
        props:       PropsStr
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        Ok(devices.existing_prop_name(
            device_name,
            props
        )?.is_some())
    }

    pub fn device_get_any_of_prop_info(
        &self,
        device_name: &str,
        props:       PropsStr
    ) -> Result<Option<Arc<NumPropElemInfo>>> {
        let devices = self.devices.lock().unwrap();
        let Some((prop_name, elem_name)) = devices.existing_prop_name(
            device_name,
            props
        )? else {
            return Ok(None);
        };
        Ok(Some(devices.get_num_prop_elem_info(
            device_name,
            prop_name, elem_name
        )?))
    }

    pub fn device_set_any_of_num_props(
        &self,
        device_name: &str,
        props:       PropsStr,
        value:       f64,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name_or_err(
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
        props:       PropsStr,
        value:       bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name_or_err(
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

    pub fn device_get_any_of_num_props(
        &self,
        device_name: &str,
        props:       PropsStr,
    ) -> Result<f64> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name_or_err(
            device_name,
            props
        )?;
        devices.get_num_property(
            device_name,
            prop_name,
            elem_name
        )
    }

    pub fn device_get_any_of_swicth_props(
        &self,
        device_name: &str,
        props:       PropsStr,
    ) -> Result<bool> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name_or_err(
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
            PROP_DEVICE_CHASH
        )
    }

    pub fn device_chash(
        &self,
        device_name: &str,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        self.set_any_of_switch_props(
            device_name,
            PROP_DEVICE_CHASH,
            true,
            force_set,
            timeout_ms
        )
    }

    // Device polling period

    pub fn get_polling_period(
        &self,
        device_name: &str,
    ) -> Result<usize> {
        let result = self.get_num_property(
            device_name,
            "POLLING_PERIOD",
            "PERIOD_MS"
        )?;
        Ok(result as usize)
    }

    pub fn set_polling_period(
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

    pub fn camera_get_exposure_prop_info(
        &self,
        device_name: &str
    ) -> Result<Option<Arc<NumPropElemInfo>>> {
        let devices = self.devices.lock().unwrap();
        let Some((prop_name, elem_name)) = devices.existing_prop_name(
            device_name,
            PROP_CAM_EXPOSURE
        )? else {
            return Ok(None);
        };
        Ok(Some(devices.get_num_prop_elem_info(
            device_name,
            prop_name, elem_name
        )?))
    }

    pub fn camera_get_exposure(
        &self,
        device_name: &str
    ) -> Result<f64> {
        self.device_get_any_of_num_props(
            device_name,
            PROP_CAM_EXPOSURE
        )
    }

    pub fn camera_start_exposure(
        &self,
        device_name: &str,
        exposure:    f64
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        let (prop_name, elem_name) = devices.existing_prop_name_or_err(
            device_name,
            PROP_CAM_EXPOSURE
        )?;
        drop(devices);
        self.command_set_num_property(
            device_name,
            prop_name,
            &[(elem_name, exposure)]
        )
    }

    pub fn camera_abort_exposure(
        &self,
        device_name: &str
    ) -> Result<()> {
        self.command_set_switch_property(
            device_name,
            "CCD_ABORT_EXPOSURE",
            &[("ABORT", true)]
        )
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

    pub fn camera_get_temperature_prop_info(
        &self,
        device_name: &str
    ) -> Result<Option<Arc<NumPropElemInfo>>> {
        self.device_get_any_of_prop_info(
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

    pub fn camera_get_gain_prop_info(
        &self,
        device_name: &str
    ) -> Result<Option<Arc<NumPropElemInfo>>> {
        self.device_get_any_of_prop_info(
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

    pub fn camera_get_offset_prop_info(
        &self,
        device_name: &str
    ) -> Result<Option<Arc<NumPropElemInfo>>> {
        self.device_get_any_of_prop_info(
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
        let Some((prop, elem)) = (match format {
            CaptureFormat::Rgb =>
                devices.existing_prop_name(device_name, PROP_CAM_VIDEO_FORMAT_RGB)?,
            CaptureFormat::Raw =>
                devices.existing_prop_name(device_name, PROP_CAM_VIDEO_FORMAT_RAW)?,
        }) else { return Ok(false); };

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
    ) -> Result<Vec<String>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.get_device(device_name)?;
        let Some(prop) = device.get("CCD_RESOLUTION") else {
            return Ok(Vec::new());
        };
        Ok(prop.elements
            .iter()
            .map(|e| e.name.clone())
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
    ) -> Result<Option<String>> {
        let devices = self.devices.lock().unwrap();
        let device = devices.get_device(device_name)?;
        let Some(prop) = device.get("CCD_RESOLUTION") else {
            return Ok(None);
        };
        Ok(prop.elements
            .iter()
            .find(|e| e.value.as_i32().unwrap_or(0) != 0)
            .map(|e| e.name.clone())
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

    // Camera frame size

    pub fn camera_set_frame_size(
        &self,
        device_name: &str,
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
            "CCD_FRAME", &[
            ("X",      x as f64),
            ("Y",      y as f64),
            ("WIDTH",  width as f64),
            ("HEIGHT", height as f64),
        ])
    }

    pub fn camera_get_max_frame_size(
        &self,
        device_name: &str,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        let width = devices.get_num_property(device_name, "CCD_INFO", "CCD_MAX_X")?;
        let height = devices.get_num_property(device_name, "CCD_INFO", "CCD_MAX_Y")?;
        Ok((width as usize, height as usize))
    }

    pub fn camera_is_binning_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.property_exists(
            device_name,
            "CCD_BINNING",
            None
        )
    }

    pub fn camera_get_max_binning(
        &self,
        device_name: &str,
    ) -> Result<(usize, usize)> {
        let devices = self.devices.lock().unwrap();
        if devices.property_exists(device_name, "CCD_BINNING", None)? {
            let max_hor = devices.get_num_prop_elem_info(device_name, "CCD_BINNING", "HOR_BIN")?.max;
            let max_vert = devices.get_num_prop_elem_info(device_name, "CCD_BINNING", "VER_BIN")?.max;
            Ok((max_hor as usize, max_vert as usize))
        } else {
            Ok((1, 1))
        }
    }

    pub fn camera_set_binning(
        &self,
        device_name:   &str,
        hor_binnging:  usize,
        vert_binnging: usize,
        force_set:     bool,
        timeout_ms:    Option<u64>,
    ) -> Result<()> {
        self.command_set_num_property_and_wait(
            force_set,
            timeout_ms,
            device_name,
            "CCD_BINNING", &[
            ("HOR_BIN", hor_binnging as f64),
            ("VER_BIN", vert_binnging as f64),
        ])
    }

    pub fn camera_is_binning_mode_supported(
        &self,
        device_name: &str,
    ) -> Result<bool> {
        self.is_device_support_any_of_props(
            device_name,
            PROP_CAM_BIN_AVG
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
        let Some((prop, elem)) = (match binning_mode {
            BinningMode::Add =>
                devices.existing_prop_name(device_name, PROP_CAM_BIN_ADD)?,
            BinningMode::Avg =>
                devices.existing_prop_name(device_name, PROP_CAM_BIN_AVG)?,
        }) else { return Ok(false) };
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

    pub fn camera_set_frame_type(
        &self,
        device_name: &str,
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
            "CCD_FRAME_TYPE",
            &[(elem_name, true)]
        )
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
        let Some((prop, elem)) = (if enable {
            devices.existing_prop_name(device_name, PROP_CAM_FAN_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_FAN_OFF)?
        }) else { return Ok(false) };
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

    pub fn camera_control_heater(
        &self,
        device_name: &str,
        enable:      bool,
        force_set:   bool,
        timeout_ms:  Option<u64>,
    ) -> Result<()> {
        let devices = self.devices.lock().unwrap();
        if let (Some((prop_name, on_elem_name)), Some((_, off_elem_name)))
        = (devices.existing_prop_name(device_name, PROP_CAM_HEAT_ON)?,
           devices.existing_prop_name(device_name, PROP_CAM_HEAT_OFF)?)
        {
            drop(devices);
            self.command_set_switch_property_and_wait(
                force_set,
                timeout_ms,
                device_name,
                prop_name, &[
                (on_elem_name,  enable),
                (off_elem_name, !enable),
            ])?;
        }
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
        let Some((prop, elem)) = (if enable {
            devices.existing_prop_name(device_name, PROP_CAM_LOW_NOISE_ON)?
        } else {
            devices.existing_prop_name(device_name, PROP_CAM_LOW_NOISE_OFF)?
        }) else { return Ok(false); };
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

#[derive(Debug)]
enum ReceiveXmlResult {
    Xml { xml: String, blob: Option<Vec<u8>>, time: f64},
    TimeOut,
    Disconnected
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
    xml_sender:    XmlSender,
    state:         XmlReceiverState,
    read_buffer:   Vec<u8>,
    start_tag_re:  regex::bytes::Regex,
    begin_blob_re: regex::bytes::Regex,
}

impl XmlReceiver {
    fn new(
        conn_state: Arc<Mutex<ConnState>>,
        devices:    Arc<Mutex<Devices>>,
        stream:     TcpStream,
        xml_sender: XmlSender,
    ) -> Self {
        Self {
            conn_state,
            devices,
            stream,
            xml_sender,
            state: XmlReceiverState::Undef,
            read_buffer: Vec::new(),
            start_tag_re: regex::bytes::Regex::new(r"<(\w+)[> /]").unwrap(),
            begin_blob_re: regex::bytes::Regex::new(r#"(?m)<setBLOBVector\s+device="(.*?)"\s+name="(.*?)"(?:.|\s)*?<oneBLOB[^>]+?name="(.*?)"[^>]+?len="(.*?)"(?:.|\s)*?>"#).unwrap(),
        }
    }

    fn main(&mut self, events_sender: mpsc::Sender<Event>) {
        self.stream.set_read_timeout(Some(Duration::from_millis(1000))).unwrap(); // TODO: check error

        self.xml_sender.command_get_properties_impl(None, None).unwrap(); // TODO: check error
        self.state = XmlReceiverState::WaitForDevicesList;

        let mut timeout_processed = false;
        loop {
            let xml_res = self.receive_xml(&events_sender);
            match xml_res {
                Ok(ReceiveXmlResult::Xml{ xml, blob, time }) => {
                    if log::log_enabled!(log::Level::Trace) {
                        log::trace!("indi_api: incoming xml =\n{}", xml);
                    }

                    timeout_processed = false;
                    let process_xml_res = self.process_xml(&xml, blob, time, &events_sender);
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
                },
                Ok(ReceiveXmlResult::Disconnected) => {
                    log::debug!("indi_api: Disconnected");
                    break;
                }
                Ok(ReceiveXmlResult::TimeOut) => {
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
                    log::error!("indi_api: {}", err.to_string());
                },
            }
        }
    }

    fn read_buffer_from_network(
        stream: &mut TcpStream,
        buffer: &mut [u8],
    ) -> anyhow::Result<(usize, Option<ReceiveXmlResult>)> {
        let read_res = stream.read(buffer);
        match read_res {
            Err(e) => match e.kind() {
                ErrorKind::NotConnected |
                ErrorKind::ConnectionAborted |
                ErrorKind::ConnectionReset =>
                    Ok((0, Some(ReceiveXmlResult::Disconnected))),
                ErrorKind::TimedOut | ErrorKind::WouldBlock =>
                    Ok((0, Some(ReceiveXmlResult::TimeOut))),
                _ => Err(e.into()),
            },
            Ok(0) => Ok((0, Some(ReceiveXmlResult::Disconnected))),
            Ok(size) => {
                Ok((size, None))
            }
        }
    }

    fn receive_xml(
        &mut self,
        events_sender: &mpsc::Sender<Event>
    ) -> anyhow::Result<ReceiveXmlResult> {
        let mut start_time: Option<std::time::Instant> = None;
        let mut end_tag_re: Option<regex::bytes::Regex> = None;
        let mut begin_blob_re: Option<&regex::bytes::Regex> = None;
        let mut buffer = [0u8; 2048];
        let mut blob: Option<Vec<u8>> = None;
        loop {
            if let Some(end_tag_re) = &end_tag_re {
                if let Some(end_tag_res) = end_tag_re.captures(&self.read_buffer) {
                    let end_pos = end_tag_res.get(0).unwrap().end();
                    let xml_text = std::str::from_utf8(&self.read_buffer[0..end_pos])?;
                    let xml_text = xml_text.trim().to_string();
                    self.read_buffer.drain(0..end_pos);
                    let time = if let Some(start_time) = start_time {
                        start_time.elapsed().as_secs_f64()
                    } else {
                        0.0
                    };
                    if time > 0.1 {
                        log::info!("XML download time = {:.2} s", time);
                    }
                    return Ok(ReceiveXmlResult::Xml{
                        xml: xml_text,
                        blob,
                        time,
                    });
                }
            } else if let Some(begin_blob_re) = begin_blob_re {
                if let Some(begin_blob_res) = begin_blob_re.captures(&self.read_buffer) {
                    let device_name = std::str::from_utf8(begin_blob_res.get(1).unwrap().as_bytes())?;
                    let prop_name = std::str::from_utf8(begin_blob_res.get(2).unwrap().as_bytes())?;
                    let elem_name = std::str::from_utf8(begin_blob_res.get(3).unwrap().as_bytes())?;
                    let blob_len: usize = std::str::from_utf8(begin_blob_res.get(4).unwrap().as_bytes())?.parse().unwrap_or(0);
                    self.notify_subcribers_about_blob_start(device_name, prop_name, elem_name, events_sender);
                    let mut blob_read_buffer = Vec::new();
                    let blob_start = begin_blob_res.get(0).unwrap().end();
                    blob_read_buffer.extend_from_slice(&self.read_buffer[blob_start..]);
                    self.read_buffer.drain(blob_start..);
                    let mut base64_decoder = Base64Decoder::new(blob_len.min(100_000_000));
                    loop {
                        if let Some(blob_end) = blob_read_buffer.iter().position(|&v| v == b'<') {
                            base64_decoder.add_base64(&blob_read_buffer[..blob_end]);
                            blob = Some(base64_decoder.take_result());
                            self.read_buffer.extend_from_slice(&blob_read_buffer[blob_end..]);
                            break;
                        } else {
                            base64_decoder.add_base64(&blob_read_buffer);
                        }
                        blob_read_buffer.resize(65536, 0);
                        let read = Self::read_buffer_from_network(&mut self.stream, &mut blob_read_buffer)?;
                        if let (_, Some(exit_res)) = read { return Ok(exit_res); }
                        let (read, _) = read;
                        if read == 0 { return Ok(ReceiveXmlResult::Disconnected); }
                        blob_read_buffer.resize(read, 0);
                    }
                    end_tag_re = Some(regex::bytes::Regex::new(r#"</setBLOBVector>"#).unwrap());
                    continue;
                }
            } else if let Some(start_tag_res) = self.start_tag_re.captures(&self.read_buffer) {
                if start_time.is_none() {
                    start_time = Some(std::time::Instant::now());
                }
                let tag_name = std::str::from_utf8(start_tag_res.get(1).unwrap().as_bytes())?;
                if tag_name == "setBLOBVector" {
                    begin_blob_re = Some(&self.begin_blob_re);
                } else {
                    let end_tag_re_text = format!(r#"<{0}\s+.*?/>|</{0}>"#, tag_name);
                    end_tag_re = Some(regex::bytes::Regex::new(&end_tag_re_text).unwrap());
                }
                continue;
            }
            let read = Self::read_buffer_from_network(&mut self.stream, &mut buffer)?;
            if let (_, Some(exit_res)) = read { return Ok(exit_res); }
            let (read, _) = read;
            self.read_buffer.extend_from_slice(&buffer[..read]);
        }
    }

    fn notify_subcribers_about_prop_change(
        &self,
        new_prop:       bool,
        timestamp:      DateTime<Utc>,
        device_name:    &str,
        prop_name:      &str,
        changed_values: Vec<(String, PropValue)>,
        events_sender:  &mpsc::Sender<Event>
    ) {
        for (name, value) in changed_values {
            let value = PropChangeValue {
                elem_name:  name.to_string(),
                prop_value: value,
            };
            let change = if new_prop {
                PropChange::New(value)
            } else {
                PropChange::Change(value)
            };
            let event = PropChangeEvent {
                timestamp,
                device_name: device_name.to_string(),
                prop_name: prop_name.to_string(),
                change,
            };
            events_sender.send(Event::PropChange(Arc::new(event))).unwrap();
        }
    }

    fn notify_subcribers_about_prop_delete(
        &self,
        time:          DateTime<Utc>,
        device_name:   &str,
        prop_name:     &str,
        events_sender: &mpsc::Sender<Event>
    ) {
        let event = PropChangeEvent {
            timestamp: time,
            device_name: device_name.to_string(),
            prop_name: prop_name.to_string(),
            change: PropChange::Delete,
        };
        events_sender.send(Event::PropChange(Arc::new(event))).unwrap();
    }

    fn notify_subcribers_about_device_delete(
        &self,
        time:          DateTime<Utc>,
        device_name:   &str,
        events_sender: &mpsc::Sender<Event>
    ) {
        let event = DeviceDeleteEvent {
            timestamp: time,
            device_name: device_name.to_string(),
        };
        events_sender.send(Event::DeviceDelete(Arc::new(event))).unwrap();
    }

    fn notify_subcribers_about_message(
        &self,
        timestamp:     DateTime<Utc>,
        device_name:   &str,
        message:       String,
        events_sender: &mpsc::Sender<Event>
    ) {
        events_sender.send(Event::Message(Arc::new(MessageEvent {
            timestamp,
            device_name: device_name.to_string(),
            text:        message
        }))).unwrap();
    }

    fn notify_subcribers_about_blob_start(
        &self,
        device_name:   &str,
        prop_name:     &str,
        elem_name:     &str,
        events_sender: &mpsc::Sender<Event>
    ) {
        events_sender.send(Event::BlobStart(Arc::new(BlobStartEvent {
            device_name: device_name.to_string(),
            prop_name: prop_name.to_string(),
            elem_name: elem_name.to_string(),
        }))).unwrap();
    }

    fn process_xml(
        &mut self,
        xml_text:      &str,
        blob:          Option<Vec<u8>>,
        dl_time:       f64,
        events_sender: &mpsc::Sender<Event>
    ) -> anyhow::Result<()> {
        let mut xml_elem = xmltree::Element::parse(xml_text.as_bytes())?;
        if xml_elem.name.starts_with("def") { // defXXXXVector
            let device_name = xml_elem.attr_string_or_err("device")?;
            let prop_name = xml_elem.attr_string_or_err("name")?;
            let timestamp = xml_elem.attr_time_or_err("timestamp")?;
            let property = Property::new_from_xml(xml_elem)?;
            let values = property.get_values(false);
            let mut devices = self.devices.lock().unwrap();
            if let Some(device) = devices.0.get_mut(&device_name) {
                device.insert(prop_name.clone(), property);
            } else {
                devices.0.insert(
                    device_name.clone(),
                    HashMap::from([
                        (prop_name.clone(), property)
                    ])
                );
            }
            drop(devices);
            self.notify_subcribers_about_prop_change(
                true,
                timestamp,
                &device_name,
                &prop_name,
                values,
                events_sender
            );
        } else if xml_elem.name.starts_with("set") { // setXXXXVector
            let device_name = xml_elem.attr_string_or_err("device")?;
            let prop_name = xml_elem.attr_string_or_err("name")?;
            let timestamp = xml_elem.attr_time_or_err("timestamp")?;
            let mut devices = self.devices.lock().unwrap();
            let Some(device) = devices.0.get_mut(&device_name) else {
                anyhow::bail!(Error::DeviceNotExists(device_name));
            };
            let Some(property) = device.get_mut(&prop_name) else {
                anyhow::bail!(Error::PropertyNotExists(
                    device_name,
                    prop_name
                ));
            };
            property.update_dyn_data_from_xml(
                &mut xml_elem,
                blob,
                &device_name,
                &prop_name,
                dl_time,
            )?;
            let values = property.get_values(true);
            drop(devices);
            self.notify_subcribers_about_prop_change(
                false,
                timestamp,
                &device_name,
                &prop_name,
                values,
                events_sender
            );
        } else if xml_elem.name == "delProperty" { // delProperty
            let device_name = xml_elem.attr_string_or_err("device")?;
            let timestamp = xml_elem.attr_time_or_err("timestamp")?;
            let mut devices = self.devices.lock().unwrap();
            if let Some(prop_name) = xml_elem.attributes.remove("name") {
                let Some(device) = devices.0.get_mut(&device_name) else {
                    anyhow::bail!(Error::DeviceNotExists(device_name));
                };
                device
                    .remove(&prop_name)
                    .map_or_else(
                        | | Err(Error::PropertyNotExists(device_name.clone(), prop_name.clone())),
                        |_| Ok(())
                    )?;
                self.notify_subcribers_about_prop_delete(
                    timestamp,
                    &device_name,
                    &prop_name,
                    events_sender
                );
            } else {
                let removed = devices.0.remove(&device_name).is_some();
                if !removed {
                    anyhow::bail!(Error::DeviceNotExists(device_name));
                }
                self.notify_subcribers_about_device_delete(
                    timestamp,
                    &device_name,
                    events_sender
                );
            }
        // message
        } else if xml_elem.name == "message" {
            let message = xml_elem.attr_string_or_err("message")?;
            let device = xml_elem.attr_str_or_err("device")?;
            let timestamp = xml_elem.attr_time_or_err("timestamp")?;
            self.notify_subcribers_about_message(timestamp, device, message, events_sender);
        } else {
            println!("Unknown tag: {}, xml=\n{}", xml_elem.name, xml_text);
        }
        Ok(())
    }

    fn process_time_out(&mut self) -> anyhow::Result<()> {
        match self.state {
            XmlReceiverState::WaitForDevicesList => {
                let devices = self.devices.lock().unwrap();
                let names = devices.get_names();
                for device_name in names {
                    self.xml_sender.command_enable_device(
                        &device_name,
                        true
                    )?;
                }
                self.state = XmlReceiverState::WaitForDevicesOn;
            },
            XmlReceiverState::WaitForDevicesOn => {
                self.state = XmlReceiverState::Working;
            }
            _ => {}
        }
        Ok(())
    }
}

struct Base64Decoder {
    table:    [u8; 256],
    result:   Vec<u8>,
    buffer:   u32,
    eq_count: usize,
}

impl Base64Decoder {
    fn new(expected_size: usize) -> Self {
        let mut table = [0u8; 256];
        for (i, v) in table.iter_mut().enumerate() {
            let i = i as u8;
            *v = match i {
                b'A'..=b'Z' => i - b'A',
                b'a'..=b'z' => i - b'a' + 26,
                b'0'..=b'9' => i - b'0' + 52,
                b'+'        => 62,
                b'/'        => 63,
                b'='        => 0,
                _           => 255,
            }
        }

        Self {
            table,
            result: Vec::with_capacity(expected_size),
            buffer: 1,
            eq_count: 0,
        }
    }

    fn take_result(mut self) -> Vec<u8> {
        while self.buffer & 0x01000000 != 0 {
            self.add_byte(b'=');
        }
        if self.eq_count > 2 {
            self.eq_count = 2;
        }
        if self.result.len() >= self.eq_count {
            self.result.resize(self.result.len()-self.eq_count, 0);
        }
        self.result
    }

    fn add_base64(&mut self, base64_data: &[u8]) {
        for byte in base64_data {
            self.add_byte(*byte);
        }
    }

    #[inline(always)]
    fn add_byte(&mut self, v: u8) {
        if v == b'=' { self.eq_count += 1; }
        let index = self.table[v as usize] as u32;
        if index == 255 { return; }
        self.buffer = (self.buffer << 6) | index;
        if self.buffer & 0x01000000 != 0 {
            let bytes = self.buffer.to_be_bytes();
            self.result.extend_from_slice(&bytes[1..]);
            self.buffer = 1;
        }
    }
}

#[test]
fn test_base64_decoder() {
    let mut decoder = Base64Decoder::new(0);
    decoder.add_base64(b"TWFu");
    assert!(&decoder.take_result() == &b"Man");

    let mut decoder = Base64Decoder::new(0);
    decoder.add_base64(b"TWF=");
    assert!(&decoder.take_result() == &b"Ma");

    let mut decoder = Base64Decoder::new(0);
    decoder.add_base64(b"TW==");
    assert!(&decoder.take_result() == &b"M");

    let mut decoder = Base64Decoder::new(0);
    decoder.add_base64(br#"////////"#);
    assert!(&decoder.take_result() == &[255, 255, 255, 255, 255, 255]);

    let mut decoder = Base64Decoder::new(0);
    decoder.add_base64(br#"///////="#);
    assert!(&decoder.take_result() == &[255, 255, 255, 255, 255]);
}

#[derive(Debug)]
pub struct DriverItem {
    pub device: String,
    pub manufacturer: String,
    pub version: String,
    pub driver_caption: String,
    pub driver: String,
}

impl DriverItem {
    fn from_xml(mut xml: xmltree::Element) -> Result<Self> {
        let device = xml.attr_string_or_err("label")?;
        let manufacturer = xml.attributes.remove("manufacturer")
            .unwrap_or_else(|| "[Undefined]".to_string());
        let driver_child = xml.child_mut_or_err("driver")?;
        let driver_caption = driver_child.attr_string_or_err("name")?;
        let driver = driver_child.text_or_err()?.to_string();
        let version_child = xml.child_mut_or_err("version")?;
        let version = version_child.text_or_err()?.to_string();
        Ok(Self {
            device,
            manufacturer,
            version,
            driver_caption,
            driver
        })
    }
}

#[derive(Debug)]
pub struct DriverGroup {
    pub name:  String,
    pub items: Vec<DriverItem>,
}

impl DriverGroup {
    pub fn get_item_by_device_name(&self, device_name: &str) -> Option<&DriverItem> {
        self.items.iter().find(|d| d.device == device_name)
    }
}

#[derive(Debug)]
pub struct Drivers {
    pub groups: Vec<DriverGroup>,
}

impl Drivers {
    pub fn new() -> Result<Self> {
        Self::new_from_directory(Path::new("/usr/share/indi"))
    }

    fn append_file_data(&mut self, xml_elem: xmltree::Element)  -> Result<()> {
        for mut xml_group_elem in xml_elem.into_elements(Some("devGroup")) {
            let mut driver_items = Vec::new();
            let group = xml_group_elem.attr_string_or_err("group")?;
            for item_xml_elem in xml_group_elem.into_elements(Some("device")) {
                driver_items.push(DriverItem::from_xml(item_xml_elem)?);
            }
            if let Some(existing_group) = self.groups.iter_mut().find(|g| g.name == group) {
                existing_group.items.extend(driver_items);
            } else {
                self.groups.push(DriverGroup {
                    name: group,
                    items: driver_items
                });
            }
        }
        Ok(())
    }

    fn sort_group_items(&mut self) {
        for group in &mut self.groups {
            group.items.sort_by(|item1, item2| {
                let man_cmp = String::cmp(
                    &item1.manufacturer.to_lowercase(),
                    &item2.manufacturer.to_lowercase()
                );
                if man_cmp != std::cmp::Ordering::Equal {
                    return man_cmp;
                }
                String::cmp(
                    &item1.device.to_lowercase(),
                    &item2.device.to_lowercase()
                )
            });
        }
    }

    pub fn new_from_file(p: &Path) -> Result<Self> {
        let xml_text = std::fs::read(p)?;
        let xml_elem = xmltree::Element::parse(&xml_text[..])
            .map_err(|e| Error::Xml(e.to_string()))?;
        let mut result = Drivers { groups: Vec::new() };
        result.append_file_data(xml_elem)?;
        result.sort_group_items();
        Ok(result)
    }

    pub fn new_from_directory(p: &Path) -> Result<Self> {
        let files = std::fs::read_dir(p)?
            .filter_map(|e| e.ok())
            .filter(|e|
                e.path()
                    .is_file()
            )
            .filter(|e|
                e.path()
                    .extension()
                    .and_then(|s|s.to_str()) == Some("xml")
            );

        let mut result = Drivers { groups: Vec::new() };
        for file in files {
            let xml_text = std::fs::read(file.path())?;
            if let Ok(xml_elem) = xmltree::Element::parse(&xml_text[..]) {
                if xml_elem.name == "driversList" {
                    result.append_file_data(xml_elem)?;
                }
            }
        }
        result.sort_group_items();
        Ok(result)
    }

    pub fn get_group_by_name(&self, name: &str) -> Result<&DriverGroup> {
        self
            .groups
            .iter()
            .find(|g| g.name == name)
            .ok_or_else(||
                Error::WrongArgument(format!(
                    "Group {} not found",
                    name
                ))
            )
    }
}

// Helpers for reading from xmltree::Element

trait XmlElementHelper {
    fn into_elements(self, tag: Option<&'static str>) -> Box<dyn Iterator<Item = xmltree::Element>>;
    fn elements_mut<'a>(&'a mut self, tag: Option<&'static str>) -> Box<dyn Iterator<Item = &'a mut xmltree::Element> + 'a>;
    fn elements<'a>(&'a self, tag: Option<&'static str>) -> Box<dyn Iterator<Item = &'a xmltree::Element> + 'a>;
    fn attr_string_or_err(&mut self, attr_name: &str) -> Result<String>;
    fn attr_str_or_err<'a>(&'a self, attr_name: &str) -> Result<&'a str>;
    fn attr_time_or_err(&self, attr_name: &str) -> Result<DateTime<Utc>>;
    fn text_or_err(&self) -> Result<Cow<str>>;
    fn child_mut_or_err(&mut self, child_name: &str) -> Result<&mut xmltree::Element>;
}

impl XmlElementHelper for xmltree::Element {
    fn into_elements(
        self,
        tag: Option<&'static str>
    ) -> Box<dyn Iterator<Item = xmltree::Element>> {
        Box::new(self.children.into_iter()
            .filter_map(move |e| {
                match e {
                    xmltree::XMLNode::Element(e)
                    if tag.is_none() || tag == Some(e.name.as_str()) =>
                        Some(e),
                    _ =>
                        None,
                }
            })
        )
    }

    fn elements_mut<'a>(
        &'a mut self,
        tag: Option<&'static str>
    ) -> Box<dyn Iterator<Item = &'a mut xmltree::Element> + 'a> {
        Box::new(self.children.iter_mut()
            .filter_map(move |e| {
                match e {
                    xmltree::XMLNode::Element(e)
                    if tag.is_none() || tag == Some(e.name.as_str()) =>
                        Some(e),
                    _ =>
                        None,
                }
            })
        )
    }

    fn elements<'a>(
        &'a self,
        tag: Option<&'static str>
    ) -> Box<dyn Iterator<Item = &'a xmltree::Element> + 'a> {
        Box::new(self.children.iter()
            .filter_map(move |e| {
                match e {
                    xmltree::XMLNode::Element(e)
                    if tag.is_none() || tag == Some(e.name.as_str()) =>
                        Some(e),
                    _ =>
                        None,
                }
            })
        )
    }

    fn attr_string_or_err(&mut self, attr_name: &str) -> Result<String> {
        self.attributes.remove(attr_name)
            .ok_or_else(|| Error::Xml(format!(
                "`{}` without `{}` attribute",
                self.name,
                attr_name
            )))
    }

    fn attr_str_or_err<'a>(&'a self, attr_name: &str) -> Result<&'a str> {
        self.attributes.get(attr_name)
            .map(String::as_str)
            .ok_or_else(|| Error::Xml(format!(
                "`{}` without `{}` attribute",
                self.name,
                attr_name
            )))
    }

    fn attr_time_or_err(&self, attr_name: &str) -> Result<DateTime<Utc>> {
        self.attributes.get(attr_name)
            .map(|s| Utc.datetime_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap_or_default())
            .ok_or_else(|| Error::Xml(format!(
                "`{}` without `{}` attribute",
                self.name,
                attr_name
            )))
    }

    fn text_or_err(&self) -> Result<Cow<str>> {
        self
            .get_text()
            .ok_or_else(||Error::Xml(format!(
                "`{}` dosn't contain text",
                self.name
            )))
    }

    fn child_mut_or_err(
        &mut self,
        child_name: &str
    ) -> Result<&mut xmltree::Element> {
        self.children.iter_mut()
            .filter_map(|n|
                match n {
                    xmltree::XMLNode::Element(e) => Some(e),
                    _ => None,
                }
            )
            .find(|e| e.name == child_name)
            .ok_or_else(|| Error::Xml(
                format!(
                    "Child `{}` not found",
                    child_name
                )
            ))
    }
}

type PropsStr = &'static [(&'static str, &'static str)];

const PROP_CAM_TEMPERATURE: PropsStr = &[
    ("CCD_TEMPERATURE", "CCD_TEMPERATURE_VALUE"),
];
const PROP_CAM_EXPOSURE: PropsStr = &[
    ("CCD_EXPOSURE", "CCD_EXPOSURE_VALUE"),
];
const PROP_CAM_GAIN: PropsStr = &[
    ("CCD_GAIN",     "GAIN"),
    ("CCD_CONTROLS", "Gain"),
];
const PROP_CAM_OFFSET: PropsStr = &[
    ("CCD_OFFSET",   "OFFSET"),
    ("CCD_CONTROLS", "Offset"),
];
const PROP_CAM_FAN_ON: PropsStr = &[
    ("TC_FAN_CONTROL", "TC_FAN_ON"),
];
const PROP_CAM_FAN_OFF: PropsStr = &[
    ("TC_FAN_CONTROL", "TC_FAN_OFF"),
];
const PROP_CAM_HEAT_ON: PropsStr = &[
    ("TC_HEAT_CONTROL", "TC_HEAT_ON"),
];
const PROP_CAM_HEAT_OFF: PropsStr = &[
    ("TC_HEAT_CONTROL", "TC_HEAT_OFF"),
];
const PROP_CAM_LOW_NOISE_ON: PropsStr = &[
    ("TC_LOW_NOISE_CONTROL", "INDI_ENABLED"),
];
const PROP_CAM_LOW_NOISE_OFF: PropsStr = &[
    ("TC_LOW_NOISE_CONTROL", "INDI_DISABLED"),
];
const PROP_CAM_VIDEO_FORMAT_RGB: PropsStr = &[
    ("CCD_VIDEO_FORMAT", "TC_VIDEO_COLOR_RGB"),
];
const PROP_CAM_VIDEO_FORMAT_RAW: PropsStr = &[
    ("CCD_VIDEO_FORMAT", "TC_VIDEO_COLOR_RAW"),
];
const PROP_CAM_BIN_AVG: PropsStr = &[
    ("CCD_BINNING_MODE", "TC_BINNING_AVG"),
];
const PROP_CAM_BIN_ADD: PropsStr = &[
    ("CCD_BINNING_MODE", "TC_BINNING_ADD"),
];
const PROP_DEVICE_CHASH: PropsStr = &[
    ("CCD_SIMULATE_CRASH", "CRASH"),
];