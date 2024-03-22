use std::path::Path;
use super::{xml_helper::*, error::*};

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
    pub fn new_empty() -> Self {
        Self {
            groups: Vec::new(),
        }
    }

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
                    .is_file() &&
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
