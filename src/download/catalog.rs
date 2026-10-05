use super::model::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupFile {
    pub location: TargetLocation,
    pub relative: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub tasks: Vec<TaskRecord>,
    pub saved: Vec<SavedSong>,
    pub locations: Vec<TargetLocation>,
    pub pending_cleanup: Vec<CleanupFile>,
    #[serde(default)]
    pub local_cleanup: Vec<TargetLocation>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: 1,
            tasks: vec![],
            saved: vec![],
            locations: vec![],
            pending_cleanup: vec![],
            local_cleanup: vec![],
        }
    }
}
impl Catalog {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let decode = |path: &Path| -> Result<Self> {
            let data: Self = serde_json::from_slice(&fs::read(path)?)?;
            ensure!(data.version == 1, "Unsupported download catalog version");
            Ok(data)
        };
        decode(path).or_else(|_| decode(&path.with_extension("previous.json")))
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Missing catalog directory"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".catalog-{}.tmp", fastrand::u64(..)));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            if path.exists() {
                fs::copy(path, path.with_extension("previous.json"))?;
            }
            fs::rename(&temporary, path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    pub fn clear_history(&mut self) {
        self.tasks.retain(|task| !task.stage.terminal());
    }
    pub fn snapshot(&self) -> ManagerSnapshot {
        ManagerSnapshot {
            tasks: self.tasks.clone(),
            saved: self.saved.clone(),
            locations: self.locations.clone(),
        }
    }
}
