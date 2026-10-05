mod common;
use adw::prelude::*;
use gtk::gio;
use netease_cloud_music_gtk4::{download::model::*, download_dialog::*};
#[test]
fn native_dialog_defaults_selection_and_saved_preferences() {
    adw::init().expect("UI verification requires an accessible display");
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
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
    let space = dialog.builder.object::<gtk::Label>("space").unwrap();
    space.set_label("stale previous folder");
    dialog.update_space();
    assert_ne!(space.text().as_str(), "stale previous folder");
    dialog.set_total_duration(10000);
    assert!(
        dialog
            .builder
            .object::<gtk::Label>("estimate")
            .unwrap()
            .text()
            .contains("0.2 MB")
    );
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
    let (manager, _events) =
        netease_cloud_music_gtk4::download::manager::DownloadManager::new_with_backend(
            temp.0.join("catalog"),
            temp.0.join("cache"),
            std::sync::Arc::new(UiBackend),
        )
        .unwrap();
    let page = netease_cloud_music_gtk4::downloads_page::DownloadsPage::new(manager.clone());
    assert!(
        page.builder
            .object::<adw::StatusPage>("tasks_empty")
            .unwrap()
            .is_visible()
    );
    assert_eq!(
        page.builder
            .object::<gtk::Stack>("views")
            .unwrap()
            .visible_child_name()
            .as_deref(),
        Some("tasks")
    );
    assert_eq!(
        page.builder
            .object::<gtk::ListBox>("files")
            .unwrap()
            .first_child(),
        None
    );
    let target_path = temp.0.join("existing");
    std::fs::create_dir(&target_path).unwrap();
    std::fs::write(target_path.join("Example.mp3"), b"existing MP3").unwrap();
    std::fs::write(target_path.join("Example.lrc"), b"[00:01.00]example").unwrap();
    let target = TargetLocation::capture(&target_path, false, None, None).unwrap();
    let items = netease_cloud_music_gtk4::download::storage::scan(&target).unwrap();
    manager.import_files(target, items).unwrap();
    page.apply_event(DownloadEvent::Changed(manager.snapshot()));
    let window = adw::Window::builder()
        .default_width(800)
        .default_height(600)
        .content(&page.widget)
        .build();
    window.present();
    spin(150);
    page.builder
        .object::<gtk::Stack>("views")
        .unwrap()
        .set_visible_child_name("saved");
    let list = page.builder.object::<gtk::ListBox>("files").unwrap();
    let selection = find_check(list.first_child().unwrap()).unwrap();
    selection.set_active(true);
    assert!(
        page.builder
            .object::<gtk::Label>("file_count")
            .unwrap()
            .text()
            .contains("1 selected")
    );
    selection.set_active(false);
    page.builder
        .object::<gtk::Button>("select_all")
        .unwrap()
        .emit_clicked();
    page.builder
        .object::<gtk::Button>("delete")
        .unwrap()
        .emit_clicked();
    let choices = window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(choices.heading().as_deref(), Some("Delete saved files"));
    choices.emit_by_name::<()>("response", &[&"preview"]);
    choices.close();
    spin(350);
    let confirmation = window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(
        confirmation.heading().as_deref(),
        Some("Confirm file deletion")
    );
    assert_eq!(confirmation.default_response().as_deref(), Some("cancel"));
    let file_text = labels(confirmation.extra_child().unwrap());
    assert!(file_text.contains(&target_path.join("Example.mp3").display().to_string()));
    assert!(file_text.contains(&target_path.join("Example.lrc").display().to_string()));
    assert!(target_path.join("Example.mp3").exists());
    screenshot(&window, "delete-preview");
    confirmation.close();
    spin(300);
    screenshot(&window, "downloads-light");
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    window.set_default_size(540, 560);
    spin(150);
    screenshot(&window, "downloads-dark-small");
    let visual = DownloadDialog::new(prefs, 2, None);
    visual.present_for(&window);
    spin(350);
    screenshot(&window, "download-dialog-dark-small");
    visual.dialog.close();
    spin(200);
    let locale_dir = temp.0.join("locale");
    let messages = locale_dir.join("zh_CN/LC_MESSAGES");
    std::fs::create_dir_all(&messages).unwrap();
    assert!(
        std::process::Command::new("msgfmt")
            .arg(source.parent().unwrap().join("po/zh_CN.po"))
            .arg("-o")
            .arg(messages.join("netease-cloud-music-gtk4.mo"))
            .status()
            .unwrap()
            .success()
    );
    gettextrs::bindtextdomain("netease-cloud-music-gtk4", &locale_dir).unwrap();
    gettextrs::textdomain("netease-cloud-music-gtk4").unwrap();
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "zh_CN.UTF-8").unwrap();
    let chinese = DownloadDialog::new(DownloadPreferences::default(), 2, None);
    assert_eq!(
        chinese
            .builder
            .object::<gtk::Button>("submit")
            .unwrap()
            .label()
            .as_deref(),
        Some("加入下载队列")
    );
    chinese.set_total_duration(20000);
    chinese.present_for(&window);
    spin(300);
    screenshot(&window, "download-dialog-zh");
    chinese.dialog.close();
    window.close();
    let target = TargetLocation::capture(&target_path, false, None, None).unwrap();
    let mut queue_prefs = DownloadPreferences::default();
    queue_prefs.content = DownloadContent::Lyrics;
    let options = queue_prefs.with_target(target).unwrap();
    manager
        .enqueue(
            vec![common::song(77)],
            options.clone(),
            None,
            ncm_api::MusicApi::new(2),
        )
        .unwrap();
    page.apply_event(DownloadEvent::Changed(manager.snapshot()));
    let task_row = page
        .builder
        .object::<gtk::ListBox>("tasks")
        .unwrap()
        .first_child()
        .unwrap();
    spin(300);
    page.apply_event(DownloadEvent::Changed(manager.snapshot()));
    assert_eq!(
        page.builder
            .object::<gtk::ListBox>("tasks")
            .unwrap()
            .first_child()
            .unwrap(),
        task_row
    );
    manager
        .enqueue(
            vec![common::song(78)],
            options,
            None,
            ncm_api::MusicApi::new(2),
        )
        .unwrap();
    window.set_content(None::<&gtk::Widget>);
    drop(page);
    spin(300);
    assert!(
        manager
            .snapshot()
            .tasks
            .iter()
            .all(|t| t.stage == TaskStage::Complete)
    );
    manager.shutdown();
}

