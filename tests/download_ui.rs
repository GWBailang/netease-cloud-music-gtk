mod common;
use adw::prelude::*;
use gtk::{gio, prelude::*};
use netease_cloud_music_gtk4::{download::model::*, download_dialog::*};
#[test]
fn native_dialog_defaults_selection_and_saved_preferences() {
    adw::init().expect("UI verification requires an accessible display");
    let temp = common::TempDir::new();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    assert!(
        std::process::Command::new("glib-compile-resources")
            .arg(source.join("netease_cloud_music_gtk4.gresource.xml"))
            .arg(format!("--sourcedir={}", source.display()))
            .arg(format!(
                "--target={}",
                temp.0.join("test.gresource").display()
            ))
            .status()
            .unwrap()
            .success()
    );
    gio::resources_register(&gio::Resource::load(temp.0.join("test.gresource")).unwrap());
    assert!(
        std::process::Command::new("glib-compile-schemas")
            .arg(&source)
            .arg(format!("--targetdir={}", temp.0.display()))
            .status()
            .unwrap()
            .success()
    );
    let schemas = gio::SettingsSchemaSource::from_directory(&temp.0, None, true).unwrap();
    let schema = schemas
        .lookup("com.gitee.gmg137.NeteaseCloudMusicGtk4", false)
        .unwrap();
    let backend = gio::memory_settings_backend_new();
    let settings = gio::Settings::new_full(&schema, Some(&backend), None);
    let prefs = load_preferences(&settings);
    assert_eq!(prefs.bitrate, 192);
    assert_eq!(prefs.lyric_formats, vec![LyricFormat::Lrc]);
    let dialog = DownloadDialog::new(prefs.clone(), 1, None);
    let content = dialog.builder.object::<adw::ComboRow>("content").unwrap();
    content.set_selected(2);
    for id in ["lrc", "txt", "srt"] {
        dialog
            .builder
            .object::<gtk::CheckButton>(id)
            .unwrap()
            .set_active(false);
    }
    assert!(dialog.read_preferences().unwrap().validate().is_err());
    assert!(
        !dialog
            .builder
            .object::<adw::ComboRow>("bitrate")
            .unwrap()
            .is_visible()
    );
    dialog
        .builder
        .object::<gtk::CheckButton>("txt")
        .unwrap()
        .set_active(true);
    let selected = dialog.read_preferences().unwrap();
    assert_eq!(selected.lyric_formats, vec![LyricFormat::Txt]);
    assert_eq!(prefs.bitrate, 192);
    save_preferences(&settings, &selected).unwrap();
    assert_eq!(load_preferences(&settings).content, DownloadContent::Lyrics);
}
