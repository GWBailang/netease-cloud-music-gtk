//
// songlist_view.rs
// Copyright (C) 2022 gmg137 <gmg137 AT live.com>
// Distributed under terms of the GPL-3.0-or-later license.
//
use gio::Settings;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{CompositeTemplate, glib, *};

use crate::{application::Action, gui::songlist_row::SonglistRow};
use async_channel::Sender;
use glib::{
    ParamSpec, ParamSpecBoolean, ParamSpecInt, RustClosure, SignalHandlerId, Value, clone,
    subclass::Signal,
};
use ncm_api::SongInfo;
use once_cell::sync::{Lazy, OnceCell};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

glib::wrapper! {
    pub struct SongListView(ObjectSubclass<imp::SongListView>)
        @extends Widget, Box,
        @implements Accessible, Actionable, Buildable, ConstraintTarget;
}

impl Default for SongListView {
    fn default() -> Self {
        Self::new()
    }
}

impl SongListView {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_sender(&self, _sender: Sender<Action>) {
        let sender = &self.imp().sender;
        if sender.get().is_none() {
            sender.set(_sender).unwrap();
        }
    }

    fn setup_settings(&self) {
        let settings = Settings::new(crate::APP_ID);

        self.imp()
            .settings
            .set(settings)
            .expect("Could not set `Settings`.");
    }

    pub fn init_new_list(&self, sis: &[SongInfo], likes: &[bool]) {
        let imp = self.imp();
        let sender = imp.sender.get().unwrap().to_owned();
        let settings = imp.settings.get().unwrap();

        let listbox = imp.listbox.get();
        let no_act_like = self.property::<bool>("no-act-like");
        let no_act_album = self.property::<bool>("no-act-album");
        let no_act_remove = self.property::<bool>("no-act-remove");
        sis.iter().zip(likes.iter()).for_each(|(si, like)| {
            let sender = sender.clone();

            let row = SonglistRow::new(sender.clone(), si);
            row.set_property("like", like);
            row.set_like_button_visible(!no_act_like);
            row.set_album_button_visible(!no_act_album);
            row.set_remove_button_visible(!no_act_remove);

            let si = si.clone();
            row.connect_activate(clone!(
                #[weak(rename_to = s)]
                self,
                move |row| {
                    if s.imp().selection_mode.get() {
                        row.set_checked(!row.is_checked());
                        return;
                    }
                    if row.is_activatable() || row.not_ignore_grey() {
                        row.switch_image(true);
                        sender.send_blocking(Action::AddPlay(si.clone())).unwrap();
                        s.emit_row_activated(row);
                    }
                }
            ));

            settings
                .bind("not-ignore-grey", &row, "not-ignore-grey")
                .get_only()
                .build();
            row.set_selection_mode(imp.selection_mode.get());
            let weak = self.downgrade();
            row.selection_button().connect_toggled(move |_| {
                if let Some(list) = weak.upgrade() {
                    list.update_selection_count();
                }
            });
            listbox.append(&row);
        });
    }

    pub fn get_songinfo_list(&self) -> Vec<SongInfo> {
        let listbox = self.imp().listbox.get();
        let mut sis: Vec<SongInfo> = vec![];
        if let Some(mut child) = listbox.first_child() {
            loop {
                let row = child.clone().downcast::<SonglistRow>().unwrap();
                sis.push(row.get_song_info().unwrap());

                if let Some(next) = child.next_sibling() {
                    child = next;
                } else {
                    break;
                }
            }
        }
        sis
    }

    pub fn set_selection_mode(&self, active: bool) {
        self.imp().selection_mode.set(active);
        self.imp().select_all.set_visible(active);
        self.imp().download_selected.set_visible(active);
        self.imp()
            .selection_toggle
            .set_label(&gettextrs::gettext(if active {
                "Cancel selection"
            } else {
                "Select songs"
            }));
        let mut child = self.list_box().first_child();
        while let Some(widget) = child {
            let row = widget.downcast::<SonglistRow>().unwrap();
            row.set_selection_mode(active);
            child = row.next_sibling();
        }
        self.update_selection_count();
    }
    pub fn selected_songs(&self) -> Vec<SongInfo> {
        let mut songs = vec![];
        let mut child = self.list_box().first_child();
        while let Some(widget) = child {
            let row = widget.downcast::<SonglistRow>().unwrap();
            if row.is_checked() {
                if let Some(song) = row.get_song_info() {
                    songs.push(song);
                }
            }
            child = row.next_sibling();
        }
        songs
    }
    pub fn download_selected(&self) {
        let songs = self.selected_songs();
        if !songs.is_empty() {
            let _ = self
                .imp()
                .sender
                .get()
                .unwrap()
                .send_blocking(Action::RequestDownload(songs, None));
        }
    }
    fn update_selection_count(&self) {
        let count = self.selected_songs().len();
        self.imp().download_selected.set_label(&format!(
            "{} ({count})",
            gettextrs::gettext("Download selected")
        ));
        self.imp().download_selected.set_sensitive(count > 0);
    }

    pub fn clear_list(&self) {
        self.set_selection_mode(false);
        let listbox = self.imp().listbox.get();
        while let Some(child) = listbox.last_child() {
            listbox.remove(&child);
        }
    }

    pub fn list_box(&self) -> ListBox {
        self.imp().listbox.get()
    }

