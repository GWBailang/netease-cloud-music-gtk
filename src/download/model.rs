use anyhow::{Result, ensure};
use ncm_api::SongInfo;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type TaskId = u64;
pub type LocationId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadContent {
    AudioAndLyrics,
    Audio,
    Lyrics,
}
impl DownloadContent {
    pub fn audio(self) -> bool {
        self != Self::Lyrics
    }
    pub fn lyrics(self) -> bool {
        self != Self::Audio
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricFormat {
    Lrc,
    Txt,
    Srt,
}
impl LyricFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Lrc => "lrc",
            Self::Txt => "txt",
            Self::Srt => "srt",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricMode {
    Original,
    Separate,
    Bilingual,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextEncoding {
    Utf8,
    Gbk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetLocation {
    pub id: LocationId,
    pub root: PathBuf,
    pub filesystem_id: u64,
    pub root_inode: u64,
    pub removable: bool,
    pub volume_uuid: Option<String>,
    pub mount_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadPreferences {
    pub target_path: PathBuf,
    #[serde(default)]
    pub target_removable: bool,
    #[serde(default)]
    pub target_uuid: Option<String>,
    pub bitrate: u32,
    pub content: DownloadContent,
    pub lyric_formats: Vec<LyricFormat>,
    pub lyric_mode: LyricMode,
    pub encoding: TextEncoding,
    pub collection_folder: bool,
    pub numbered: bool,
}
impl Default for DownloadPreferences {
    fn default() -> Self {
        let music = gtk::glib::user_special_dir(gtk::glib::UserDirectory::Music)
            .unwrap_or_else(|| gtk::glib::home_dir().join("Music"));
        Self {
            target_path: music.join("NetEase Downloads"),
            target_removable: false,
            target_uuid: None,
            bitrate: 192,
            content: DownloadContent::AudioAndLyrics,
            lyric_formats: vec![LyricFormat::Lrc],
            lyric_mode: LyricMode::Original,
            encoding: TextEncoding::Utf8,
            collection_folder: true,
            numbered: false,
        }
    }
}
impl DownloadPreferences {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [128, 192, 320].contains(&self.bitrate),
            "Unsupported MP3 bitrate"
        );
        ensure!(
            !self.target_path.as_os_str().is_empty(),
            "Choose a save folder"
        );
        ensure!(
            !self.content.lyrics() || !self.lyric_formats.is_empty(),
            "Choose at least one lyric format"
        );
        Ok(())
    }
    pub fn with_target(&self, target: TargetLocation) -> Result<DownloadOptions> {
        self.validate()?;
        Ok(DownloadOptions {
            target,
            bitrate: self.bitrate,
            content: self.content,
            lyric_formats: self.lyric_formats.clone(),
            lyric_mode: self.lyric_mode,
            encoding: self.encoding,
            collection_folder: self.collection_folder,
            numbered: self.numbered,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadOptions {
    pub target: TargetLocation,
    pub bitrate: u32,
    pub content: DownloadContent,
    pub lyric_formats: Vec<LyricFormat>,
    pub lyric_mode: LyricMode,
    pub encoding: TextEncoding,
    pub collection_folder: bool,
    pub numbered: bool,
}
impl DownloadOptions {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [128, 192, 320].contains(&self.bitrate),
            "Unsupported MP3 bitrate"
        );
        ensure!(
            self.target.root.is_absolute(),
            "Save folder must be a local absolute path"
        );
        ensure!(
            !self.content.lyrics() || !self.lyric_formats.is_empty(),
            "Choose at least one lyric format"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionItem {
    pub song_id: u64,
    pub song: Option<SongInfo>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSnapshot {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub items: Vec<CollectionItem>,
    pub duplicates: usize,
}
#[derive(Debug, Clone)]
pub struct LyricOutput {
    pub suffix: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size: u64,
    pub sha256: String,
    pub inode: u64,
    pub modified_ns: u128,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    Audio,
    Lyrics,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    #[serde(default)]
    pub encoding: Option<TextEncoding>,
    pub relative: PathBuf,
    pub kind: FileKind,
    pub fingerprint: Fingerprint,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedState {
    #[default]
    Available,
    Missing,
    Changed,
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSong {
    #[serde(default)]
    pub requested_basename: Option<PathBuf>,
    #[serde(default)]
    pub state: SavedState,
    pub key: String,
    pub location: TargetLocation,
    pub song: Option<SongInfo>,
    pub title: String,
    pub bitrate: u32,
    pub encoding: TextEncoding,
    pub lyric_mode: LyricMode,
    pub files: Vec<StoredFile>,
    pub imported: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStage {
    Waiting,
    Fetching,
    Downloading,
    Converting,
    Saving,
    Complete,
    Cancelled,
    Failed,
    Interrupted,
}
impl TaskStage {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Cancelled | Self::Failed | Self::Interrupted
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting",
            Self::Fetching => "Fetching resources",
            Self::Downloading => "Downloading",
            Self::Converting => "Converting",
            Self::Saving => "Saving",
            Self::Complete => "Complete",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
            Self::Interrupted => "Interrupted",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputState {
    Pending,
    Saved,
    Skipped,
    Unavailable,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRecord {
    pub suffix: String,
    pub state: OutputState,
    pub file: Option<StoredFile>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    #[serde(default)]
    pub requested_basename: Option<PathBuf>,
    #[serde(default)]
    pub source_info: Option<String>,
    pub id: TaskId,
    pub song_id: u64,
    pub song: Option<SongInfo>,
    pub options: DownloadOptions,
    pub basename: PathBuf,
    pub stage: TaskStage,
    pub progress: f64,
    pub error: Option<String>,
    pub outputs: Vec<OutputRecord>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagerSnapshot {
    pub tasks: Vec<TaskRecord>,
    pub saved: Vec<SavedSong>,
    pub locations: Vec<TargetLocation>,
}
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Task(TaskRecord),
    Changed(ManagerSnapshot),
    LocationInvalid(LocationId),
    Error(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    Trash,
    Permanent,
}
#[derive(Debug, Clone)]
pub struct DeleteItem {
    pub key: String,
    pub location: TargetLocation,
    pub file: StoredFile,
}
#[derive(Debug, Clone, Default)]
pub struct DeletePreview {
    pub items: Vec<DeleteItem>,
    pub total_bytes: u64,
}
#[derive(Debug, Clone)]
pub struct DeleteResult {
    pub item: DeleteItem,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct DeleteReport {
    pub results: Vec<DeleteResult>,
}
