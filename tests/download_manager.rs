mod common;
use ncm_api::MusicApi;
use netease_cloud_music_gtk4::download::{catalog::Catalog, manager::*, model::*, worker::*};
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct Backend {
    fail_lyrics: AtomicBool,
    active: AtomicUsize,
    max_active: AtomicUsize,
    encoding: AtomicUsize,
    max_encoding: AtomicUsize,
    delay: u64,
}
impl Backend {
    fn new(delay: u64) -> Arc<Self> {
        Arc::new(Self {
            fail_lyrics: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            encoding: AtomicUsize::new(0),
            max_encoding: AtomicUsize::new(0),
            delay,
        })
    }
}
impl DownloadBackend for Backend {
    fn run(
        &self,
        task: &TaskRecord,
        _: &MusicApi,
        temp: &Path,
        cancel: &AtomicBool,
        update: &dyn Fn(TaskStage, f64),
        encoder: &Mutex<()>,
    ) -> anyhow::Result<Vec<PreparedOutput>> {
        let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(count, Ordering::SeqCst);
        update(TaskStage::Downloading, 0.2);
        std::thread::sleep(Duration::from_millis(self.delay));
        {
            let _permit = encoder.lock().unwrap();
            let count = self.encoding.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_encoding.fetch_max(count, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            self.encoding.fetch_sub(1, Ordering::SeqCst);
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
        let mut result = vec![];
        for output in &task.outputs {
            if !matches!(
                output.state,
                OutputState::Pending | OutputState::Failed | OutputState::Unavailable
            ) {
                continue;
            }
            if output.suffix == ".lrc" && self.fail_lyrics.swap(false, Ordering::SeqCst) {
                result.push(PreparedOutput::failed(&output.suffix, "lyrics unavailable"));
                continue;
            }
            let path = temp.join(format!("prepared{}", output.suffix));
            fs::write(
                &path,
                if output.suffix == ".mp3" {
                    b"complete audio".as_slice()
                } else {
                    b"[00:01.00]lyrics".as_slice()
                },
            )?;
            result.push(PreparedOutput::ready(&output.suffix, path));
        }
        Ok(result)
    }
}
fn setup(
    backend: Arc<Backend>,
) -> (
    common::TempDir,
    DownloadManager,
    async_channel::Receiver<DownloadEvent>,
    DownloadOptions,
) {
    let temp = common::TempDir::new();
    let target = temp.0.join("output");
    fs::create_dir(&target).unwrap();
    let options = DownloadPreferences::default()
        .with_target(TargetLocation::capture(&target, false, None, None).unwrap())
        .unwrap();
    let (manager, events) =
        DownloadManager::new_with_backend(temp.0.join("data"), temp.0.join("temp"), backend)
            .unwrap();
    (temp, manager, events, options)
}
fn wait(manager: &DownloadManager) {
    let start = Instant::now();
    while manager.snapshot().tasks.iter().any(|t| !t.stage.terminal()) {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn deduplicates_same_target_but_allows_other_copy() {
    let (temp, manager, _, options) = setup(Backend::new(100));
    let client = MusicApi::new(2);
    let first = manager
        .enqueue(vec![common::song(1)], options.clone(), None, client.clone())
        .unwrap();
    assert_eq!(
        manager
            .enqueue(vec![common::song(1)], options.clone(), None, client.clone())
            .unwrap(),
        first
    );
    let other = temp.0.join("other");
    fs::create_dir(&other).unwrap();
    let mut other_options = options;
    other_options.target = TargetLocation::capture(&other, false, None, None).unwrap();
    manager
        .enqueue(vec![common::song(1)], other_options, None, client)
        .unwrap();
    wait(&manager);
    assert_eq!(manager.snapshot().tasks.len(), 2);
    assert_eq!(manager.snapshot().saved.len(), 2);
    manager.shutdown();
}
#[test]
fn limits_parallel_downloads_and_encoding() {
    let backend = Backend::new(30);
    let (_temp, manager, _, options) = setup(backend.clone());
    manager
        .enqueue(
            (1..=6).map(common::song).collect(),
            options,
            None,
            MusicApi::new(2),
        )
        .unwrap();
    wait(&manager);
    assert!(backend.max_active.load(Ordering::SeqCst) <= 2);
    assert_eq!(backend.max_encoding.load(Ordering::SeqCst), 1);
    assert_eq!(manager.snapshot().saved.len(), 6);
    manager.shutdown();
}
#[test]
fn retries_only_failed_lyrics_and_preserves_saved_audio() {
    let backend = Backend::new(0);
    backend.fail_lyrics.store(true, Ordering::Relaxed);
    let (_temp, manager, _, options) = setup(backend);
    let id = manager
        .enqueue(
            vec![common::song(1)],
            options.clone(),
            None,
            MusicApi::new(2),
        )
        .unwrap()[0];
    wait(&manager);
    assert_eq!(manager.snapshot().tasks[0].stage, TaskStage::Failed);
    let audio = options.target.root.join("歌手 - 歌名.mp3");
    let before = fs::metadata(&audio).unwrap().modified().unwrap();
    manager.retry(id, MusicApi::new(2)).unwrap();
    wait(&manager);
    assert_eq!(manager.snapshot().tasks[0].stage, TaskStage::Complete);
    assert_eq!(fs::metadata(&audio).unwrap().modified().unwrap(), before);
    assert!(options.target.root.join("歌手 - 歌名.lrc").exists());
    manager.cancel(id);
    assert!(audio.exists());
    manager.clear_history().unwrap();
    assert_eq!(manager.snapshot().saved.len(), 1);
    manager.shutdown();
}
#[test]
fn invalidating_location_cancels_active_and_queued_tasks() {
    let (_temp, manager, _, options) = setup(Backend::new(100));
    manager
        .enqueue(
            (1..=6).map(common::song).collect(),
            options.clone(),
            None,
            MusicApi::new(2),
        )
        .unwrap();
    manager.invalidate_location(&options.target.id);
    wait(&manager);
    assert!(
        manager
            .snapshot()
            .tasks
            .iter()
            .all(|t| t.stage == TaskStage::Cancelled)
    );
    assert!(manager.snapshot().saved.is_empty());
    manager.shutdown();
}
#[test]
fn account_change_stops_network_tasks() {
    let (_temp, manager, _, options) = setup(Backend::new(100));
    manager
        .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
        .unwrap();
    manager.stop_network_tasks();
    wait(&manager);
    assert_eq!(manager.snapshot().tasks[0].stage, TaskStage::Cancelled);
    manager.shutdown();
}
#[test]
fn restart_marks_unfinished_tasks_interrupted() {
    let (temp, manager, _, options) = setup(Backend::new(200));
    manager
        .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
        .unwrap();
    let state = manager.snapshot();
    let mut catalog = Catalog::default();
    catalog.tasks = state.tasks;
    manager.shutdown();
    std::thread::sleep(Duration::from_millis(250));
    let data = temp.0.join("restart");
    fs::create_dir(&data).unwrap();
    catalog.save(&data.join("downloads.json")).unwrap();
    let (restarted, _) =
        DownloadManager::new_with_backend(data, temp.0.join("restart-temp"), Backend::new(0))
            .unwrap();
    assert_eq!(restarted.snapshot().tasks[0].stage, TaskStage::Interrupted);
    restarted.shutdown();
}
#[test]
fn final_state_is_delivered_after_progress_throttling() {
    let (_temp, manager, events, options) = setup(Backend::new(20));
    manager
        .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
        .unwrap();
    wait(&manager);
    let mut completed = false;
    while let Ok(event) = events.try_recv() {
        if let DownloadEvent::Task(task) = event {
            completed |= task.stage == TaskStage::Complete;
        }
    }
    assert!(completed);
    manager.shutdown();
}
#[test]
fn retry_waits_until_cancelled_worker_has_stopped() {
    let (_temp, manager, _, options) = setup(Backend::new(100));
    let id = manager
        .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
        .unwrap()[0];
    let start = Instant::now();
    while manager.snapshot().tasks[0].stage == TaskStage::Waiting {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    manager.cancel(id);
    assert!(manager.retry(id, MusicApi::new(2)).is_err());
    manager.clear_history().unwrap();
    assert_eq!(manager.snapshot().tasks.len(), 1);
    std::thread::sleep(Duration::from_millis(150));
    manager.shutdown();
}

#[test]
fn importing_scan_does_not_duplicate_downloaded_file() {
    let (_temp, manager, _, options) = setup(Backend::new(0));
    manager
        .enqueue(
            vec![common::song(1)],
            options.clone(),
            None,
            MusicApi::new(2),
        )
        .unwrap();
    wait(&manager);
    let items = netease_cloud_music_gtk4::download::storage::scan(&options.target).unwrap();
    manager.import_files(options.target, items).unwrap();
    assert_eq!(manager.snapshot().saved.len(), 1);
    assert!(
        manager.snapshot().saved[0]
            .files
            .iter()
            .filter(|f| f.kind == FileKind::Lyrics)
            .all(|f| f.encoding == Some(TextEncoding::Utf8)),
        "scan/import must preserve known lyric encoding"
    );
    manager.shutdown();
}

#[test]
fn removed_target_requires_identity_checked_confirmation() {
    let (_temp, manager, _events, options) = setup(Backend::new(0));
    manager
        .enqueue(
            vec![common::song(12)],
            options.clone(),
            None,
            MusicApi::new(2),
        )
        .unwrap();
    let wrong =
        TargetLocation::capture(&options.target.root.parent().unwrap(), false, None, None).unwrap();
    manager.invalidate_location(&options.target.id);
    assert!(
        manager
            .confirm_location(&options.target.id, &wrong)
            .is_err()
    );
    std::thread::sleep(Duration::from_millis(40));
    manager
        .confirm_location(&options.target.id, &options.target)
        .unwrap();
    assert_eq!(manager.snapshot().locations[0].root, options.target.root);
    manager.shutdown();
}

#[test]
fn restart_cleans_only_journalled_temporary_files_and_refresh_marks_changed_audio() {
    let (temp, manager, _events, options) = setup(Backend::new(0));
    manager
        .enqueue(
            vec![common::song(1)],
            options.clone(),
            None,
            MusicApi::new(2),
        )
        .unwrap();
    wait(&manager);
    manager.shutdown();
    std::thread::sleep(Duration::from_millis(30));
    let catalog_path = temp.0.join("data/downloads.json");
    let mut catalog = Catalog::load(&catalog_path).unwrap();
    fs::write(options.target.root.join(".ncm-123.part"), b"owned").unwrap();
    fs::write(options.target.root.join("other.part"), b"external").unwrap();
    catalog
        .pending_cleanup
        .push(netease_cloud_music_gtk4::download::catalog::CleanupFile {
            location: options.target.clone(),
            relative: ".ncm-123.part".into(),
        });
    catalog.save(&catalog_path).unwrap();
    let (reopened, _) = DownloadManager::new_with_backend(
        temp.0.join("data"),
        temp.0.join("temp2"),
        Backend::new(0),
    )
    .unwrap();
    assert!(!options.target.root.join(".ncm-123.part").exists());
    assert!(options.target.root.join("other.part").exists());
    let audio = reopened.snapshot().saved[0]
        .files
        .iter()
        .find(|f| f.kind == FileKind::Audio)
        .unwrap()
        .relative
        .clone();
    fs::write(options.target.root.join(audio), b"replaced").unwrap();
    reopened.refresh_files().unwrap();
    assert_eq!(reopened.snapshot().saved[0].state, SavedState::Changed);
    reopened.shutdown();
}

#[test]
fn numbering_and_reordered_ordinals_create_new_copies() {
    let (_temp, manager, _, mut options) = setup(Backend::new(0));
    options.collection_folder = false;
    let client = MusicApi::new(2);
    manager
        .enqueue(vec![common::song(1)], options.clone(), None, client.clone())
        .unwrap();
    wait(&manager);
    options.numbered = true;
    manager
        .enqueue(vec![common::song(1)], options.clone(), None, client.clone())
        .unwrap();
    wait(&manager);
    let tasks = manager.snapshot().tasks;
    assert_ne!(tasks[0].basename, tasks[1].basename);
    assert!(tasks[1].basename.to_string_lossy().starts_with("001 "));
    manager
        .enqueue(
            vec![common::song(2), common::song(1)],
            options,
            None,
            client,
        )
        .unwrap();
    wait(&manager);
    let tasks = manager.snapshot().tasks;
    assert_ne!(tasks[1].basename, tasks[3].basename);
    manager.shutdown();
}
#[test]
fn adding_audio_to_dotted_lyric_name_keeps_companion_basename() {
    let (_temp, manager, _, mut options) = setup(Backend::new(0));
    options.content = DownloadContent::Lyrics;
    let mut song = common::song(1);
    song.name = "Mr. Blue".into();
    let client = MusicApi::new(2);
    manager
        .enqueue(vec![song.clone()], options.clone(), None, client.clone())
        .unwrap();
    wait(&manager);
    let base = manager.snapshot().tasks[0].basename.clone();
    options.content = DownloadContent::AudioAndLyrics;
    manager.enqueue(vec![song], options, None, client).unwrap();
    wait(&manager);
    let tasks = manager.snapshot().tasks;
    assert_eq!(tasks[1].basename, base);
    assert_eq!(
        tasks[1]
            .outputs
            .iter()
            .find(|o| o.suffix == ".lrc")
            .unwrap()
            .state,
        OutputState::Skipped
    );
    manager.shutdown();
}

#[test]
fn restart_cleans_journalled_local_work_but_preserves_foreign_files() {
    let temp = common::TempDir::new();
    let cache = temp.0.join("cache");
    let data = temp.0.join("data");
    fs::create_dir(&cache).unwrap();
    fs::create_dir(&data).unwrap();
    let owned = cache.join("task-77-88");
    let foreign = cache.join("task-99-55");
    fs::create_dir(&owned).unwrap();
    fs::create_dir(&foreign).unwrap();
    fs::write(owned.join("source"), b"source audio").unwrap();
    fs::write(owned.join("audio.mp3"), b"converted audio").unwrap();
    fs::write(foreign.join("notes.txt"), b"external").unwrap();
    let mut json = serde_json::to_value(Catalog::default()).unwrap();
    json["local_cleanup"] = serde_json::json!([
        TargetLocation::capture(&owned, false, None, None).unwrap(),
        TargetLocation::capture(&foreign, false, None, None).unwrap()
    ]);
    fs::write(
        data.join("downloads.json"),
        serde_json::to_vec(&json).unwrap(),
    )
    .unwrap();
    let (manager, _) = DownloadManager::new_with_backend(data, cache, Backend::new(0)).unwrap();
    assert!(
        !owned.exists(),
        "owned source/output audio remained after restart"
    );
    assert!(foreign.join("notes.txt").exists());
    manager.shutdown();
}

#[test]
fn shutdown_waits_for_worker_and_cleans_local_audio() {
    let backend = Backend::new(200);
    let (temp, manager, _, options) = setup(backend.clone());
    manager
        .enqueue(vec![common::song(1)], options, None, MusicApi::new(2))
        .unwrap();
    let start = Instant::now();
    while backend.active.load(Ordering::SeqCst) == 0 {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2));
    }
    manager.shutdown();
    assert!(!manager.wait_for_shutdown(Duration::from_millis(1)));
    assert!(manager.wait_for_shutdown(Duration::from_secs(2)));
    assert_eq!(backend.active.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read_dir(temp.0.join("temp")).unwrap().count(), 0);
    assert!(
        Catalog::load(&temp.0.join("data/downloads.json"))
            .unwrap()
            .local_cleanup
            .is_empty()
    );
    assert!(manager.snapshot().saved.is_empty());
}
