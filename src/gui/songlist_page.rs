//
// songlist_page.rs
// Copyright (C) 2022 gmg137 <gmg137 AT live.com>
// Distributed under terms of the GPL-3.0-or-later license.
//
use async_channel::Sender;
use chrono::{TimeZone, Utc};
use gettextrs::gettext;
use glib::{ParamSpec, ParamSpecBoolean, Value};
pub(crate) use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*, *};
use ncm_api::SongList;
use once_cell::sync::{Lazy, OnceCell};

use crate::{
    application::Action,
    gui::songlist_view::SongListView,
    model::{DiscoverSubPage, ImageDownloadImpl, SongListDetail},
    path::CACHE,
    utils::*,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

glib::wrapper! {
    pub struct SonglistPage(ObjectSubclass<imp::SonglistPage>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable,gtk::ConstraintTarget, gtk::Orientable;
}

impl SonglistPage {
    pub fn new() -> Self {
        let songlist_page: SonglistPage = glib::Object::new();
        setup_ellipsis_tooltip(&songlist_page.imp().title_label.get());
        songlist_page
    }

    pub fn set_sender(&self, sender: Sender<Action>) {
        self.imp().sender.set(sender).unwrap();
    }

    pub fn init_songlist_info(&self, songlist: &SongList, is_album: bool, is_logined: bool) {
        let imp = self.imp();
        let sender = imp.sender.get().unwrap();
        imp.songlist.replace(Some(songlist.to_owned()));

        if is_album {
            imp.time_label.set_visible(true);
        }

        // 判断是否显示收藏按钮
        let like_button = imp.like_button.get();
        if is_logined {
            like_button.set_visible(true);
            imp.songs_list.set_property("no-act-like", false);
        } else {
            like_button.set_visible(false);
            imp.songs_list.set_property("no-act-like", true);
        }

        // 设置专辑图
        let cover_image = imp.cover_image.get();
        let mut path = CACHE.clone();
        path.push(format!("{}-songlist.jpg", songlist.id));
        if !path.exists() {
            cover_image.set_icon_name(Some("image-missing-symbolic"));
            cover_image.set_from_net(songlist.cover_img_url.to_owned(), path, (140, 140), sender);
        } else {
            cover_image.set_from_file(Some(&path));
        }

        // 设置标题
        let title = imp.title_label.get();
        title.set_label(&songlist.name);

        imp.num_label
            .get()
            .set_label(&gettext_f("{num} songs", &[("num", "0")]));
        self.set_property("like", false);

        imp.songs_list.clear_list();
        // 页面创建时为加载态，列表填充完成后由 init_songlist 关闭
        self.set_property("loading", true);
    }

    pub fn init_songlist(&self, detail: &SongListDetail, likes: &[bool]) {
        let imp = self.imp();
        let songs_list = imp.songs_list.get();

        let sis = &mut detail.sis().clone();

        match detail {
            SongListDetail::Album(detail, dy) => {
                self.set_property("like", dy.is_sub);
                imp.songs_list.set_property("no-act-album", true);
                imp.songs_list.set_property("no-act-remove", true);
                imp.page_type.replace(Some(DiscoverSubPage::Album));
                let dt = Utc
                    .timestamp_millis_opt(detail.publish_time as i64)
                    .unwrap();
                let dt = dt.format("%Y-%m-%d");
                imp.time_label.set_label(&format!("{}", dt,));

                imp.num_label.set_label(&format!(
                    "{}, {}",
                    gettext_f("{num} songs", &[("num", &sis.len().to_string())]),
                    gettext_f("{num} favs", &[("num", &dy.sub_count.to_string())])
                ));
            }
            SongListDetail::PlayList(_detail, dy) => {
                self.set_property("like", dy.subscribed);
                imp.songs_list.set_property("no-act-album", false);
                imp.songs_list.set_property("no-act-remove", true);
                imp.page_type.replace(Some(DiscoverSubPage::SongList));
                imp.num_label.set_label(&format!(
                    "{}, {}",
                    gettext_f("{num} songs", &[("num", &sis.len().to_string())]),
                    gettext_f("{num} favs", &[("num", &dy.booked_count.to_string())])
                ));
            }
            SongListDetail::Radio(detail) => {
                imp.songs_list.set_property("no-act-album", true);
                imp.songs_list.set_property("no-act-like", true);
                imp.songs_list.set_property("no-act-remove", true);
                // 电台页不支持收藏，隐藏收藏按钮
                imp.like_button.get().set_visible(false);
                imp.page_type.replace(Some(DiscoverSubPage::Radio));
                imp.num_label.set_label(&gettext_f(
                    "Total {num} issues",
                    &[("num", &detail.len().to_string())],
                ));
                for si in sis.iter_mut() {
                    if let Ok(date) = si.album.parse() {
                        let dt = Utc.timestamp_millis_opt(date).unwrap();
                        let dt = dt.format("%Y-%m-%d");
                        si.album = dt.to_string();
                    } else {
                        si.album = "未知".to_string();
                    }
                }
                // 缓存电台节目列表，供点击排序按钮时本地反转重渲染
                imp.radio_songs.replace(sis.clone());
                imp.radio_asc.set(false);
                // 默认按“最新优先”展示，并据此设置按钮提示
                let sort_btn = imp.sort_button.get();
                sort_btn.set_active(false);
                sort_btn.set_tooltip_text(Some(&gettext("Switch to oldest first")));
            }
        }

        let sender = imp.sender.get().unwrap();
        songs_list.set_sender(sender.clone());
        if matches!(detail, SongListDetail::Radio(_)) {
            self.render_radio();
        } else {
            songs_list.init_new_list(sis, likes);
        }
        imp.download_all
            .set_visible(!matches!(detail, SongListDetail::Radio(_)));
        imp.sort_button
            .get()
            .set_visible(matches!(detail, SongListDetail::Radio(_)));

        // 订阅窗口当前播放歌曲变化，使 ▶️ 指示符跟随播放进度。
        if let Some(window) = self
            .root()
            .and_downcast::<crate::window::NeteaseCloudMusicGtk4Window>()
        {
            if !imp.subscribed.get() {
                imp.subscribed.set(true);
                let songs_list = songs_list.downgrade();
                window.connect_local("current-song-changed", false, move |args| {
                    let id = args[1].get::<u64>().unwrap_or(0);
                    if let Some(songs_list) = songs_list.upgrade() {
                        songs_list.update_playing_song(id);
                    }
                    None
                });
            }
            songs_list.update_playing_song(window.current_song_id());
        }
        // 列表已填充，关闭加载动画
        self.set_property("loading", false);
    }

    // 依据当前排序状态重渲染电台节目列表（最早优先时反转缓存的列表）
    fn render_radio(&self) {
        let imp = self.imp();
        let mut sis = imp.radio_songs.borrow().clone();
        if imp.radio_asc.get() {
            sis.reverse();
        }
        // 电台行不显示收藏状态，用等长的占位数组保证行数正确即可
        let likes = vec![false; sis.len()];
        imp.songs_list.clear_list();
        imp.songs_list.init_new_list(&sis, &likes);
        if let Some(window) = self
            .root()
            .and_downcast::<crate::window::NeteaseCloudMusicGtk4Window>()
        {
            imp.songs_list.update_playing_song(window.current_song_id());
        }
    }
}

impl Default for SonglistPage {
    fn default() -> Self {
        Self::new()
    }
}

mod imp {

    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/com/gitee/gmg137/NeteaseCloudMusicGtk4/gtk/songlist-page.ui")]
    pub struct SonglistPage {
        #[template_child(id = "cover_image")]
        pub cover_image: TemplateChild<Image>,
        #[template_child]
        pub download_all: TemplateChild<Button>,
        #[template_child(id = "title_label")]
        pub title_label: TemplateChild<Label>,
        #[template_child(id = "time_label")]
        pub time_label: TemplateChild<Label>,
        #[template_child(id = "num_label")]
        pub num_label: TemplateChild<Label>,
        #[template_child(id = "play_button")]
        pub play_button: TemplateChild<Button>,
        #[template_child(id = "like_button")]
        pub like_button: TemplateChild<Button>,

        #[template_child(id = "songs_list")]
        pub songs_list: TemplateChild<SongListView>,

        // 用于切换电台节目排序的圆形切换按钮
        #[template_child(id = "sort_button")]
        pub sort_button: TemplateChild<ToggleButton>,

        // 列表加载时的居中旋转指示器
        #[template_child(id = "loading_spinner")]
        pub loading_spinner: TemplateChild<Spinner>,

        pub songlist: Rc<RefCell<Option<SongList>>>,
        pub page_type: Rc<RefCell<Option<DiscoverSubPage>>>,

        pub sender: OnceCell<Sender<Action>>,

        pub subscribed: Cell<bool>,
        like: Cell<bool>,
        loading: Cell<bool>,

        // 电台节目排序状态：缓存已加载的节目列表与当前顺序
        // （false=最新优先，true=最早优先），以便本地反转而无需重新请求接口
        pub radio_songs: Rc<RefCell<Vec<ncm_api::SongInfo>>>,
        pub radio_asc: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SonglistPage {
        const NAME: &'static str = "SonglistPage";
        type Type = super::SonglistPage;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl SonglistPage {
        #[template_callback]
        fn download_all_clicked_cb(&self) {
            use netease_cloud_music_gtk4::download::model::{CollectionItem, CollectionSnapshot};
            if self.loading.get() {
                return;
            }
            let sender = self.sender.get().unwrap();
            if let Some(info) = self.songlist.borrow().as_ref() {
                if matches!(*self.page_type.borrow(), Some(DiscoverSubPage::SongList)) {
                    let _ = sender.send_blocking(Action::RequestCollectionDownload(info.id));
                } else {
                    let songs = self.songs_list.get_songinfo_list();
                    let snapshot = CollectionSnapshot {
                        id: info.id,
                        name: info.name.clone(),
                        kind: "album".into(),
                        items: songs
                            .iter()
                            .map(|song| CollectionItem {
                                song_id: song.id,
                                song: Some(song.clone()),
                                error: None,
                            })
                            .collect(),
                        duplicates: 0,
                    };
                    let _ = sender.send_blocking(Action::RequestDownload(songs, Some(snapshot)));
                }
            }
        }
        #[template_callback]
        fn play_button_clicked_cb(&self) {
            let sender = self.sender.get().unwrap();
            let playlist = self.songs_list.get_songinfo_list();
            if !playlist.is_empty() {
                sender
                    .send_blocking(Action::AddPlayList(playlist, true))
                    .unwrap();
            } else {
                sender
                    .send_blocking(Action::AddToast(gettext("This is an empty song list！")))
                    .unwrap();
            }
        }

        #[template_callback]
        fn sort_button_clicked_cb(&self) {
            // 翻转排序状态：false=最新优先，true=最早优先
            let asc = !self.radio_asc.get();
            self.radio_asc.set(asc);
            let btn = self.sort_button.get();
            // 更新按钮提示，使其始终描述“下一次点击”的行为
            if asc {
                btn.set_tooltip_text(Some(&gettext("Switch to newest first")));
            } else {
                btn.set_tooltip_text(Some(&gettext("Switch to oldest first")));
            }
            // active 状态经由 constructed() 中的绑定驱动排序图标切换
            btn.set_active(asc);
            self.obj().render_radio();
        }

        #[template_callback]
        fn like_button_clicked_cb(&self) {
            let page_type = &*self.page_type.borrow();
            if let Some(pt) = page_type {
                let sender = self.sender.get().unwrap();
                if let Some(songlist) = &*self.songlist.borrow() {
                    let s = glib::SendWeakRef::from(self.obj().downgrade());
                    let like = self.like.get();
                    let cb = Arc::new(move |_| {
                        if let Some(s) = s.upgrade() {
                            s.set_property("like", !like);
                        }
                    });
                    match pt {
                        DiscoverSubPage::SongList => sender
                            .send_blocking(Action::LikeSongList(songlist.id, !like, Some(cb)))
                            .unwrap(),
                        DiscoverSubPage::Album => sender
                            .send_blocking(Action::LikeAlbum(songlist.id, !like, Some(cb)))
                            .unwrap(),
                        DiscoverSubPage::Radio => sender
                            .send_blocking(Action::AddToast(gettext(
                                "Favorite radio stations are not supported!",
                            )))
                            .unwrap(),
                    }
                }
            }
        }
    }

    impl ObjectImpl for SonglistPage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.bind_property("like", &self.like_button.get(), "icon_name")
                .transform_to(|_, v: bool| {
                    Some(
                        (if v {
                            "starred-symbolic"
                        } else {
                            "non-starred-symbolic"
                        })
                        .to_string(),
                    )
                })
                .build();

            // 根据按钮 active 状态自动切换升/降序图标：
            // active => 最早优先（升序图标），inactive => 最新优先（降序图标）
            self.sort_button
                .get()
                .bind_property("active", &self.sort_button.get(), "icon_name")
                .transform_to(|_, active: bool| {
                    Some(
                        (if active {
                            "view-sort-ascending-symbolic"
                        } else {
                            "view-sort-descending-symbolic"
                        })
                        .to_string(),
                    )
                })
                .sync_create()
                .build();

            // 加载状态由 set_property("loading") 直接驱动 Spinner 的旋转显示，
            // 不依赖属性绑定，避免手动 set_property 未触发 notify 导致动画不刷新。
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecBoolean::builder("like").readwrite().build(),
                    ParamSpecBoolean::builder("loading").readwrite().build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &Value, pspec: &ParamSpec) {
            match pspec.name() {
                "like" => {
                    let like = value.get().expect("The value needs to be of type `bool`.");
                    self.like.replace(like);
                }
                "loading" => {
                    let loading = value.get().expect("The value needs to be of type `bool`.");
                    self.loading.replace(loading);
                    self.loading_spinner.get().set_spinning(loading);
                    self.download_all.set_sensitive(!loading);
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> Value {
            match pspec.name() {
                "like" => self.like.get().to_value(),
                "loading" => self.loading.get().to_value(),
                _ => unimplemented!(),
            }
        }
    }
    impl WidgetImpl for SonglistPage {}
    impl BoxImpl for SonglistPage {}
}
