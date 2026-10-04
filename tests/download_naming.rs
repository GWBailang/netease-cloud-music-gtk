mod common;
use netease_cloud_music_gtk4::download::{model::*, naming::*};
#[test]
fn sanitizes_path_and_reserved_names() {
    assert_eq!(sanitize_component("CON"), "_CON");
    let result = sanitize_component("../CON:歌/曲\0 ");
    assert!(!result.contains(['/', '\\', ':', '\0']));
    assert!(!result.ends_with(['.', ' ']));
    assert_eq!(sanitize_component("..."), "Untitled");
}
#[test]
fn truncates_unicode_at_character_boundary() {
    let result = sanitize_component(&"音乐🎵".repeat(100));
    assert!(result.len() <= 180);
    assert!(!result.is_empty());
}
#[test]
fn numbers_from_snapshot() {
    assert_eq!(
        song_basename(&common::song(1), Some((1, 1000))),
        "0001 歌手 - 歌名"
    );
}
#[test]
fn rejects_empty_lyric_selection() {
    let mut prefs = DownloadPreferences::default();
    prefs.content = DownloadContent::Lyrics;
    prefs.lyric_formats.clear();
    assert!(prefs.validate().is_err());
    prefs.content = DownloadContent::Audio;
    assert!(prefs.validate().is_ok());
    prefs.bitrate = 200;
    assert!(prefs.validate().is_err());
}
