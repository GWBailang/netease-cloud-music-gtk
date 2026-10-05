mod common;
use gstreamer::{self as gst, prelude::*};
use ncm_api::{DownloadResource, MusicApi};
use netease_cloud_music_gtk4::download::{
    audio, lyrics::LyricDocument, manager::DownloadManager, model::*, storage, worker::*,
};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
struct Sources {
    base: String,
}
impl DownloadService for Sources {
    fn resource(&self, _: &MusicApi, id: u64, _: u32) -> anyhow::Result<DownloadResource> {
        DownloadResource::parse(
            &serde_json::json!({"id":id,"url":format!("{}/{}",self.base,id),"type":if id==1{"aac"}else{"flac"},"br":192000,"freeTrialInfo":null}),
            4644,
        )
    }
    fn lyrics(&self, _: &MusicApi, id: u64) -> anyhow::Result<LyricDocument> {
        Ok(if id == 1 {
            LyricDocument::parse(
                &["[00:01.00]原文🎵".into()],
                &["[00:01.00]translation".into()],
            )
        } else {
            LyricDocument::parse(&[], &[])
        })
    }
}
fn fixture(path: &Path, encoder: &str) {
    gst::init().unwrap();
    let pipeline=gst::parse::launch(&format!("audiotestsrc num-buffers=200 ! audio/x-raw,rate=44100,channels=2 ! audioconvert ! {encoder} ! filesink location=\"{}\"",path.display())).unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let message = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(15),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    pipeline.set_state(gst::State::Null).unwrap();
    assert!(matches!(message.view(), gst::MessageView::Eos(_)));
}
fn wait(manager: &DownloadManager) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if manager.snapshot().tasks.iter().all(|t| t.stage.terminal()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("Downloads did not finish");
}
#[test]
fn batch_streams_converts_exports_and_deletes_without_affecting_other_copies() {
    let temp = common::TempDir::new();
    let aac = temp.0.join("sample.aac");
    let flac = temp.0.join("sample.flac");
    fixture(&aac, "avenc_aac ! aacparse ! audio/mpeg,stream-format=adts");
    fixture(&flac, "flacenc");
    let aac = fs::read(aac).unwrap();
    let flac = fs::read(flac).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for stream in listener.incoming().take(3) {
            let mut stream = stream.unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            let bytes = if request
                .split_whitespace()
                .nth(1)
                .is_some_and(|url| url.ends_with("/1"))
            {
                &aac
            } else {
                &flac
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(bytes).unwrap();
        }
    });
    let mut client = MusicApi::new(2);
    client.set_proxy(&base).unwrap();
    let backend = Arc::new(ServiceBackend::new(Arc::new(Sources { base })));
    let (manager, _) =
        DownloadManager::new_with_backend(temp.0.join("catalog"), temp.0.join("cache"), backend)
            .unwrap();
    let root = temp.0.join("music");
    fs::create_dir(&root).unwrap();
    let mut prefs = DownloadPreferences::default();
    prefs.numbered = true;
    prefs.lyric_formats = vec![LyricFormat::Lrc, LyricFormat::Txt, LyricFormat::Srt];
    prefs.lyric_mode = LyricMode::Bilingual;
    let options = prefs
        .with_target(TargetLocation::capture(&root, false, None, None).unwrap())
        .unwrap();
    let songs: Vec<_> = (1..=2)
        .map(|id| {
            let mut song = common::song(id);
            song.duration = 4644;
            song.name = format!("Song {id}");
            song
        })
        .collect();
    let collection = CollectionSnapshot {
        id: 99,
        name: "My playlist".into(),
        kind: "playlist".into(),
        items: songs
            .iter()
            .map(|s| CollectionItem {
                song_id: s.id,
                song: Some(s.clone()),
                error: None,
            })
            .collect(),
        duplicates: 0,
    };
    manager
        .enqueue_collection(collection, options.clone(), client.clone())
        .unwrap();
    wait(&manager);
    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot.saved.len(),
        2,
        "{:?}",
        snapshot
            .tasks
            .iter()
            .map(|t| (t.song_id, t.stage, &t.error))
            .collect::<Vec<_>>()
    );
    assert!(snapshot.tasks.iter().all(|task| task.source_info.is_some()));
    let first = snapshot
        .saved
        .iter()
        .find(|s| s.song.as_ref().unwrap().id == 1)
        .unwrap();
    assert_eq!(first.files.len(), 4);
    let mp3 = first
        .files
        .iter()
        .find(|f| f.kind == FileKind::Audio)
        .unwrap();
    let audio = audio::probe(&root.join(&mp3.relative)).unwrap();
    assert!(audio.mp3 && audio.constant_bitrate);
    assert_eq!(audio.bitrate, 192000);
    assert_eq!(audio.sample_rate, 44100);
    assert_eq!(audio.channels, 2);
    let lrc = first
        .files
        .iter()
        .find(|f| f.relative.extension().is_some_and(|s| s == "lrc"))
        .unwrap();
    let text = fs::read_to_string(root.join(&lrc.relative)).unwrap();
    assert!(text.contains("原文") && text.contains("translation"));
    assert!(mp3.relative.to_string_lossy().contains("001 "));
    let second = snapshot.tasks.iter().find(|t| t.song_id == 2).unwrap();
    assert_eq!(second.stage, TaskStage::Complete);
    assert!(
        second
            .outputs
            .iter()
            .any(|o| o.state == OutputState::Unavailable)
    );
    assert!(
        second
            .outputs
            .iter()
            .any(|o| o.suffix == ".mp3" && o.state == OutputState::Saved)
    );
    let copy_root = temp.0.join("other-copy");
    fs::create_dir(&copy_root).unwrap();
    let mut copy_options = options;
    copy_options.target = TargetLocation::capture(&copy_root, false, None, None).unwrap();
    manager
        .enqueue(vec![songs[0].clone()], copy_options, None, client)
        .unwrap();
    wait(&manager);
    server.join().unwrap();
    let preview = storage::preview_delete(&[first.clone()], true, true).unwrap();
    let report = manager
        .delete_files(&preview, DeleteMode::Permanent)
        .unwrap();
    assert!(report.results.iter().all(|r| r.error.is_none()));
    let remaining = manager.snapshot();
    assert_eq!(remaining.saved.len(), 2);
    for saved in &remaining.saved {
        for file in &saved.files {
            assert!(saved.location.root.join(&file.relative).exists());
        }
    }
    assert!(!root.join(&mp3.relative).exists());
    manager.shutdown();
}
#[test]
fn gbk_failed_lyrics_can_retry_as_utf8() {
    for removable in [false, true] {
        let temp = common::TempDir::new();
        let root = temp.0.join("lyrics");
        fs::create_dir(&root).unwrap();
        let backend = Arc::new(ServiceBackend::new(Arc::new(Sources {
            base: String::new(),
        })));
        let (manager, _) = DownloadManager::new_with_backend(
            temp.0.join("catalog"),
            temp.0.join("cache"),
            backend,
        )
        .unwrap();
        let prefs = DownloadPreferences {
            content: DownloadContent::Lyrics,
            encoding: TextEncoding::Gbk,
            ..Default::default()
        };
        let options = prefs
            .with_target(TargetLocation::capture(&root, removable, None, None).unwrap())
            .unwrap();
        let id = manager
            .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
            .unwrap()[0];
        wait(&manager);
        assert_eq!(manager.snapshot().tasks[0].stage, TaskStage::Failed);
        assert_eq!(
            manager.snapshot().tasks[0].outputs[0].state,
            OutputState::Failed
        );
        let target = manager.snapshot().tasks[0].options.target.clone();
        if removable {
            manager.invalidate_location(&target.id);
        }
        manager
            .retry_confirmed(
                id,
                MusicApi::new(2),
                true,
                if removable {
                    Some((&target.id, &target))
                } else {
                    None
                },
            )
            .unwrap();
        wait(&manager);
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.tasks[0].stage, TaskStage::Complete);
        assert!(
            fs::read_to_string(root.join(&snapshot.saved[0].files[0].relative))
                .unwrap()
                .contains('🎵')
        );
        manager.shutdown();
    }
}
#[test]
fn independent_translation_survives_original_encoding_failure() {
    let temp = common::TempDir::new();
    let root = temp.0.join("lyrics");
    fs::create_dir(&root).unwrap();
    let backend = Arc::new(ServiceBackend::new(Arc::new(Sources {
        base: String::new(),
    })));
    let (manager, _) =
        DownloadManager::new_with_backend(temp.0.join("catalog"), temp.0.join("cache"), backend)
            .unwrap();
    let prefs = DownloadPreferences {
        content: DownloadContent::Lyrics,
        encoding: TextEncoding::Gbk,
        lyric_mode: LyricMode::Separate,
        ..Default::default()
    };
    let options = prefs
        .with_target(TargetLocation::capture(&root, false, None, None).unwrap())
        .unwrap();
    let id = manager
        .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
        .unwrap()[0];
    wait(&manager);
    let snapshot = manager.snapshot();
    assert_eq!(
        snapshot.tasks[0]
            .outputs
            .iter()
            .find(|o| o.suffix == ".translated.lrc")
            .unwrap()
            .state,
        OutputState::Saved
    );
    let before = snapshot.saved[0].files[0].fingerprint.clone();
    manager.retry_utf8(id, MusicApi::new(2)).unwrap();
    wait(&manager);
    let snapshot = manager.snapshot();
    assert_eq!(snapshot.tasks[0].stage, TaskStage::Complete);
    let translated = snapshot.saved[0]
        .files
        .iter()
        .find(|f| f.relative.to_string_lossy().ends_with(".translated.lrc"))
        .unwrap();
    assert_eq!(translated.fingerprint, before);
    let mut utf8 = snapshot.tasks[0].options.clone();
    utf8.encoding = TextEncoding::Utf8;
    manager
        .enqueue(vec![common::song(1)], utf8, None, MusicApi::new(2))
        .unwrap();
    wait(&manager);
    let later = manager.snapshot();
    assert_ne!(
        later.tasks[1].basename, later.tasks[0].basename,
        "A preserved GBK file must not count as UTF-8"
    );
    manager.shutdown();
}