    // 根据当前播放歌曲 id 更新指示符，使 ▶️ 始终指向正在播放的曲目。
    pub fn update_playing_song(&self, song_id: u64) {
        let listbox = self.list_box();
        let mut child = listbox.first_child();
        while let Some(row) = child {
            let row = row.downcast::<SonglistRow>().unwrap();
            let visible = row
                .get_song_info()
                .map(|si| si.id == song_id)
                .unwrap_or(false);
            row.switch_image(visible);
            child = row.next_sibling();
        }
    }

    pub fn emit_row_activated(&self, row: &SonglistRow) {
        self.emit_by_name::<()>("row-activated", &[&row]);
    }

    pub fn connect_row_activated(&self, f: RustClosure) -> SignalHandlerId {
        self.connect_closure("row-activated", false, f)
    }
}

#[gtk::template_callbacks]
impl SongListView {}

mod imp {

    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/com/gitee/gmg137/NeteaseCloudMusicGtk4/gtk/songlist-view.ui")]
    pub struct SongListView {
        #[template_child]
        pub scroll_win: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub adw_clamp: TemplateChild<adw::Clamp>,
        #[template_child]
        pub listbox: TemplateChild<ListBox>,
        #[template_child]
        pub selection_toggle: TemplateChild<Button>,
        #[template_child]
        pub select_all: TemplateChild<Button>,
        #[template_child]
        pub download_selected: TemplateChild<Button>,
        pub selection_mode: Cell<bool>,

        pub sender: OnceCell<Sender<Action>>,
        pub settings: OnceCell<Settings>,

        no_act_like: Cell<bool>,
        no_act_album: Cell<bool>,
        no_act_remove: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SongListView {
        const NAME: &'static str = "SongListView";
        type Type = super::SongListView;
        type ParentType = Box;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.bind_template_callbacks();
            klass.bind_template_instance_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl SongListView {}

    impl ObjectImpl for SongListView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.setup_settings();
            let weak = obj.downgrade();
            self.selection_toggle.connect_clicked(move |_| {
                if let Some(list) = weak.upgrade() {
                    list.set_selection_mode(!list.imp().selection_mode.get());
                }
            });
            let weak = obj.downgrade();
            self.download_selected.connect_clicked(move |_| {
                if let Some(list) = weak.upgrade() {
                    list.download_selected();
                }
            });
            let weak = obj.downgrade();
            self.select_all.connect_clicked(move |_| {
                if let Some(list) = weak.upgrade() {
                    let mut child = list.list_box().first_child();
                    while let Some(widget) = child {
                        let row = widget.downcast::<SonglistRow>().unwrap();
                        row.set_checked(true);
                        child = row.next_sibling();
                    }
                    list.update_selection_count();
                }
            });

            // clear old actived row
            let old_select_row = Rc::new(RefCell::new(-1));
            self.listbox.connect_row_activated(move |list, row| {
                let index;
                {
                    index = *old_select_row.borrow();
                }
                *old_select_row.borrow_mut() = row.index();
                if index != -1 && index != row.index() {
                    if let Some(row) = list.row_at_index(index) {
                        let row = row.downcast::<SonglistRow>().unwrap();
                        row.switch_image(false);
                    }
                }
            });
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
                vec![
                    Signal::builder("row-activated")
                        .param_types([SonglistRow::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecBoolean::builder("no-act-like").build(),
                    ParamSpecBoolean::builder("no-act-album").build(),
                    ParamSpecBoolean::builder("no-act-remove").build(),
                    ParamSpecInt::builder("clamp-margin-top").build(),
                    ParamSpecInt::builder("clamp-margin-bottom").build(),
                    ParamSpecInt::builder("clamp-maximum-size").build(),
                    ParamSpecInt::builder("clamp-tightening-threshold").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &Value, pspec: &ParamSpec) {
            match pspec.name() {
                "no-act-like" => {
                    let val = value.get().unwrap();
                    self.no_act_like.replace(val);
                }
                "no-act-album" => {
                    let val = value.get().unwrap();
                    self.no_act_album.replace(val);
                }
                "no-act-remove" => {
                    let val = value.get().unwrap();
                    self.no_act_remove.replace(val);
                }
                "clamp-margin-top" => {
                    let val = value.get().unwrap();
                    self.adw_clamp.set_margin_top(val);
                }
                "clamp-margin-bottom" => {
                    let val = value.get().unwrap();
                    self.adw_clamp.set_margin_bottom(val);
                }
                "clamp-maximum-size" => {
                    let val = value.get().unwrap();
                    self.adw_clamp.set_maximum_size(val);
                }
                "clamp-tightening-threshold" => {
                    let val = value.get().unwrap();
                    self.adw_clamp.set_tightening_threshold(val);
                }
                n => unimplemented!("{}", n),
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            match pspec.name() {
                "no-act-like" => self.no_act_like.get().to_value(),
                "no-act-album" => self.no_act_album.get().to_value(),
                "no-act-remove" => self.no_act_remove.get().to_value(),
                "clamp-margin-top" => self.adw_clamp.margin_top().to_value(),
                "clamp-margin-bottom" => self.adw_clamp.margin_bottom().to_value(),
                "clamp-maximum-size" => self.adw_clamp.maximum_size().to_value(),
                "clamp-tightening-threshold" => self.adw_clamp.tightening_threshold().to_value(),
                n => unimplemented!("{}", n),
            }
        }
    }
    impl WidgetImpl for SongListView {}
    impl BoxImpl for SongListView {}
}
