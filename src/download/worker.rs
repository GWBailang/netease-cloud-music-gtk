use super::lyrics::LyricDocument;
use super::{api, audio, model::*};
use anyhow::{Result, ensure};
use ncm_api::{DownloadResource, MusicApi};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct PreparedOutput {
    pub source_info: Option<String>,
    pub suffix: String,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub unavailable: bool,
}
impl PreparedOutput {
    pub fn ready(suffix: &str, path: PathBuf) -> Self {
        Self {
            source_info: None,
            suffix: suffix.into(),
            path: Some(path),
            error: None,
            unavailable: false,
        }
    }
    pub fn failed(suffix: &str, error: &str) -> Self {
        Self {
            source_info: None,
            suffix: suffix.into(),
            path: None,
            error: Some(error.into()),
            unavailable: false,
        }
    }
    pub fn unavailable(suffix: &str, error: &str) -> Self {
        Self {
            source_info: None,
            suffix: suffix.into(),
            path: None,
            error: Some(error.into()),
            unavailable: true,
        }
    }
}
pub trait DownloadBackend: Send + Sync {
    fn run(
        &self,
        task: &TaskRecord,
        client: &MusicApi,
        temp: &Path,
        cancelled: &AtomicBool,
        update: &dyn Fn(TaskStage, f64),
        encoder: &Mutex<()>,
    ) -> Result<Vec<PreparedOutput>>;
}
/// Resource lookup boundary; streaming, conversion and storage remain production code.
pub trait DownloadService: Send + Sync {
    fn resource(&self, client: &MusicApi, id: u64, bitrate: u32) -> Result<DownloadResource>;
    fn lyrics(&self, client: &MusicApi, id: u64) -> Result<LyricDocument>;
}
struct ApiService;
impl DownloadService for ApiService {
    fn resource(&self, client: &MusicApi, id: u64, bitrate: u32) -> Result<DownloadResource> {
        futures_lite::future::block_on(client.download_resource(id, bitrate))
    }
    fn lyrics(&self, client: &MusicApi, id: u64) -> Result<LyricDocument> {
        futures_lite::future::block_on(api::fetch_lyrics(client, id))
    }
}
pub struct ServiceBackend {
    service: Arc<dyn DownloadService>,
}
impl ServiceBackend {
    pub fn new(service: Arc<dyn DownloadService>) -> Self {
        Self { service }
    }
}
pub struct NetworkBackend;
impl DownloadBackend for NetworkBackend {
    fn run(
        &self,
        task: &TaskRecord,
        client: &MusicApi,
        temp: &Path,
        cancelled: &AtomicBool,
        update: &dyn Fn(TaskStage, f64),
        encoder: &Mutex<()>,
    ) -> Result<Vec<PreparedOutput>> {
        ServiceBackend::new(Arc::new(ApiService))
            .run(task, client, temp, cancelled, update, encoder)
    }
}
impl DownloadBackend for ServiceBackend {
    fn run(
        &self,
        task: &TaskRecord,
        client: &MusicApi,
        temp: &Path,
        cancelled: &AtomicBool,
        update: &dyn Fn(TaskStage, f64),
        encoder: &Mutex<()>,
    ) -> Result<Vec<PreparedOutput>> {
        let song = task
            .song
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Song details are unavailable"))?;
        let pending = |suffix: &str| {
            task.outputs.iter().any(|o| {
                o.suffix == suffix && !matches!(o.state, OutputState::Saved | OutputState::Skipped)
            })
        };
        let mut outputs = Vec::new();
        if pending(".mp3") {
            let prepared = (|| -> Result<(PathBuf, String)> {
                ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
                update(TaskStage::Fetching, 0.0);
                let resource = self
                    .service
                    .resource(client, song.id, task.options.bitrate)?;
                ensure!(
                    !resource.preview,
                    "Only a preview is available; full audio was not downloaded"
                );
                let space = rustix::fs::statvfs(temp)?;
                ensure!(
                    space.f_bavail.saturating_mul(space.f_frsize)
                        > song.duration.saturating_mul(task.options.bitrate as u64) / 8,
                    "Not enough temporary space for MP3 conversion"
                );
                let source = temp.join("source");
                update(TaskStage::Downloading, 0.0);
                futures_lite::future::block_on(client.download_to(
                    &resource.url,
                    &source,
                    cancelled,
                    |done, total| {
                        update(
                            TaskStage::Downloading,
                            total
                                .filter(|n| *n > 0)
                                .map(|n| done as f64 / n as f64)
                                .unwrap_or(0.0),
                        );
                    },
                ))?;
                ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
                let info = audio::probe(&source)?;
                audio::verify_complete(
                    info.duration_ms,
                    resource.expected_duration_ms,
                    resource.preview,
                )?;
                let _permit = encoder.lock().unwrap_or_else(|e| e.into_inner());
                ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
                update(TaskStage::Converting, 0.0);
                let output = temp.join("audio.mp3");
                let ordinal = if task.options.numbered {
                    task.basename
                        .file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.split(' ').next())
                        .and_then(|n| n.parse().ok())
                } else {
                    None
                };
                audio::prepare_mp3(
                    &source,
                    &output,
                    song,
                    task.options.bitrate,
                    ordinal,
                    cancelled,
                    |p| update(TaskStage::Converting, p),
                )?;
                Ok((
                    output,
                    format!(
                        "{} · {} kbps",
                        resource.codec.to_uppercase(),
                        resource.source_rate / 1000
                    ),
                ))
            })();
            outputs.push(match prepared {
                Ok((path, source)) => {
                    let mut output = PreparedOutput::ready(".mp3", path);
                    output.source_info = Some(source);
                    output
                }
                Err(e) => PreparedOutput::failed(".mp3", &e.to_string()),
            });
        }
        ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
        if task.options.content.lyrics()
            && task
                .outputs
                .iter()
                .any(|o| o.suffix != ".mp3" && pending(&o.suffix))
        {
            update(TaskStage::Fetching, 0.0);
            let lyrics = self.service.lyrics(client, song.id);
            for format in &task.options.lyric_formats {
                let original = format!(".{}", format.extension());
                let mut variants = vec![(original, false)];
                if task.options.lyric_mode == LyricMode::Separate {
                    variants.push((format!(".translated.{}", format.extension()), true));
                }
                for (suffix, translation) in variants {
                    if !pending(&suffix) {
                        continue;
                    }
                    let rendered = match &lyrics {
                        Err(e) => {
                            outputs.push(PreparedOutput::failed(&suffix, &e.to_string()));
                            continue;
                        }
                        Ok(doc) if translation => {
                            if doc.has_translation() {
                                doc.translation_only().render(
                                    *format,
                                    LyricMode::Original,
                                    task.options.encoding,
                                    Some(song.duration),
                                )
                            } else {
                                Err(anyhow::anyhow!("No translated lyrics available"))
                            }
                        }
                        Ok(doc) => doc.render(
                            *format,
                            if task.options.lyric_mode == LyricMode::Separate {
                                LyricMode::Original
                            } else {
                                task.options.lyric_mode
                            },
                            task.options.encoding,
                            Some(song.duration),
                        ),
                    };
                    match rendered {
                        Ok(rendered) => {
                            for output in rendered {
                                let path = temp.join(format!("lyrics{suffix}"));
                                match fs::write(&path, output.bytes) {
                                    Ok(()) => outputs.push(PreparedOutput::ready(&suffix, path)),
                                    Err(e) => outputs
                                        .push(PreparedOutput::failed(&suffix, &e.to_string())),
                                }
                            }
                        }
                        Err(e) => outputs.push(if e.to_string().contains("GBK") {
                            PreparedOutput::failed(&suffix, &e.to_string())
                        } else {
                            PreparedOutput::unavailable(&suffix, &e.to_string())
                        }),
                    }
                }
            }
        }
        Ok(outputs)
    }
}

/// Retag the private copy when its destination has a different track order.
/// Audio frames are preserved, and the original saved file is never changed.
pub(super) fn update_copy_tags(path: &Path, task: &TaskRecord) -> Result<()> {
    use id3::TagLike;
    let ordinal = if task.options.numbered {
        task.basename
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse::<u32>().ok())
    } else {
        None
    };
    let mut tag = match id3::Tag::read_from_path(path) {
        Ok(tag) => tag,
        Err(error) if matches!(error.kind, id3::ErrorKind::NoTag) => id3::Tag::new(),
        Err(error) => return Err(error.into()),
    };
    if tag.track() != ordinal {
        if let Some(song) = &task.song {
            tag.set_title(&song.name);
            tag.set_artist(&song.singer);
            tag.set_album(&song.album);
        }
        tag.remove_track();
        tag.remove_total_tracks();
        if let Some(ordinal) = ordinal {
            tag.set_track(ordinal);
        }
        tag.write_to_path(path, id3::Version::Id3v23)?;
    }
    Ok(())
}
