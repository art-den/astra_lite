use std::borrow::Cow;
use chrono::prelude::*;
use super::error::*;

// Helpers for reading from xmltree::Element

pub trait XmlElementHelper {
    fn into_elements(self, tag: Option<&'static str>) -> Box<dyn Iterator<Item = xmltree::Element>>;
    fn elements_mut<'a>(&'a mut self, tag: Option<&'static str>) -> Box<dyn Iterator<Item = &'a mut xmltree::Element> + 'a>;
    fn elements<'a>(&'a self, tag: Option<&'static str>) -> Box<dyn Iterator<Item = &'a xmltree::Element> + 'a>;
    fn attr_string_or_err(&mut self, attr_name: &str) -> Result<String>;
    fn attr_str_or_err<'a>(&'a self, attr_name: &str) -> Result<&'a str>;
    fn attr_time(&self, attr_name: &str) -> Option<DateTime<Utc>>;
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

    fn attr_time(&self, attr_name: &str) -> Option<DateTime<Utc>> {
        self.attributes
            .get(attr_name)
            .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
            .map(|dt| Utc.from_utc_datetime(&dt))
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
