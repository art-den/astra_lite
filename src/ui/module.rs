use std::rc::Rc;

use gtk::{prelude::*, pango};
use bitflags::bitflags;

use crate::options::Options;

pub enum PanelPosition {
    Left,
    Right,
    Center,
    BottomLeft,
    Top,
}

pub enum PanelTab {
    Hardware,
    Map,
    Common,
}

bitflags! {
    pub struct PanelFlags: u32 {
        const NO_EXPANDER = 1;
        const EXPANDED    = 2;
    }
}

pub struct Panel {
    pub str_id: &'static str,
    pub name:   String,
    pub widget: gtk::Widget,
    pub pos:    PanelPosition,
    pub tab:    PanelTab,
    pub flags:  PanelFlags,
}

impl Panel {
    pub fn create_caption_label(&self) -> Option<gtk::Label> {
        if self.flags.contains(PanelFlags::NO_EXPANDER)
        && !self.name.is_empty() {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
            let caption_label = gtk::Label::builder()
                .label(&format!("[ {} ]", self.name))
                .attributes(&attrs)
                .visible(true)
                .build();
            caption_label.set_halign(gtk::Align::Start);
            caption_label.style_context().add_class("header_label");
            Some(caption_label)
        } else {
            None
        }
    }

    pub fn create_widget(&self) -> gtk::Widget {
        if !self.flags.contains(PanelFlags::NO_EXPANDER)
        && !self.name.is_empty() {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
            let expander = gtk::Expander::builder()
                .visible(true)
                .label(&format!("[ {} ]", self.name))
                .build();
            expander.style_context().add_class("expander");
            let label = expander.label_widget().unwrap().downcast::<gtk::Label>().unwrap();
            label.set_attributes(Some(&attrs));
            expander.add(&self.widget);
            expander.upcast()
        } else {
            self.widget.clone()
        }
    }
}

#[derive(Clone, PartialEq, Copy)]
pub enum TabPage {
    Hardware,
    SkyMap,
    Camera,
}

pub const TAB_HARDWARE: u32 = 0;
pub const TAB_MAP:      u32 = 1;
pub const TAB_CAMERA:   u32 = 2;

impl TabPage {
    pub fn from_tab_index(index: u32) -> Self {
        match index {
            TAB_HARDWARE => TabPage::Hardware,
            TAB_MAP      => TabPage::SkyMap,
            TAB_CAMERA   => TabPage::Camera,
            _ => unreachable!(),
        }
    }
}

pub enum UiModuleEvent {
    AfterFirstShowOptions,
    FullScreen(bool),
    ProgramClosing,
    TabChanged { from: TabPage, to: TabPage },
    Timer,
}

pub trait UiModule {
    fn show_options(&self, options: &Options);
    fn get_options(&self, options: &mut Options);
    fn panels(&self) -> Vec<Panel>;
    fn process_event(&self, event: &UiModuleEvent);
}

pub struct UiModules {
    items: Vec<Rc<dyn UiModule>>,
}

impl UiModules {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
        }
    }

    pub fn add(&mut self, module: Rc<dyn UiModule>) {
        self.items.push(module);
    }

    pub fn show_options(&self, options: &Options) {
        for module in &self.items {
            module.show_options(options);
        }
    }

    pub fn get_options(&self, options: &mut Options) {
        for module in &self.items {
            module.get_options(options);
        }
    }

    pub fn items(&self) -> impl Iterator<Item=&Rc<dyn UiModule>> {
        self.items.iter()
    }

    pub fn process_event(&self, event: &UiModuleEvent) {
        for module in &self.items {
            module.process_event(event);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}
