use std::{collections::HashMap, rc::Rc};

use gtk::{prelude::*, pango};
use bitflags::bitflags;

use crate::{core::events::Event, indi, options::Options};

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

pub trait UiModule {
    fn show_options(&self, options: &Options);
    fn get_options(&self, options: &mut Options);
    fn panels(&self) -> Vec<Panel>;
    fn on_show_options_first_time(&self) {}
    fn on_full_screen(&self, _full_screen: bool) {}
    fn on_app_closing(&self) {}
    fn on_tab_changed(&self, _from: TabPage, _to: TabPage) {}
    fn on_250ms_timer(&self) {}
    fn on_indi_event(&self, _event: &indi::Event) {}
    fn on_core_event(&self, _event: &Event) {}
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

    pub fn clear(&mut self) {
        self.items.clear();
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

    pub fn on_show_first_options(&self) {
        for item in &self.items {
            item.module.on_show_options_first_time();
        }
    }

    pub fn on_full_screen(&self, full_screen: bool) {
        for item in &self.items {
            item.module.on_full_screen(full_screen);
        }
    }

    pub fn on_app_closing(&self) {
        for item in &self.items {
            item.module.on_app_closing();
        }
    }

    pub fn on_tab_changed(&self, from: TabPage, to: TabPage) {
        for item in &self.items {
            item.module.on_tab_changed(from, to);
        }
    }

    pub fn on_250ms_timer(&self) {
        for item in &self.items {
            item.module.on_250ms_timer();
        }
    }

    pub fn on_indi_event(&self, event: &indi::Event) {
        for item in &self.items {
            item.module.on_indi_event(event);
        }
    }

    pub fn on_core_event(&self, event: &Event) {
        for item in &self.items {
            item.module.on_core_event(event);
        }
    }
}
