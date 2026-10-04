use crate::download::model::*;
use adw::prelude::*;
use anyhow::{Result, ensure};
use gettextrs::gettext;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
};
type Submit = Box<dyn Fn(DownloadOptions)>;
type Save = Box<dyn Fn(DownloadPreferences)>;
struct Data {
    prefs: RefCell<DownloadPreferences>,
    submit: RefCell<Vec<Submit>>,
    save: RefCell<Vec<Save>>,
    count: usize,
    duration: Cell<u64>,
}
#[derive(Clone)]
pub struct DownloadDialog {
    pub dialog: adw::Dialog,
    pub builder: gtk::Builder,
    data: Rc<Data>,
}
fn find(root: &gtk::Widget, id: &str) -> Option<gtk::Widget> {
    if root.buildable_id().as_deref() == Some(id) {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find(&widget, id) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}
fn object<T: IsA<gtk::Widget>>(root: &adw::Dialog, id: &str) -> T {
    root.child()
        .and_then(|child| find(&child, id))
        .unwrap_or_else(|| panic!("Missing download widget {id}"))
        .downcast()
        .unwrap_or_else(|_| panic!("Wrong download widget type {id}"))
}
pub fn load_preferences(settings: &gio::Settings) -> DownloadPreferences {
    if settings
        .settings_schema()
        .is_some_and(|s| s.has_key("download-settings"))
    {
        serde_json::from_str(&settings.string("download-settings")).unwrap_or_default()
    } else {
        DownloadPreferences::default()
    }
}
pub fn save_preferences(settings: &gio::Settings, prefs: &DownloadPreferences) -> Result<()> {
    prefs.validate()?;
    if settings
        .settings_schema()
        .is_some_and(|s| s.has_key("download-settings"))
    {
        settings.set_string("download-settings", &serde_json::to_string(prefs)?)?;
    }
    Ok(())
}
pub fn capture_folder(
    path: &Path,
    removable_hint: bool,
    expected_uuid: Option<String>,
) -> Result<TargetLocation> {
    let file = gio::File::for_path(path);
    let mount = file.find_enclosing_mount(None::<&gio::Cancellable>).ok();
    let removable = removable_hint
        || mount.as_ref().is_some_and(|m| {
            (m.can_eject() || m.can_unmount())
                || m.volume()
                    .and_then(|v| v.drive())
                    .is_some_and(|d| d.is_removable() || d.is_media_removable())
        });
    let uuid = mount.as_ref().and_then(|m| m.uuid()).map(|s| s.to_string());
    if removable {
        ensure!(
            path.is_dir() && mount.is_some(),
            "Save location is unavailable; choose the mounted folder again"
        );
        if let Some(expected) = &expected_uuid {
            ensure!(
                uuid.as_ref() == Some(expected),
                "A different storage device is mounted here; choose the folder again"
            );
        }
    } else {
        std::fs::create_dir_all(path)?;
    }
    TargetLocation::capture(
        path,
        removable,
        uuid,
        mount.map(|m| m.root().uri().to_string()),
    )
}
impl DownloadDialog {
    pub fn new(
        initial: DownloadPreferences,
        count: usize,
        collection: Option<CollectionSnapshot>,
    ) -> Self {
        let builder = gtk::Builder::new();
        builder.set_translation_domain(Some("netease-cloud-music-gtk4"));
        builder
            .add_from_resource("/com/gitee/gmg137/NeteaseCloudMusicGtk4/gtk/download-dialog.ui")
            .expect("Download dialog resource");
        let dialog: adw::Dialog = builder.object("download_dialog").unwrap();
        let data = Rc::new(Data {
            prefs: RefCell::new(initial.clone()),
            submit: RefCell::new(vec![]),
            save: RefCell::new(vec![]),
            count,
            duration: Cell::new(0),
        });
        object::<adw::ComboRow>(&dialog, "content").set_selected(match initial.content {
            DownloadContent::AudioAndLyrics => 0,
            DownloadContent::Audio => 1,
            DownloadContent::Lyrics => 2,
        });
        object::<adw::ComboRow>(&dialog, "bitrate").set_selected(match initial.bitrate {
            128 => 0,
            320 => 2,
            _ => 1,
        });
        object::<adw::ComboRow>(&dialog, "lyric_mode").set_selected(match initial.lyric_mode {
            LyricMode::Original => 0,
            LyricMode::Separate => 1,
            LyricMode::Bilingual => 2,
        });
        object::<adw::ComboRow>(&dialog, "encoding").set_selected(
            if initial.encoding == TextEncoding::Gbk {
                1
            } else {
                0
            },
        );
        for (id, f) in [
            ("lrc", LyricFormat::Lrc),
            ("txt", LyricFormat::Txt),
            ("srt", LyricFormat::Srt),
        ] {
            object::<gtk::CheckButton>(&dialog, id).set_active(initial.lyric_formats.contains(&f));
        }
        object::<adw::SwitchRow>(&dialog, "collection_folder")
            .set_active(initial.collection_folder);
        object::<adw::SwitchRow>(&dialog, "numbered").set_active(initial.numbered);
        object::<adw::ActionRow>(&dialog, "location")
            .set_subtitle(&initial.target_path.display().to_string());
        object::<adw::WindowTitle>(&dialog, "heading").set_subtitle(&format!(
            "{} {}",
            count,
            gettext("songs")
        ));
        object::<adw::PreferencesGroup>(&dialog, "batch_group")
            .set_visible(count > 1 || count == 0);
        if count == 0 {
            object::<adw::WindowTitle>(&dialog, "heading").set_title(&gettext("Download defaults"));
            object::<gtk::Button>(&dialog, "submit").set_label(&gettext("Save"));
        }
        if let Some(collection) = collection {
            if collection.duplicates > 0 {
                object::<adw::WindowTitle>(&dialog, "heading").set_subtitle(&format!(
                    "{} {} · {} {}",
                    count,
                    gettext("songs"),
                    collection.duplicates,
                    gettext("duplicates removed")
                ));
            }
        }
        let weak = dialog.downgrade();
        let shared = data.clone();
        object::<adw::ComboRow>(&dialog, "bitrate").connect_selected_notify(move |_| {
            if let Some(dialog) = weak.upgrade() {
                update_estimate(&dialog, &shared);
            }
        });
        update_visibility(&dialog);
        let weak = dialog.downgrade();
        let shared = data.clone();
        object::<adw::ComboRow>(&dialog, "content").connect_selected_notify(move |_| {
            if let Some(dialog) = weak.upgrade() {
                update_visibility(&dialog);
                update_estimate(&dialog, &shared);
            }
        });
        let weak = dialog.downgrade();
        let shared = data.clone();
        object::<gtk::Button>(&dialog, "choose_folder").connect_clicked(move |_| {
            let Some(dialog) = weak.upgrade() else { return };
            let chooser = gtk::FileDialog::builder()
                .title(gettext("Choose save folder"))
                .build();
            let parent = dialog.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let weak = dialog.downgrade();
            let shared = shared.clone();
            chooser.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
                if let (Ok(file), Some(dialog)) = (result, weak.upgrade()) {
                    if let Some(path) = file.path() {
                        let mut prefs = shared.prefs.borrow_mut();
                        prefs.target_path = path;
                        prefs.target_removable = file
                            .find_enclosing_mount(None::<&gio::Cancellable>)
                            .ok()
                            .is_some_and(|m| {
                                (m.can_eject() || m.can_unmount())
                                    || m.volume()
                                        .and_then(|v| v.drive())
                                        .is_some_and(|d| d.is_removable() || d.is_media_removable())
                            });
                        prefs.target_uuid = file
                            .find_enclosing_mount(None::<&gio::Cancellable>)
                            .ok()
                            .and_then(|m| m.uuid())
                            .map(|s| s.to_string());
                        object::<adw::ActionRow>(&dialog, "location")
                            .set_subtitle(&prefs.target_path.display().to_string());
                    }
                }
            });
        });
        let weak = dialog.downgrade();
        let shared = data.clone();
        object::<gtk::Button>(&dialog, "submit").connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                submit(&dialog, shared.clone());
            }
        });
        Self {
            dialog,
            builder,
            data,
        }
    }
    pub fn set_total_duration(&self, ms: u64) {
        self.data.duration.set(ms);
        update_estimate(&self.dialog, &self.data);
    }
    pub fn read_preferences(&self) -> Result<DownloadPreferences> {
        read_preferences(&self.dialog, &self.data)
    }
    pub fn connect_submit(&self, callback: impl Fn(DownloadOptions) + 'static) {
        self.data.submit.borrow_mut().push(Box::new(callback));
    }
    pub fn connect_save(&self, callback: impl Fn(DownloadPreferences) + 'static) {
        self.data.save.borrow_mut().push(Box::new(callback));
    }
    pub fn present_for(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
        self.update_space();
    }
    pub fn update_space(&self) {
        let prefs = self.data.prefs.borrow().clone();
        let weak = self.dialog.downgrade();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let value = rustix::fs::statvfs(&prefs.target_path)
                .map(|s| s.f_bavail.saturating_mul(s.f_frsize));
            let _ = tx.send_blocking(value.ok());
        });
        glib::MainContext::default().spawn_local(async move {
            if let (Ok(Some(bytes)), Some(dialog)) = (rx.recv().await, weak.upgrade()) {
                object::<gtk::Label>(&dialog, "space").set_label(&format!(
                    "{}: {:.1} GB",
                    gettext("Available space"),
                    bytes as f64 / 1e9
                ));
            }
        });
    }
}
fn update_visibility(dialog: &adw::Dialog) {
    let selection = object::<adw::ComboRow>(dialog, "content").selected();
    object::<adw::ComboRow>(dialog, "bitrate").set_visible(selection != 2);
    object::<adw::PreferencesGroup>(dialog, "lyrics_group").set_visible(selection != 1);
}
fn read_preferences(dialog: &adw::Dialog, data: &Data) -> Result<DownloadPreferences> {
    let mut prefs = data.prefs.borrow().clone();
    prefs.content = match object::<adw::ComboRow>(dialog, "content").selected() {
        0 => DownloadContent::AudioAndLyrics,
        1 => DownloadContent::Audio,
        _ => DownloadContent::Lyrics,
    };
    prefs.bitrate = [128, 192, 320]
        .get(object::<adw::ComboRow>(dialog, "bitrate").selected() as usize)
        .copied()
        .unwrap_or(192);
    prefs.lyric_formats = [
        ("lrc", LyricFormat::Lrc),
        ("txt", LyricFormat::Txt),
        ("srt", LyricFormat::Srt),
    ]
    .into_iter()
    .filter(|(id, _)| object::<gtk::CheckButton>(dialog, id).is_active())
    .map(|(_, f)| f)
    .collect();
    prefs.lyric_mode = match object::<adw::ComboRow>(dialog, "lyric_mode").selected() {
        0 => LyricMode::Original,
        1 => LyricMode::Separate,
        _ => LyricMode::Bilingual,
    };
    prefs.encoding = if object::<adw::ComboRow>(dialog, "encoding").selected() == 1 {
        TextEncoding::Gbk
    } else {
        TextEncoding::Utf8
    };
    prefs.collection_folder = object::<adw::SwitchRow>(dialog, "collection_folder").is_active();
    prefs.numbered = object::<adw::SwitchRow>(dialog, "numbered").is_active();
    Ok(prefs)
}
fn submit(dialog: &adw::Dialog, data: Rc<Data>) {
    let prefs = read_preferences(dialog, &data).and_then(|p| {
        p.validate()?;
        Ok(p)
    });
    let prefs = match prefs {
        Ok(p) => p,
        Err(e) => {
            show_error(dialog, &e.to_string());
            return;
        }
    };
    if data.count == 0 {
        for callback in data.save.borrow().iter() {
            callback(prefs.clone());
        }
        dialog.close();
        return;
    }
    object::<gtk::Button>(dialog, "submit").set_sensitive(false);
    dialog.set_can_close(false);
    let weak = dialog.downgrade();
    let (tx, rx) = async_channel::bounded(1);
    let worker_prefs = prefs.clone();
    std::thread::spawn(move || {
        let result = capture_folder(
            &worker_prefs.target_path,
            worker_prefs.target_removable,
            worker_prefs.target_uuid.clone(),
        )
        .and_then(|target| worker_prefs.with_target(target));
        let _ = tx.send_blocking(result);
    });
    glib::MainContext::default().spawn_local(async move {
        if let (Ok(result), Some(dialog)) = (rx.recv().await, weak.upgrade()) {
            object::<gtk::Button>(&dialog, "submit").set_sensitive(true);
            dialog.set_can_close(true);
            match result {
                Ok(options) => {
                    for callback in data.save.borrow().iter() {
                        callback(prefs.clone());
                    }
                    for callback in data.submit.borrow().iter() {
                        callback(options.clone());
                    }
                    dialog.close();
                }
                Err(e) => show_error(&dialog, &e.to_string()),
            }
        }
    });
}
fn show_error(dialog: &adw::Dialog, message: &str) {
    let label = object::<gtk::Label>(dialog, "error");
    label.set_label(&gettext(message));
    label.set_visible(true);
}

fn update_estimate(dialog: &adw::Dialog, data: &Data) {
    let bitrate =
        [128, 192, 320][object::<adw::ComboRow>(dialog, "bitrate").selected().min(2) as usize];
    let label = object::<gtk::Label>(dialog, "estimate");
    label.set_visible(
        data.duration.get() > 0 && object::<adw::ComboRow>(dialog, "content").selected() != 2,
    );
    label.set_label(&format!(
        "{}: {:.1} MB · {}",
        gettext("Estimated MP3 size"),
        data.duration.get() as f64 * bitrate as f64 / 8e6,
        gettext("Excludes lyrics, tags and temporary files")
    ));
}
