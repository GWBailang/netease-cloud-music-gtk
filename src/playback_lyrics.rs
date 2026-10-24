//! Playback lyric caching and request ordering, independent of the GTK view.
use crate::download::lyrics::LyricDocument;
use anyhow::Result;
use std::{fs, future::Future, path::Path};

pub async fn load(
    cache: &Path,
    song_id: u64,
    refresh: bool,
    fetch: impl Future<Output = Result<(Vec<String>, Vec<String>)>>,
    is_current: impl Fn() -> bool,
) -> Result<Vec<(u64, String)>> {
    // Numeric IDs avoid collisions and slashes in song/artist/album names.
    let path = cache.join(format!("{song_id}.json"));
    if !refresh {
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(lyrics) = serde_json::from_slice::<ncm_api::Lyrics>(&bytes) {
                let lines = LyricDocument::parse(&lyrics.lyric, &lyrics.tlyric).playback_lines();
                if !lines.is_empty() {
                    return Ok(lines);
                }
            }
        }
    }
    let (lyric, tlyric) = fetch.await?;
    let lines = LyricDocument::parse(&lyric, &tlyric).playback_lines();
    // A late response must not replace (or delete) a newer refresh's cache.
    // No await occurs between this guard and the atomic cache replacement.
    if !is_current() {
        return Ok(lines);
    }
    if !lines.is_empty() {
        let temporary = cache.join(format!(".{song_id}-{}.tmp", fastrand::u64(..)));
        let saved = (|| -> Result<()> {
            fs::create_dir_all(cache)?;
            fs::write(
                &temporary,
                serde_json::to_vec(&ncm_api::Lyrics { lyric, tlyric })?,
            )?;
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if let Err(error) = saved {
            let _ = fs::remove_file(&temporary);
            log::warn!("Unable to cache lyrics for {song_id}: {error}");
        }
    } else if refresh {
        // A successful empty response must not leave stale lyrics in the cache.
        let _ = fs::remove_file(path);
    }
    Ok(lines)
}

#[derive(Debug, Default)]
pub struct LyricsRequest {
    song: Option<u64>,
    generation: u64,
    position: u64,
}
impl LyricsRequest {
    pub fn begin(&mut self, song: u64, position: u64, refresh: bool) -> Option<u64> {
        self.position = position;
        if self.song == Some(song) && !refresh {
            return None;
        }
        self.song = Some(song);
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }
    pub fn position(&self) -> u64 {
        self.position
    }
}
