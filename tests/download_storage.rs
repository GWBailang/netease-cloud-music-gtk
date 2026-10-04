mod common;
use netease_cloud_music_gtk4::download::{catalog::*, model::*, storage::*};
use std::{fs, os::unix::fs::symlink, sync::atomic::AtomicBool};
fn location(temp: &common::TempDir) -> TargetLocation {
    TargetLocation::capture(&temp.0, false, None, None).unwrap()
}
fn saved(target: TargetLocation, file: StoredFile) -> SavedSong {
    SavedSong {
        key: "song".into(),
        location: target,
        song: Some(common::song(1)),
        title: "歌名".into(),
        bitrate: 192,
        encoding: TextEncoding::Utf8,
        lyric_mode: LyricMode::Original,
        files: vec![file],
        imported: false,
    }
}
#[test]
fn refuses_overwrite_escape_and_symlink() {
    let temp = common::TempDir::new();
    let source = common::TempDir::new();
    fs::write(source.0.join("input"), b"new").unwrap();
    fs::write(temp.0.join("song.mp3"), b"old").unwrap();
    let target = location(&temp);
    assert!(
        commit_file(
            &source.0.join("input"),
            &target,
            std::path::Path::new("song.mp3"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(fs::read(temp.0.join("song.mp3")).unwrap(), b"old");
    assert!(
        commit_file(
            &source.0.join("input"),
            &target,
            std::path::Path::new("../escape.mp3"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    symlink(&source.0, temp.0.join("link")).unwrap();
    assert!(
        commit_file(
            &source.0.join("input"),
            &target,
            std::path::Path::new("link/song.mp3"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
}
#[test]
fn cancelled_copy_and_changed_location_never_publish() {
    let temp = common::TempDir::new();
    let source = common::TempDir::new();
    fs::write(source.0.join("input"), b"data").unwrap();
    let mut target = location(&temp);
    assert!(
        commit_file(
            &source.0.join("input"),
            &target,
            std::path::Path::new("song.mp3"),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    target.root_inode += 1;
    assert!(validate_location(&target).is_err());
    assert!(!temp.0.join("song.mp3").exists());
}
#[test]
fn deletion_detects_external_replacement_and_preserves_other_copy() {
    let temp = common::TempDir::new();
    let source = common::TempDir::new();
    fs::write(source.0.join("input"), b"aaa").unwrap();
    let target = location(&temp);
    let file = commit_file(
        &source.0.join("input"),
        &target,
        std::path::Path::new("song.mp3"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let item = saved(target, file);
    let preview = preview_delete(&[item], true, true).unwrap();
    let modified = fs::metadata(temp.0.join("song.mp3"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(temp.0.join("song.mp3"), b"bbb").unwrap();
    fs::File::open(temp.0.join("song.mp3"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let report = execute_delete(&preview, DeleteMode::Permanent);
    assert!(report.results[0].error.is_some());
    assert!(temp.0.join("song.mp3").exists());
    assert!(source.0.join("input").exists());
}
#[test]
fn scans_associated_lyrics_without_following_links() {
    let temp = common::TempDir::new();
    let other = common::TempDir::new();
    fs::write(temp.0.join("song.mp3"), b"audio").unwrap();
    fs::write(temp.0.join("song.lrc"), b"lyrics").unwrap();
    fs::write(temp.0.join("song.translated.txt"), b"translation").unwrap();
    fs::write(other.0.join("other.mp3"), b"other").unwrap();
    symlink(&other.0, temp.0.join("link")).unwrap();
    let items = scan(&location(&temp)).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].files.len(), 3);
    let preview = preview_delete(&items, false, true).unwrap();
    assert_eq!(preview.items.len(), 2);
    let report = execute_delete(&preview, DeleteMode::Permanent);
    assert!(report.results.iter().all(|r| r.error.is_none()));
    assert!(temp.0.join("song.mp3").exists());
    assert!(other.0.join("other.mp3").exists());
}
#[test]
fn clearing_history_keeps_file_index_and_cleanup_records() {
    let temp = common::TempDir::new();
    let mut catalog = Catalog::default();
    catalog.pending_cleanup.push(CleanupFile {
        location: location(&temp),
        relative: "owned.part".into(),
    });
    fs::write(temp.0.join("song.mp3"), b"audio").unwrap();
    catalog.saved = scan(&location(&temp)).unwrap();
    catalog.clear_history();
    catalog.save(&temp.0.join("catalog.json")).unwrap();
    let loaded = Catalog::load(&temp.0.join("catalog.json")).unwrap();
    assert_eq!(loaded.saved.len(), 1);
    assert_eq!(loaded.pending_cleanup.len(), 1);
}
#[test]
fn refuses_deleted_or_replaced_directory() {
    let temp = common::TempDir::new();
    let root = temp.0.join("card");
    fs::create_dir(&root).unwrap();
    let target = TargetLocation::capture(&root, true, None, Some("session".into())).unwrap();
    fs::rename(&root, temp.0.join("removed-card")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(validate_location(&target).is_err());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}
#[test]
fn trash_failure_does_not_permanently_delete() {
    let temp = common::TempDir::new();
    fs::write(temp.0.join("song.mp3"), b"audio").unwrap();
    let items = scan(&location(&temp)).unwrap();
    let preview = preview_delete(&items, true, true).unwrap();
    let report = execute_delete_with_trash(&preview, DeleteMode::Trash, |_| {
        Err(anyhow::anyhow!("trash unavailable"))
    });
    assert!(report.results[0].error.is_some());
    assert!(temp.0.join("song.mp3").exists());
}
#[test]
fn partial_delete_reports_actual_results() {
    let temp = common::TempDir::new();
    fs::write(temp.0.join("a.mp3"), b"one").unwrap();
    fs::write(temp.0.join("b.mp3"), b"two").unwrap();
    let items = scan(&location(&temp)).unwrap();
    let preview = preview_delete(&items, true, true).unwrap();
    fs::write(temp.0.join("b.mp3"), b"changed").unwrap();
    let report = execute_delete(&preview, DeleteMode::Permanent);
    assert_eq!(
        report.results.iter().filter(|r| r.error.is_none()).count(),
        1
    );
    assert!(!temp.0.join("a.mp3").exists());
    assert!(temp.0.join("b.mp3").exists());
}
#[test]
fn tracks_owned_temporary_file_before_creating_it() {
    let temp = common::TempDir::new();
    let source = common::TempDir::new();
    fs::write(source.0.join("input"), b"audio").unwrap();
    let target = location(&temp);
    let calls = std::cell::RefCell::new(Vec::new());
    commit_file_tracked(
        &source.0.join("input"),
        &target,
        std::path::Path::new("song.mp3"),
        &AtomicBool::new(false),
        |relative, owned| {
            if owned {
                assert!(!temp.0.join(relative).exists());
            }
            calls.borrow_mut().push(owned);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(*calls.borrow(), vec![true, false]);
}
