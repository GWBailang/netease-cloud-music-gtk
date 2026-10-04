#![allow(dead_code)]
use std::{fs, path::PathBuf};
pub struct TempDir(pub PathBuf);
impl TempDir {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ncm-test-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub fn song(id: u64) -> ncm_api::SongInfo {
    ncm_api::SongInfo {
        id,
        name: "歌名".into(),
        singer: "歌手".into(),
        album: "专辑".into(),
        album_id: 1,
        pic_url: String::new(),
        duration: 10000,
        song_url: String::new(),
        quality: Default::default(),
        copyright: ncm_api::SongCopyright::Free,
    }
}
