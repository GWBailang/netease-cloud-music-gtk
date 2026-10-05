use super::{lyrics::LyricDocument, model::*};
use anyhow::{Result, ensure};
use ncm_api::{MusicApi, PlaylistTracks, SongInfo};
use std::collections::{HashMap, HashSet};

pub fn assemble_collection(
    tracks: PlaylistTracks,
    songs: Vec<SongInfo>,
) -> Result<CollectionSnapshot> {
    // The service can undercount trackIds. Keep all IDs, but reject a shorter list.
    ensure!(
        tracks.track_ids.len() >= tracks.total,
        "Cannot verify all playlist tracks"
    );
    let mut details: HashMap<_, _> = songs.into_iter().map(|s| (s.id, s)).collect();
    let mut seen = HashSet::new();
    let mut duplicates = 0;
    let mut items = Vec::new();
    for id in tracks.track_ids {
        if !seen.insert(id) {
            duplicates += 1;
            continue;
        }
        let song = details.remove(&id);
        let error = if song.is_none() {
            Some("Song details are unavailable".into())
        } else {
            None
        };
        items.push(CollectionItem {
            song_id: id,
            song,
            error,
        });
    }
    Ok(CollectionSnapshot {
        id: tracks.id,
        name: tracks.name,
        kind: "playlist".into(),
        items,
        duplicates,
    })
}
pub async fn complete_collection(client: &MusicApi, id: u64) -> Result<CollectionSnapshot> {
    let tracks = client.playlist_tracks(id).await?;
    let mut songs = Vec::new();
    for ids in tracks.track_ids.chunks(200) {
        if let Ok(mut details) = client.songs_detail(ids).await {
            songs.append(&mut details);
        }
    }
    assemble_collection(tracks, songs)
}
pub async fn fetch_lyrics(client: &MusicApi, id: u64) -> Result<LyricDocument> {
    let lyrics = client
        .song_lyric(id)
        .await
        .map_err(|_| anyhow::anyhow!("No lyrics available"))?;
    Ok(LyricDocument::parse(&lyrics.lyric, &lyrics.tlyric))
}
