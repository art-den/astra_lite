use std::{borrow::Cow, sync::Arc};

use chrono::{DateTime, Utc};

use super::{xml_reader::*, error::*, xml_helper::*, num_format::*};

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CamCcd { Main, Guider }

impl CamCcd {
    pub fn from_ccd_prop_name(name: &str) -> Self {
        match name {
            "CCD1"|"" => Self::Main,
            "CCD2"    => Self::Guider,
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
    pub fn to_str(&self) -> &'static str {
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
                Ok(*value),
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
            Self::Num(NumPropValue{value, format, ..}) => {
                let num_format = NumFormat::new_from_indi_format(format);
                num_format.value_to_string(*value)
            }
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

    pub fn to_arc_string(&self) -> Result<Arc<String>> {
        match self {
            Self::Text(text) =>
                Ok(Arc::clone(text)),
            _ =>
                Err(Error::Internal("Element type is not text".to_string())),
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
    pub fn new_from_xml(
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
                label: label.map(Arc::new),
                value,
            });
        }
        Ok(Property {
            device: Arc::clone(dev_name),
            name: Arc::new(prop_name.to_string()),
            type_,
            label: label.map(Arc::new),
            group: group.map(Arc::new),
            permition,
            state,
            timeout,
            timestamp,
            message: message.map(Arc::new),
            elements: items,
            change_id: 0,
        })
    }

    pub fn update_data_from_xml_and_return_changes(
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
            self.message = message.map(Arc::new);
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

    pub fn get_elem(&self, name: &str) -> Option<&PropElement> {
        self.elements.iter().find(|elem| *elem.name == name)
    }

    pub fn get_elem_mut(&mut self, name: &str) -> Option<&mut PropElement> {
        self.elements.iter_mut().find(|elem| *elem.name == name)
    }

    pub fn get_values(&self) -> Vec<(Arc<String>, PropValue)> {
        self
            .elements
            .iter()
            .map(|v| (Arc::clone(&v.name), v.value.clone()))
            .collect()
    }
}
