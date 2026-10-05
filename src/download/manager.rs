use super::{
    catalog::{Catalog, CleanupFile},
    model::*,
    naming::*,
    storage,
    worker::*,
};
use anyhow::{Result, anyhow, ensure};
use async_channel::{Receiver, Sender};
use ncm_api::{MusicApi, SongInfo};
use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct State {
    catalog: Catalog,
    clients: HashMap<TaskId, MusicApi>,
    cancelled: HashMap<TaskId, Arc<AtomicBool>>,
    invalid: HashSet<LocationId>,
    last_progress: HashMap<TaskId, Instant>,
    busy: HashSet<TaskId>,
    shutdown: bool,
}
struct Core {
    state: Mutex<State>,
    wake: Condvar,
    encoder: Mutex<()>,
    backend: Arc<dyn DownloadBackend>,
    catalog_path: PathBuf,
    temp: PathBuf,
    events: Sender<DownloadEvent>,
}
struct Lifetime(Weak<Core>);
impl Drop for Lifetime {
    fn drop(&mut self) {
        if let Some(core) = self.0.upgrade() {
            core.stop();
        }
    }
}
#[derive(Clone)]
pub struct DownloadManager {
    core: Arc<Core>,
    _lifetime: Arc<Lifetime>,
}
#[derive(Debug, Default)]
pub struct EnqueueReport {
    pub task_ids: Vec<TaskId>,
    pub queued: usize,
    pub already_saved: usize,
    pub already_queued: usize,
    pub unavailable: usize,
}
impl std::fmt::Debug for DownloadManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DownloadManager")
    }
}
fn suffixes(options: &DownloadOptions) -> Vec<String> {
    let mut list = Vec::new();
    if options.content.audio() {
        list.push(".mp3".into());
    }
    if options.content.lyrics() {
        for f in &options.lyric_formats {
            for suffix in [
                format!(".{}", f.extension()),
                format!(".translated.{}", f.extension()),
            ] {
                if suffix.contains("translated") && options.lyric_mode != LyricMode::Separate {
                    continue;
                }
                if !list.contains(&suffix) {
                    list.push(suffix);
                }
            }
        }
    }
    list
}
fn relative_output(base: &Path, suffix: &str) -> PathBuf {
    base.with_file_name(format!(
        "{}{}",
        base.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ))
}
fn output_basename(path: &Path) -> PathBuf {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let stem = [
        ".translated.lrc",
        ".translated.txt",
        ".translated.srt",
        ".mp3",
        ".lrc",
        ".txt",
        ".srt",
    ]
    .iter()
    .find_map(|s| filename.strip_suffix(s))
    .unwrap_or(&filename);
    path.with_file_name(stem)
}
impl DownloadManager {
    pub fn new(data_dir: PathBuf, temp_dir: PathBuf) -> Result<(Self, Receiver<DownloadEvent>)> {
        Self::new_with_backend(data_dir, temp_dir, Arc::new(NetworkBackend))
    }
    pub fn new_with_backend(
        data_dir: PathBuf,
        temp_dir: PathBuf,
        backend: Arc<dyn DownloadBackend>,
    ) -> Result<(Self, Receiver<DownloadEvent>)> {
        fs::create_dir_all(&data_dir)?;
        fs::create_dir_all(&temp_dir)?;
        let catalog_path = data_dir.join("downloads.json");
        let mut catalog = Catalog::load(&catalog_path)?;
        catalog.pending_cleanup.retain(|f| {
            f.location.removable || storage::cleanup_temporary(&f.location, &f.relative).is_err()
        });
        catalog
            .local_cleanup
            .retain(|location| storage::cleanup_work_directory(location).is_err());
        for task in &mut catalog.tasks {
            if !task.stage.terminal() {
                task.stage = TaskStage::Interrupted;
                task.error = Some("Download interrupted; retry to continue".into());
            }
        }
        catalog.save(&catalog_path)?;
        let invalid = catalog
            .locations
            .iter()
            .filter(|l| l.removable)
            .map(|l| l.id.clone())
            .collect();
        let (events, receiver) = async_channel::unbounded();
        let core = Arc::new(Core {
            state: Mutex::new(State {
                catalog,
                clients: HashMap::new(),
                cancelled: HashMap::new(),
                invalid,
                last_progress: HashMap::new(),
                busy: HashSet::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
            encoder: Mutex::new(()),
            backend,
            catalog_path,
            temp: temp_dir,
            events,
        });
        let manager = Self {
            core: core.clone(),
            _lifetime: Arc::new(Lifetime(Arc::downgrade(&core))),
        };
        for _ in 0..2 {
            let core = core.clone();
            std::thread::spawn(move || core.work_loop());
        }
        Ok((manager, receiver))
    }
    pub fn snapshot(&self) -> ManagerSnapshot {
        self.core
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .catalog
            .snapshot()
    }
    pub fn enqueue(
        &self,
        songs: Vec<SongInfo>,
        options: DownloadOptions,
        collection: Option<CollectionSnapshot>,
        client: MusicApi,
    ) -> Result<Vec<TaskId>> {
        self.enqueue_with_report(songs, options, collection, client)
            .map(|report| report.task_ids)
    }
    pub fn enqueue_with_report(
        &self,
        songs: Vec<SongInfo>,
        options: DownloadOptions,
        collection: Option<CollectionSnapshot>,
        client: MusicApi,
    ) -> Result<EnqueueReport> {
        options.validate()?;
        storage::validate_location(&options.target)?;
        // Hash saved files outside the queue mutex; slow cards must not block cancel.
        let verified = self
            .snapshot()
            .saved
            .into_iter()
            .filter(|saved| {
                saved.location.id == options.target.id
                    && saved.files.iter().all(|file| {
                        storage::fingerprint(&saved.location, &file.relative)
                            .ok()
                            .as_ref()
                            == Some(&file.fingerprint)
                    })
            })
            .map(|saved| (saved.key, (saved.location, saved.files)))
            .collect::<HashMap<_, _>>();
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(!state.shutdown, "Download manager is shutting down");
        state.invalid.remove(&options.target.id);
        if !state
            .catalog
            .locations
            .iter()
            .any(|l| l.id == options.target.id)
        {
            state.catalog.locations.push(options.target.clone());
        }
        let folder = if options.collection_folder {
            collection
                .as_ref()
                .map(|c| sanitize_component(&format!("{} [{}]", c.name, c.id)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut report = EnqueueReport::default();
        let mut seen = HashSet::new();
        let total = collection
            .as_ref()
            .map(|c| c.items.len())
            .unwrap_or(songs.len());
        for (position, mut song) in songs.into_iter().enumerate() {
            if !seen.insert(song.id) {
                continue;
            }
            song.song_url.clear();
            let ordinal = collection
                .as_ref()
                .and_then(|c| c.items.iter().position(|i| i.song_id == song.id))
                .unwrap_or(position)
                + 1;
            let stem = song_basename(
                &song,
                if options.numbered {
                    Some((ordinal, total))
                } else {
                    None
                },
            );
            let mut basename = PathBuf::from(&folder).join(&stem);
            if let Some(existing) = state.catalog.tasks.iter().find(|t| {
                t.song_id == song.id
                    && t.options == options
                    && !t.stage.terminal()
                    && t.requested_basename.as_ref().unwrap_or(&t.basename) == &basename
            }) {
                report.task_ids.push(existing.id);
                report.already_queued += 1;
                continue;
            }
            let reusable = state
                .catalog
                .saved
                .iter()
                .find(|saved| {
                    saved.location.id == options.target.id
                        && saved.song.as_ref().is_some_and(|s| s.id == song.id)
                        && saved.bitrate == options.bitrate
                        && (!options.content.lyrics()
                            || saved
                                .files
                                .iter()
                                .filter(|f| f.kind == FileKind::Lyrics)
                                .all(|f| f.encoding == Some(options.encoding)))
                        && saved.requested_basename.as_ref().map_or_else(
                            || {
                                saved
                                    .files
                                    .first()
                                    .is_some_and(|f| output_basename(&f.relative) == basename)
                            },
                            |b| b == &basename,
                        )
                        && saved.lyric_mode == options.lyric_mode
                        && saved
                            .files
                            .iter()
                            .any(|f| f.relative.parent() == basename.parent())
                        && verified.get(&saved.key).is_some_and(|(location, files)| {
                            location == &saved.location && files == &saved.files
                        })
                })
                .cloned();
            if let Some(saved) = &reusable {
                if let Some(file) = saved.files.first() {
                    basename = output_basename(&file.relative);
                }
            } else {
                let mut attempt = 0;
                let mut shortened = stem.clone();
                while shortened.len() > 145 {
                    shortened.pop();
                }
                while state
                    .catalog
                    .tasks
                    .iter()
                    .any(|t| t.options.target.id == options.target.id && t.basename == basename)
                    || suffixes(&options).iter().any(|s| {
                        options
                            .target
                            .root
                            .join(relative_output(&basename, s))
                            .symlink_metadata()
                            .is_ok()
                    })
                {
                    attempt += 1;
                    basename = PathBuf::from(&folder).join(format!(
                        "{shortened} [{}-{}-{}]",
                        song.id, options.bitrate, attempt
                    ));
                }
            }
            let outputs = suffixes(&options)
                .into_iter()
                .map(|suffix| {
                    let existing = reusable
                        .as_ref()
                        .and_then(|saved| {
                            saved
                                .files
                                .iter()
                                .find(|f| f.relative == relative_output(&basename, &suffix))
                        })
                        .cloned();
                    OutputRecord {
                        suffix,
                        state: if existing.is_some() {
                            OutputState::Skipped
                        } else {
                            OutputState::Pending
                        },
                        file: existing,
                        error: None,
                    }
                })
                .collect::<Vec<_>>();
            let id = state.catalog.tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
            let complete = outputs.iter().all(|o| o.state == OutputState::Skipped);
            if complete {
                report.already_saved += 1;
            } else {
                report.queued += 1;
            }
            state.catalog.tasks.push(TaskRecord {
                source_info: None,
                id,
                song_id: song.id,
                song: Some(song),
                options: options.clone(),
                requested_basename: Some(PathBuf::from(&folder).join(&stem)),
                basename,
                stage: if complete {
                    TaskStage::Complete
                } else {
                    TaskStage::Waiting
                },
                progress: if complete { 1.0 } else { 0.0 },
                error: None,
                outputs,
            });
            if !complete {
                state.clients.insert(id, client.clone());
                state.cancelled.insert(id, Arc::new(AtomicBool::new(false)));
            }
            report.task_ids.push(id);
        }
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        self.core.wake.notify_all();
        Ok(report)
    }
    pub fn enqueue_collection(
        &self,
        collection: CollectionSnapshot,
        options: DownloadOptions,
        client: MusicApi,
    ) -> Result<Vec<TaskId>> {
        self.enqueue_collection_with_report(collection, options, client)
            .map(|report| report.task_ids)
    }
    pub fn enqueue_collection_with_report(
        &self,
        collection: CollectionSnapshot,
        options: DownloadOptions,
        client: MusicApi,
    ) -> Result<EnqueueReport> {
        let songs = collection
            .items
            .iter()
            .filter_map(|i| i.song.clone())
            .collect();
        let mut report =
            self.enqueue_with_report(songs, options.clone(), Some(collection.clone()), client)?;
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        for item in collection.items.iter().filter(|i| i.song.is_none()) {
            let id = state.catalog.tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
            state.catalog.tasks.push(TaskRecord {
                requested_basename: None,
                source_info: None,
                id,
                song_id: item.song_id,
                song: None,
                options: options.clone(),
                basename: PathBuf::new(),
                stage: TaskStage::Failed,
                progress: 0.0,
                error: item.error.clone(),
                outputs: vec![],
            });
            report.task_ids.push(id);
            report.unavailable += 1;
        }
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(report)
    }
    pub fn confirm_location(&self, id: &LocationId, target: &TargetLocation) -> Result<()> {
        storage::validate_location(target)?;
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        let old = state
            .catalog
            .locations
            .iter()
            .find(|l| &l.id == id)
            .ok_or_else(|| anyhow!("Save location not found"))?;
        ensure!(old.root == target.root, "Choose the original save folder");
        if let Some(uuid) = &old.volume_uuid {
            ensure!(
                target.volume_uuid.as_ref() == Some(uuid),
                "A different device was selected"
            );
        } else {
            ensure!(
                old.filesystem_id == target.filesystem_id && old.root_inode == target.root_inode,
                "Storage identity changed; scan the folder to import its files"
            );
        }
        ensure!(
            !state
                .catalog
                .tasks
                .iter()
                .any(|t| &t.options.target.id == id && state.busy.contains(&t.id)),
            "Wait for active downloads to stop"
        );
        let mut confirmed = target.clone();
        confirmed.id = id.clone();
        for location in &mut state.catalog.locations {
            if &location.id == id {
                *location = confirmed.clone();
            }
        }
        for task in &mut state.catalog.tasks {
            if &task.options.target.id == id {
                task.options.target = confirmed.clone();
            }
        }
        for song in &mut state.catalog.saved {
            if &song.location.id == id {
                song.location = confirmed.clone();
            }
        }
        state.catalog.pending_cleanup.retain_mut(|f| {
            if &f.location.id == id {
                f.location = confirmed.clone();
                storage::cleanup_temporary(&f.location, &f.relative).is_err()
            } else {
                true
            }
        });
        state.catalog.save(&self.core.catalog_path)?;
        state.invalid.remove(id);
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(())
    }

    pub fn retry(&self, id: TaskId, client: MusicApi) -> Result<()> {
        self.retry_options(id, client, None)
    }
    pub fn retry_utf8(&self, id: TaskId, client: MusicApi) -> Result<()> {
        self.retry_options(id, client, Some(TextEncoding::Utf8))
    }
    pub fn retry_confirmed(
        &self,
        id: TaskId,
        client: MusicApi,
        utf8: bool,
        confirmation: Option<(&LocationId, &TargetLocation)>,
    ) -> Result<()> {
        if let Some((location, target)) = confirmation {
            self.confirm_location(location, target)?;
        }
        if utf8 {
            self.retry_utf8(id, client)
        } else {
            self.retry(id, client)
        }
    }
    fn retry_options(
        &self,
        id: TaskId,
        client: MusicApi,
        encoding: Option<TextEncoding>,
    ) -> Result<()> {
        let state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        let task = state
            .catalog
            .tasks
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("Task not found"))?
            .clone();
        ensure!(
            task.stage.terminal() && !state.busy.contains(&id),
            "Wait for the cancelled worker to stop before retrying"
        );
        ensure!(
            !state.invalid.contains(&task.options.target.id),
            "Select this save location again before retrying"
        );
        drop(state);
        storage::validate_location(&task.options.target)?;
        ensure!(
            task.song.is_some(),
            "Song details unavailable; load the playlist again"
        );
        for output in &task.outputs {
            if let Some(file) = &output.file {
                ensure!(
                    storage::fingerprint(&task.options.target, &file.relative)
                        .ok()
                        .as_ref()
                        == Some(&file.fingerprint),
                    "Saved file has changed; refresh and download a new copy"
                );
            }
        }
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        let current = state
            .catalog
            .tasks
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow!("Task was removed"))?;
        ensure!(
            !state.shutdown
                && !state.busy.contains(&id)
                && current.stage.terminal()
                && !state.invalid.contains(&current.options.target.id)
                && current.options == task.options
                && current.outputs == task.outputs,
            "Task changed during file verification; try again"
        );
        let task = state.catalog.tasks.iter_mut().find(|t| t.id == id).unwrap();
        if let Some(encoding) = encoding {
            task.options.encoding = encoding;
        }
        for output in &mut task.outputs {
            if !matches!(output.state, OutputState::Saved | OutputState::Skipped) {
                output.state = OutputState::Pending;
                output.error = None;
            }
        }
        task.stage = TaskStage::Waiting;
        task.error = None;
        task.progress = 0.0;
        state.clients.insert(id, client);
        state.cancelled.insert(id, Arc::new(AtomicBool::new(false)));
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        self.core.wake.notify_all();
        Ok(())
    }
    pub fn cancel(&self, id: TaskId) {
        self.core.cancel(id);
    }
    pub fn invalidate_location(&self, id: &LocationId) {
        {
            self.core
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .invalid
                .insert(id.clone());
        }
        let ids = self
            .snapshot()
            .tasks
            .into_iter()
            .filter(|t| t.options.target.id == *id && !t.stage.terminal())
            .map(|t| t.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel(id);
        }
        self.core.emit(DownloadEvent::LocationInvalid(id.clone()));
    }
    pub fn stop_network_tasks(&self) {
        for task in self.snapshot().tasks {
            if !task.stage.terminal() {
                self.cancel(task.id);
            }
        }
    }
    pub fn clear_history(&self) -> Result<()> {
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        let busy = state.busy.clone();
        state
            .catalog
            .tasks
            .retain(|t| !t.stage.terminal() || busy.contains(&t.id));
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(())
    }
    pub fn import_files(&self, target: TargetLocation, items: Vec<SavedSong>) -> Result<()> {
        storage::validate_location(&target)?;
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        state.invalid.remove(&target.id);
        if !state.catalog.locations.iter().any(|l| l.id == target.id) {
            state.catalog.locations.push(target);
        }
        for item in items {
            if let Some(existing) = state.catalog.saved.iter_mut().find(|s| {
                s.location.id == item.location.id
                    && s.files
                        .iter()
                        .any(|f| item.files.iter().any(|new| new.relative == f.relative))
            }) {
                let same_audio = existing
                    .files
                    .iter()
                    .filter(|f| f.kind == FileKind::Audio)
                    .any(|f| {
                        item.files.iter().any(|new| {
                            new.relative == f.relative && new.fingerprint == f.fingerprint
                        })
                    });
                if same_audio {
                    let mut files = item.files;
                    for file in &mut files {
                        if let Some(known) = existing.files.iter().find(|known| {
                            known.relative == file.relative && known.fingerprint == file.fingerprint
                        }) {
                            file.encoding = known.encoding;
                        }
                    }
                    existing.files = files;
                } else {
                    let key = existing.key.clone();
                    *existing = item;
                    existing.key = key;
                }
            } else {
                state.catalog.saved.push(item);
            }
        }
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(())
    }
    pub fn delete_files(&self, preview: &DeletePreview, mode: DeleteMode) -> Result<DeleteReport> {
        let state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        for item in &preview.items {
            ensure!(
                !state.invalid.contains(&item.location.id),
                "Save location is offline; select it again"
            );
            ensure!(
                !state.catalog.tasks.iter().any(|t| !t.stage.terminal()
                    && t.options.target.id == item.location.id
                    && t.outputs
                        .iter()
                        .any(|o| relative_output(&t.basename, &o.suffix) == item.file.relative)),
                "Cancel the active download before deleting its files"
            );
        }
        drop(state);
        let report = storage::execute_delete(preview, mode);
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        for result in &report.results {
            if result.error.is_none() {
                for saved in &mut state.catalog.saved {
                    if saved.location.id == result.item.location.id {
                        saved
                            .files
                            .retain(|f| f.relative != result.item.file.relative);
                    }
                }
            }
        }
        state.catalog.saved.retain(|s| !s.files.is_empty());
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(report)
    }
    pub fn refresh_files(&self) -> Result<()> {
        self.refresh_files_using(|saved| {
            if storage::validate_location(&saved.location).is_err() {
                return SavedState::Unavailable;
            }
            let mut status = SavedState::Available;
            for file in &saved.files {
                match storage::fingerprint(&saved.location, &file.relative) {
                    Ok(actual) if actual != file.fingerprint => return SavedState::Changed,
                    Err(_) => status = SavedState::Missing,
                    _ => {}
                }
            }
            status
        })
    }
    fn refresh_files_using(&self, inspect: impl Fn(&SavedSong) -> SavedState) -> Result<()> {
        let (saved, invalid) = {
            let state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.catalog.saved.clone(), state.invalid.clone())
        };
        let checked = saved
            .into_iter()
            .map(|saved| {
                let status = if invalid.contains(&saved.location.id) {
                    SavedState::Unavailable
                } else {
                    inspect(&saved)
                };
                (saved, status)
            })
            .collect::<Vec<_>>();
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        let invalid = state.invalid.clone();
        for (previous, status) in checked {
            if let Some(current) = state.catalog.saved.iter_mut().find(|s| {
                s.key == previous.key
                    && s.location == previous.location
                    && s.files == previous.files
            }) {
                current.state = if invalid.contains(&current.location.id) {
                    SavedState::Unavailable
                } else {
                    status
                };
            }
        }
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(())
    }
    pub fn remove_missing(&self) -> Result<()> {
        let missing = self
            .snapshot()
            .saved
            .into_iter()
            .flat_map(|saved| {
                let valid = storage::validate_location(&saved.location).is_ok();
                saved.files.into_iter().filter_map(move |file| {
                    if valid
                        && saved
                            .location
                            .root
                            .join(&file.relative)
                            .symlink_metadata()
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        Some((saved.key.clone(), saved.location.clone(), file))
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        for (key, location, file) in missing {
            if !state.invalid.contains(&location.id) {
                if let Some(saved) = state
                    .catalog
                    .saved
                    .iter_mut()
                    .find(|s| s.key == key && s.location == location)
                {
                    saved.files.retain(|f| f != &file);
                }
            }
        }
        state.catalog.saved.retain(|s| !s.files.is_empty());
        state.catalog.save(&self.core.catalog_path)?;
        let snapshot = state.catalog.snapshot();
        drop(state);
        self.core.emit(DownloadEvent::Changed(snapshot));
        Ok(())
    }
    /// Wait on a background thread; GTK keeps processing events while workers stop.
    pub fn wait_for_shutdown(&self, timeout: Duration) -> bool {
        let state = self.core.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .core
            .wake
            .wait_timeout_while(state, timeout, |s| !s.busy.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        state.busy.is_empty()
    }
    pub fn shutdown(&self) {
        self.core.stop();
    }
}
impl Core {
    fn emit(&self, event: DownloadEvent) {
        let _ = self.events.try_send(event);
    }
    fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.shutdown = true;
        for cancel in state.cancelled.values() {
            cancel.store(true, Ordering::Relaxed);
        }
        for task in &mut state.catalog.tasks {
            if !task.stage.terminal() {
                task.stage = TaskStage::Interrupted;
            }
        }
        if let Err(e) = state.catalog.save(&self.catalog_path) {
            self.emit(DownloadEvent::Error(e.to_string()));
        }
        drop(state);
        self.wake.notify_all();
    }
    fn cancel(&self, id: TaskId) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flag) = state.cancelled.get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(task) = state
            .catalog
            .tasks
            .iter_mut()
            .find(|t| t.id == id && !t.stage.terminal())
        {
            task.stage = TaskStage::Cancelled;
            task.error = Some("Download cancelled".into());
            let task = task.clone();
            if let Err(e) = state.catalog.save(&self.catalog_path) {
                self.emit(DownloadEvent::Error(e.to_string()));
            }
            self.emit(DownloadEvent::Task(task));
        }
        drop(state);
        self.wake.notify_all();
    }
    fn update(&self, id: TaskId, stage: TaskStage, progress: f64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(task) = state.catalog.tasks.iter_mut().find(|t| t.id == id) else {
            return;
        };
        if task.stage.terminal() {
            return;
        }
        let changed = task.stage != stage;
        task.stage = stage;
        task.progress = progress.clamp(0.0, 1.0);
        let task = task.clone();
        let now = Instant::now();
        if changed {
            if let Err(e) = state.catalog.save(&self.catalog_path) {
                self.emit(DownloadEvent::Error(e.to_string()));
            }
        }
        if changed
            || state
                .last_progress
                .get(&id)
                .is_none_or(|last| now.duration_since(*last) >= Duration::from_millis(250))
        {
            state.last_progress.insert(id, now);
            self.emit(DownloadEvent::Task(task));
        }
    }
    fn work_loop(self: Arc<Self>) {
        loop {
            let job = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if state.shutdown {
                        return;
                    }
                    let next = state
                        .catalog
                        .tasks
                        .iter()
                        .find(|t| {
                            t.stage == TaskStage::Waiting && state.clients.contains_key(&t.id)
                        })
                        .map(|t| t.id);
                    if let Some(id) = next {
                        let task = state.catalog.tasks.iter_mut().find(|t| t.id == id).unwrap();
                        task.stage = TaskStage::Fetching;
                        let task = task.clone();
                        let client = state.clients.get(&id).unwrap().clone();
                        let cancel = state.cancelled.get(&id).unwrap().clone();
                        if let Err(e) = state.catalog.save(&self.catalog_path) {
                            let task = state.catalog.tasks.iter_mut().find(|t| t.id == id).unwrap();
                            task.stage = TaskStage::Failed;
                            task.error = Some(e.to_string());
                            self.emit(DownloadEvent::Task(task.clone()));
                            continue;
                        }
                        state.busy.insert(id);
                        break (task, client, cancel);
                    }
                    state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            let (task, client, cancel) = job;
            let temp = self
                .temp
                .join(format!("task-{}-{}", task.id, fastrand::u64(..)));
            let id = task.id;
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    fs::DirBuilder::new().mode(0o700).create(&temp)?;
                    let location = TargetLocation::capture(&temp, false, None, None)?;
                    {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        state.catalog.local_cleanup.push(location);
                        state.catalog.save(&self.catalog_path)?;
                    }

                    let outputs = self.backend.run(
                        &task,
                        &client,
                        &temp,
                        &cancel,
                        &|stage, p| self.update(id, stage, p),
                        &self.encoder,
                    )?;
                    for prepared in outputs {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let result = if let Some(path) = &prepared.path {
                            self.update(id, TaskStage::Saving, 0.0);
                            let relative = relative_output(&task.basename, &prepared.suffix);
                            Some(storage::commit_file_tracked(
                                path,
                                &task.options.target,
                                &relative,
                                &cancel,
                                |relative, owned| {
                                    let mut state =
                                        self.state.lock().unwrap_or_else(|e| e.into_inner());
                                    if owned {
                                        state.catalog.pending_cleanup.push(CleanupFile {
                                            location: task.options.target.clone(),
                                            relative: relative.into(),
                                        });
                                    } else {
                                        state.catalog.pending_cleanup.retain(|f| {
                                            !(f.location.id == task.options.target.id
                                                && f.relative == relative)
                                        });
                                    }
                                    state.catalog.save(&self.catalog_path)
                                },
                            ))
                        } else {
                            None
                        };
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        let record = state
                            .catalog
                            .tasks
                            .iter_mut()
                            .find(|t| t.id == id)
                            .ok_or_else(|| anyhow!("Task was removed"))?;
                        if prepared.source_info.is_some() {
                            record.source_info = prepared.source_info.clone();
                        }
                        if let Some(output) = record
                            .outputs
                            .iter_mut()
                            .find(|o| o.suffix == prepared.suffix)
                        {
                            match result {
                                Some(Ok(mut file)) => {
                                    if file.kind == FileKind::Lyrics {
                                        file.encoding = Some(record.options.encoding);
                                    }
                                    output.state = OutputState::Saved;
                                    output.file = Some(file);
                                    output.error = None;
                                }
                                Some(Err(e)) => {
                                    output.state = OutputState::Failed;
                                    output.error = Some(e.to_string());
                                }
                                None => {
                                    output.state = if prepared.unavailable {
                                        OutputState::Unavailable
                                    } else {
                                        OutputState::Failed
                                    };
                                    output.error = prepared.error;
                                }
                            }
                        }
                        let record = record.clone();
                        self.index_outputs(&mut state.catalog, &record);
                        state.catalog.save(&self.catalog_path)?;
                        self.emit(DownloadEvent::Task(record));
                    }
                    Ok(())
                }))
                .unwrap_or_else(|_| Err(anyhow!("Download worker failed")));
            let owned = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                state
                    .catalog
                    .local_cleanup
                    .iter()
                    .find(|l| l.root == temp)
                    .cloned()
            };
            let cleaned = owned
                .as_ref()
                .is_some_and(|l| storage::cleanup_work_directory(l).is_ok());
            if owned.is_none() {
                let _ = fs::remove_dir(&temp);
            }

            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(record) = state.catalog.tasks.iter_mut().find(|t| t.id == id) {
                if !matches!(record.stage, TaskStage::Cancelled | TaskStage::Interrupted) {
                    if let Err(e) = outcome {
                        record.stage = TaskStage::Failed;
                        record.error = Some(e.to_string());
                    } else {
                        for output in &mut record.outputs {
                            if output.state == OutputState::Pending {
                                output.state = OutputState::Failed;
                                output.error = Some("Output was not produced".into());
                            }
                        }
                        let any_saved = record
                            .outputs
                            .iter()
                            .any(|o| matches!(o.state, OutputState::Saved | OutputState::Skipped));
                        let failures = record
                            .outputs
                            .iter()
                            .any(|o| o.state == OutputState::Failed);
                        record.stage = if any_saved && !failures {
                            TaskStage::Complete
                        } else {
                            TaskStage::Failed
                        };
                        record.progress = 1.0;
                        record.error = record
                            .outputs
                            .iter()
                            .filter_map(|o| o.error.clone())
                            .collect::<Vec<_>>()
                            .into_iter()
                            .reduce(|a, b| format!("{a}; {b}"));
                    }
                }
                self.emit(DownloadEvent::Task(record.clone()));
            }
            if cleaned {
                state.catalog.local_cleanup.retain(|l| l.root != temp);
            }
            state.busy.remove(&id);
            state.clients.remove(&id);
            state.cancelled.remove(&id);
            state.last_progress.remove(&id);
            if let Err(e) = state.catalog.save(&self.catalog_path) {
                self.emit(DownloadEvent::Error(e.to_string()));
            }
            self.emit(DownloadEvent::Changed(state.catalog.snapshot()));
            self.wake.notify_all();
        }
    }
    fn index_outputs(&self, catalog: &mut Catalog, task: &TaskRecord) {
        let files = task
            .outputs
            .iter()
            .filter_map(|o| o.file.clone())
            .collect::<Vec<_>>();
        if files.is_empty() {
            return;
        }
        let key = format!("{}:{}", task.options.target.id, task.basename.display());
        let title = task
            .song
            .as_ref()
            .map(|s| format!("{} - {}", s.singer, s.name))
            .unwrap_or_else(|| task.song_id.to_string());
        if let Some(saved) = catalog.saved.iter_mut().find(|s| s.key == key) {
            saved.state = SavedState::Available;
            for file in files {
                if !saved.files.iter().any(|f| f.relative == file.relative) {
                    saved.files.push(file);
                }
            }
        } else {
            catalog.saved.push(SavedSong {
                requested_basename: task.requested_basename.clone(),
                state: SavedState::Available,
                key,
                location: task.options.target.clone(),
                song: task.song.clone(),
                title,
                bitrate: task.options.bitrate,
                encoding: task.options.encoding,
                lyric_mode: task.options.lyric_mode,
                files,
                imported: false,
            });
        }
    }
}

#[cfg(test)]
mod responsiveness_tests {
    use super::*;
    #[test]
    fn slow_file_refresh_does_not_block_snapshot_or_cancellation() {
        let root = std::env::temp_dir().join(format!("ncm-refresh-{}", fastrand::u64(..)));
        fs::create_dir_all(root.join("output")).unwrap();
        fs::write(root.join("output/song.mp3"), b"file").unwrap();
        let (manager, _) = DownloadManager::new(root.join("catalog"), root.join("cache")).unwrap();
        let target = TargetLocation::capture(&root.join("output"), false, None, None).unwrap();
        manager
            .import_files(target.clone(), storage::scan(&target).unwrap())
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let refreshing = manager.clone();
        let worker = std::thread::spawn(move || {
            refreshing
                .refresh_files_using(|_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    SavedState::Available
                })
                .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let responsive = manager.clone();
        let check = std::thread::spawn(move || {
            responsive.cancel(99);
            tx.send(responsive.snapshot()).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_millis(200));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        check.join().unwrap();
        manager.shutdown();
        fs::remove_dir_all(root).unwrap();
        assert!(
            result.is_ok(),
            "slow filesystem checks held the global state mutex"
        );
    }
}
