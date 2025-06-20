use std::{collections::HashMap, rc::Rc};

use gtk::{prelude::*, pango};
use bitflags::bitflags;

use crate::options::Options;

pub enum PanelPosition {
    Left,
    Right,
    Center,
    Bottom,
    Top,
}

bitflags! {
    pub struct PanelFlags: u32 {
        const NO_EXPANDER = 1;
        const EXPANDED    = 2;
        const DEVELOP     = 4;
        const INVISIBLE   = 8;
    }
}

pub struct Panel {
    pub str_id: &'static str,
    pub name:   String,
    pub widget: gtk::Widget,
    pub pos:    PanelPosition,
    pub tab:    TabPage,
    pub flags:  PanelFlags,
}

impl Panel {
    pub fn create_caption_label(&self) -> Option<gtk::Label> {
        if self.flags.contains(PanelFlags::NO_EXPANDER)
        && !self.name.is_empty() {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
            let caption_label = gtk::Label::builder()
                .label(format!("[ {} ]", self.name))
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
                .label(&self.name)
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
    Main,
}

pub const TAB_HARDWARE: u32 = 0;
pub const TAB_MAP:      u32 = 1;
pub const TAB_MAIN:     u32 = 2;

impl TabPage {
    pub fn from_tab_index(index: u32) -> Self {
        match index {
            TAB_HARDWARE => TabPage::Hardware,
            TAB_MAP      => TabPage::SkyMap,
            TAB_MAIN     => TabPage::Main,
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

pub struct UiModuleItem {
    module: Rc<dyn UiModule>,
    widgets: HashMap<gtk::Widget, Vec<gtk::Widget>>,
}

impl UiModuleItem {
    pub fn module(&self) -> &Rc<dyn UiModule> {
        &self.module
    }

    pub fn widgets(&self) -> &HashMap<gtk::Widget, Vec<gtk::Widget>> {
        &self.widgets
    }

    pub fn add_widget(&mut self, panel: &gtk::Widget, widget: gtk::Widget) {
        self.widgets
            .entry(panel.clone())
            .and_modify(|v| v.push(widget.clone()))
            .or_insert(vec![widget]);
    }
}

pub struct UiModules {
    items: Vec<UiModuleItem>,
}

impl UiModules {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
        }
    }

    pub fn add(&mut self, module: Rc<dyn UiModule>) {
        self.items.push(UiModuleItem {
            widgets: HashMap::new(),
            module,
        });
    }

    pub fn show_options(&self, options: &Options) {
        for item in &self.items {
            item.module.show_options(options);
        }
    }

    pub fn get_options(&self, options: &mut Options) {
        for item in &self.items {
            item.module.get_options(options);
        }
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item=&mut UiModuleItem> {
        self.items.iter_mut()
    }

    pub fn items(&self) -> impl Iterator<Item=&UiModuleItem> {
        self.items.iter()
    }

    pub fn process_event(&self, event: &UiModuleEvent) {
        for item in &self.items {
            item.module.process_event(event);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}
