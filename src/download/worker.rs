use super::{api, audio, model::*};
use anyhow::{Result, ensure};
use ncm_api::MusicApi;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct PreparedOutput {
    pub suffix: String,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub unavailable: bool,
}
impl PreparedOutput {
    pub fn ready(suffix: &str, path: PathBuf) -> Self {
        Self {
            suffix: suffix.into(),
            path: Some(path),
            error: None,
            unavailable: false,
        }
    }
    pub fn failed(suffix: &str, error: &str) -> Self {
        Self {
            suffix: suffix.into(),
            path: None,
            error: Some(error.into()),
            unavailable: false,
        }
    }
    pub fn unavailable(suffix: &str, error: &str) -> Self {
        Self {
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
            let prepared = (|| -> Result<PathBuf> {
                ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
                update(TaskStage::Fetching, 0.0);
                let resource = futures_lite::future::block_on(
                    client.download_resource(song.id, task.options.bitrate),
                )?;
                ensure!(
                    !resource.preview,
                    "Only a preview is available; full audio was not downloaded"
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
                Ok(output)
            })();
            outputs.push(match prepared {
                Ok(path) => PreparedOutput::ready(".mp3", path),
                Err(e) => PreparedOutput::failed(".mp3", &e.to_string()),
            });
        }
        ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
        if task.options.content.lyrics() {
            update(TaskStage::Fetching, 0.0);
            let lyrics = futures_lite::future::block_on(api::fetch_lyrics(client, song.id));
            for format in &task.options.lyric_formats {
                let suffix = format!(".{}", format.extension());
                let translated = format!(".translated.{}", format.extension());
                if !pending(&suffix) && !pending(&translated) {
                    continue;
                }
                match &lyrics {
                    Ok(doc) => match doc.render(
                        *format,
                        task.options.lyric_mode,
                        task.options.encoding,
                        Some(song.duration),
                    ) {
                        Ok(rendered) => {
                            for output in rendered {
                                if !pending(&output.suffix) {
                                    continue;
                                }
                                let path = temp.join(format!("lyrics{}", output.suffix));
                                match fs::write(&path, output.bytes) {
                                    Ok(()) => {
                                        outputs.push(PreparedOutput::ready(&output.suffix, path))
                                    }
                                    Err(e) => outputs.push(PreparedOutput::failed(
                                        &output.suffix,
                                        &e.to_string(),
                                    )),
                                }
                            }
                            if task.options.lyric_mode == LyricMode::Separate
                                && !doc.has_translation()
                                && pending(&translated)
                            {
                                outputs.push(PreparedOutput::unavailable(
                                    &translated,
                                    "No translated lyrics available",
                                ));
                            }
                        }
                        Err(e) => {
                            if pending(&suffix) {
                                outputs.push(PreparedOutput::unavailable(&suffix, &e.to_string()));
                            }
                            if task.options.lyric_mode == LyricMode::Separate
                                && pending(&translated)
                            {
                                outputs
                                    .push(PreparedOutput::unavailable(&translated, &e.to_string()));
                            }
                        }
                    },
                    Err(e) => {
                        if pending(&suffix) {
                            outputs.push(PreparedOutput::unavailable(&suffix, &e.to_string()));
                        }
                        if task.options.lyric_mode == LyricMode::Separate && pending(&translated) {
                            outputs.push(PreparedOutput::unavailable(&translated, &e.to_string()));
                        }
                    }
                }
            }
        }
        Ok(outputs)
    }
}
