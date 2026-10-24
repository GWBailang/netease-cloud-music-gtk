use crate::{
    application::Action,
    gui::{SongListView, SonglistRow},
};
use adw::prelude::*;
use gtk::{gio, subclass::prelude::ObjectSubclassIsExt};
#[test]
fn downloads_and_selection_keep_playback_separate() {
    adw::init().unwrap();
    let resource = std::env::temp_dir().join(format!("ncm-ui-{}.gresource", std::process::id()));
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
    assert!(
        std::process::Command::new("glib-compile-resources")
            .arg(source.join("netease_cloud_music_gtk4.gresource.xml"))
            .arg(format!("--sourcedir={}", source.display()))
            .arg(format!("--target={}", resource.display()))
            .status()
            .unwrap()
            .success()
    );
    gio::resources_register(&gio::Resource::load(&resource).unwrap());
    std::fs::remove_file(resource).unwrap();
    let (sender, receiver) = async_channel::unbounded();
    let mut song = crate::utils::empty_song_info();
    song.id = 123;
    song.name = "Example".into();
    let list = SongListView::new();
    list.set_sender(sender);
    list.init_new_list(&[song], &[false]);
    let row = list
        .list_box()
        .row_at_index(0)
        .unwrap()
        .downcast::<SonglistRow>()
        .unwrap();
    let window = adw::Window::builder()
        .default_width(900)
        .default_height(500)
        .content(&list)
        .build();
    window.present();
    let context = gtk::glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let bounds = row.download_button().compute_bounds(&row).unwrap();
    let controllers = row.observe_controllers();
    for i in 0..controllers.n_items() {
        if let Ok(gesture) = controllers.item(i).unwrap().downcast::<gtk::GestureClick>() {
            gesture.emit_by_name::<()>(
                "released",
                &[&1i32, &(bounds.x() as f64 + 5.), &(bounds.y() as f64 + 5.)],
            );
        }
    }
    assert!(receiver.try_recv().is_err());
    row.download_button().emit_clicked();
    assert!(
        matches!(receiver.try_recv(),Ok(Action::RequestDownload(songs,None)) if songs[0].id==123)
    );
    assert!(receiver.try_recv().is_err());
    list.set_selection_mode(true);
    row.emit_activate();
    assert_eq!(list.selected_songs().len(), 1);
    assert!(receiver.try_recv().is_err());
    list.download_selected();
    assert!(
        matches!(receiver.try_recv(),Ok(Action::RequestDownload(songs,None)) if songs.len()==1)
    );
    assert!(receiver.try_recv().is_err());
    list.set_selection_mode(false);
    assert!(list.selected_songs().is_empty());
    row.set_activatable(true);
    row.emit_activate();
    assert!(matches!(receiver.try_recv(),Ok(Action::AddPlay(song)) if song.id==123));
    let quitting = std::rc::Rc::new(std::cell::Cell::new(false));
    let signal = quitting.clone();
    crate::window::connect_graceful_close(&window, move || signal.set(true));
    window.set_hide_on_close(true);
    window.close();
    assert!(!quitting.get());
    assert!(!window.is_visible());
    window.present();
    window.set_hide_on_close(false);
    window.close();
    assert!(quitting.get());
    assert!(
        window.is_visible(),
        "close must wait for asynchronous download shutdown"
    );
    let lyrics = crate::gui::PlayListLyricsPage::new();
    let (sender, receiver) = async_channel::unbounded();
    lyrics.set_sender(sender);
    lyrics.update_lyrics(vec![(1000, "old line\n".into())]);
    lyrics.update_lyrics_text("Loading lyrics...");
    assert!(lyrics.imp().current_lyrics.read().unwrap().is_empty());
    lyrics.imp().refresh_lyrics.emit_clicked();
    assert!(matches!(receiver.try_recv(), Ok(Action::RefreshLyrics)));
    window.set_content(Some(&lyrics));
    window.set_default_size(1150, 600);
    window.present();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let paintable = gtk::WidgetPaintable::new(Some(&window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
    let node = snapshot.to_node().unwrap();
    let texture = window.renderer().unwrap().render_texture(&node, None);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/download-ui");
    std::fs::create_dir_all(&dir).unwrap();
    texture.save_to_png(dir.join("lyrics-refresh.png")).unwrap();
    window.destroy();
}
