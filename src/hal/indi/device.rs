use std::sync::Arc;

use chrono::Utc;
use itertools::Itertools;

use super::{error::*, events::*, property::*, connection::*};

pub struct Device {
    name: Arc<String>,
    props: Vec<Property>,
    is_connected: bool,
    interface: DriverInterface,
}

impl Device {
    pub fn new(name: &Arc<String>) -> Self {
        Self {
            name:         Arc::clone(name),
            props:        Vec::new(),
            is_connected: false,
            interface:    DriverInterface::empty(),
        }
    }

    pub fn name(&self) -> &Arc<String> {
        &self.name
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.is_connected = connected;
    }

    pub fn driver_interface(&self) -> DriverInterface {
        self.interface
    }

    pub fn set_driver_interface(&mut self, interface: DriverInterface) {
        self.interface = interface
    }

    pub fn add_property(&mut self, property: Property) {
        self.props.push(property);
    }

    pub fn get_property_opt(&self, prop_name: &str) -> Option<&Property> {
        self.props
            .iter()
            .find(|prop| *prop.name == prop_name)
    }

    pub fn get_property_opt_mut(&mut self, prop_name: &str) -> Option<&mut Property> {
        self.props
            .iter_mut()
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

    pub fn remove_property(&mut self, prop_name: &str) -> Option<Property> {
        let index = self.props
            .iter()
            .position(|prop| *prop.name == prop_name)?;
        Some(self.props.remove(index))
    }

    fn get_driver_info(&self) -> Option<DriverInfo> {
        let iface_elem = self.get_property_element_opt("DRIVER_INFO", "DRIVER_INTERFACE")?;
        let i32_value = iface_elem.value.to_i32().unwrap_or(0);
        let interface = DriverInterface::from_bits_truncate(i32_value as u32);

        let exec_elem = self.get_property_element_opt("DRIVER_INFO", "DRIVER_EXEC")?;
        let driver = exec_elem.value.to_arc_string().ok()?;

        Some(DriverInfo{ interface, driver })
    }
}

pub struct Devices {
    list:      Vec<Device>,
    change_id: u64,
}

impl Devices {
    pub fn new() -> Self {
        Self {
            list:      Vec::new(),
            change_id: 1,
        }
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    pub fn add(&mut self, device: Device) -> &mut Device {
        self.list.push_mut(device)
    }

    pub fn next_change_id(&mut self) -> u64 {
        self.change_id += 1;
        self.change_id
    }

    pub fn basic_check_device_and_prop_name(
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

    pub fn find_by_name_res(&self, device_name: &str) -> Result<&Device> {
        self.list
            .iter()
            .find(|device| *device.name == device_name)
            .ok_or_else(|| Error::DeviceNotExists(device_name.to_string()))
    }

    pub fn find_by_name_opt(&self, device_name: &str) -> Option<&Device> {
        self.list
            .iter()
            .find(|device| *device.name == device_name)
    }

    pub fn find_by_name_opt_mut(&mut self, device_name: &str) -> Option<&mut Device> {
        self.list
            .iter_mut()
            .find(|device| *device.name == device_name)
    }

    pub fn remove(&mut self, device_name: &str) -> Option<Device> {
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

    pub fn get_list_iter(&self) -> Box<dyn Iterator<Item = ExportDevice> + '_> {
        Box::new(self.list
            .iter()
            .filter_map(|device|
                device.get_driver_info().map(|iface| (device, iface))
            )
            .map(|(device, di)|
                ExportDevice {
                    name:      Arc::clone(&device.name),
                    interface: di.interface,
                    driver:    Arc::clone(&di.driver),
                    connected: device.is_connected,
                }
            )
        )
    }

    pub fn get_properties_list(
        &self,
        changed_after: Option<u64>,
    ) -> Vec<Property> {
        self.list
            .iter()
            .flat_map(|device| {
                device.props.iter().filter_map(|prop| {
                    if let Some(changed_after) = changed_after
                    && prop.change_id > changed_after {
                        Some(prop.clone())
                    } else {
                        None
                    }

                })
            })
            .collect()
    }

    pub fn property_exists(
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

    pub fn check_property_ok_for_writing<'a>(
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
        if property.permission == PropPermission::RO {
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

    pub fn mark_property_as_busy(
        &mut self,
        device_name:   &str,
        prop_name:     &str,
        events_sender: &std::sync::mpsc::Sender<EventSenderEvent>,
    ) -> Result<()> {
        let device = self.find_by_name_opt_mut(device_name)
            .ok_or_else(|| Error::DeviceNotExists(device_name.to_string()))?;
        let device_name = Arc::clone(&device.name);
        let property = device
            .get_property_opt_mut(prop_name)
            .ok_or_else(|| Error::PropertyNotExists(device_name.to_string(), prop_name.to_string()))?;
        if property.state == PropState::Busy {
            return Ok(());
        }
        let prev_state = property.state;
        property.state = PropState::Busy;
        for elem in &property.elements {
            let change = PropChange::Change {
                prop_name: Arc::clone(&property.name),
                elem_name: Arc::clone(&elem.name),
                value:     elem.value.clone(),
                new_state: property.state,
                prev_state,
            };
            let event = PropChangeEvent {
                timestamp:   Some(Utc::now()),
                device_name: Arc::clone(&device_name),
                change
            };
            events_sender
                .send(EventSenderEvent::Mess(Event::PropChange(event)))
                .map_err(|e| Error::Internal(e.to_string()))?;
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
                "Text property contains value of other type {:?}",
                elem_value.value.type_str()
            )))
        }
    }

    pub fn existing_prop_name_opt<'a>(
        &self,
        device:        &Device,
        prop_and_elem: &[(&'a str, &'a str)]
    ) -> Option<(&'a str, &'a str)> {
        for &(prop_name, elem_name) in prop_and_elem {
            let Some(prop) = device.get_property_opt(prop_name) else {
                continue;
            };
            let elem_exists = prop.elements.iter().any(|e|
                elem_name.is_empty() || *e.name == elem_name
            );
            if elem_exists {
                return Some((prop_name, elem_name));
            }
        }
        None
    }

    pub fn existing_prop_name<'a>(
        &self,
        device_name:   &str,
        prop_and_elem: &[(&'a str, &'a str)]
    ) -> Result<(&'a str, &'a str)> {
        let device = self.find_by_name_res(device_name)?;
        if let Some(result) = self.existing_prop_name_opt(device, prop_and_elem) {
            Ok(result)
        } else {
            let props_list = prop_and_elem
                .iter()
                .map(|(elem, name)| format!("{}.{}", elem, name))
                .join(", ");
            Err(Error::NoPropertyFound(props_list, device_name.to_string()))
        }
    }

    pub fn get_driver_info(&self, device_name: &str) -> Result<DriverInfo> {
        let device = self.find_by_name_res(device_name)?;
        device
            .get_driver_info()
            .ok_or_else(|| Error::Internal("device.get_driver_info() returned None".to_string()))
    }

    pub fn get_property(
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