fn spin(ms: u64) {
    let context = gtk::glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
fn labels(widget: gtk::Widget) -> String {
    let mut text = widget
        .clone()
        .downcast::<gtk::Label>()
        .map(|l| l.text().to_string())
        .unwrap_or_default();
    let mut child = widget.first_child();
    while let Some(w) = child {
        text.push_str(&labels(w.clone()));
        child = w.next_sibling();
    }
    text
}
fn screenshot(window: &adw::Window, name: &str) {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
    let node = snapshot.to_node().unwrap();
    let texture = window.renderer().unwrap().render_texture(&node, None);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/download-ui");
    std::fs::create_dir_all(&dir).unwrap();
    texture
        .save_to_png(dir.join(format!("{name}.png")))
        .unwrap();
}

struct UiBackend;
impl netease_cloud_music_gtk4::download::worker::DownloadBackend for UiBackend {
    fn run(
        &self,
        task: &TaskRecord,
        _: &ncm_api::MusicApi,
        temp: &std::path::Path,
        _: &std::sync::atomic::AtomicBool,
        _: &dyn Fn(TaskStage, f64),
        _: &std::sync::Mutex<()>,
    ) -> anyhow::Result<Vec<netease_cloud_music_gtk4::download::worker::PreparedOutput>> {
        std::thread::sleep(std::time::Duration::from_millis(120));
        let path = temp.join("lyrics.lrc");
        std::fs::write(&path, b"[00:01.00]lyrics")?;
        Ok(vec![
            netease_cloud_music_gtk4::download::worker::PreparedOutput::ready(
                &task.outputs[0].suffix,
                path,
            ),
        ])
    }
}

fn find_check(widget: gtk::Widget) -> Option<gtk::CheckButton> {
    if let Ok(check) = widget.clone().downcast::<gtk::CheckButton>() {
        return Some(check);
    }
    let mut child = widget.first_child();
    while let Some(w) = child {
        if let Some(check) = find_check(w.clone()) {
            return Some(check);
        }
        child = w.next_sibling();
    }
    None
}
