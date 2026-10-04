use crate::{CryptoApi, MusicApi, SongQuality};
use anyhow::{Result, anyhow, ensure};
use futures_lite::io::AsyncReadExt;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct DownloadResource {
    pub id: u64,
    pub url: String,
    pub codec: String,
    pub expected_duration_ms: u64,
    pub preview: bool,
    pub source_rate: u32,
}
impl DownloadResource {
    pub fn parse(value: &Value, duration: u64) -> Result<Self> {
        let url = value["url"].as_str().unwrap_or("");
        ensure!(!url.is_empty(), "Full audio is unavailable");
        ensure!(duration > 0, "Cannot verify the full song duration");
        ensure!(
            value["code"].as_i64().is_none_or(|code| code == 200),
            "Full audio is unavailable"
        );
        Ok(Self {
            id: value["id"]
                .as_u64()
                .ok_or_else(|| anyhow!("Missing song ID"))?,
            url: url.into(),
            codec: value["type"].as_str().unwrap_or("").into(),
            expected_duration_ms: duration,
            preview: !value["freeTrialInfo"].is_null()
                || value["isTrial"].as_bool().unwrap_or(false),
            source_rate: value["br"].as_u64().unwrap_or(0) as u32,
        })
    }
}
#[derive(Debug, Clone)]
pub struct PlaylistTracks {
    pub id: u64,
    pub name: String,
    pub track_ids: Vec<u64>,
    pub total: usize,
}

async fn wait_cancel(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        async_io::Timer::after(Duration::from_millis(50)).await;
    }
}
impl MusicApi {
    pub async fn download_resource(&self, id: u64, bitrate: u32) -> Result<DownloadResource> {
        let quality = match bitrate {
            128 => SongQuality::Standard,
            192 => SongQuality::Higher,
            320 => SongQuality::Extreme,
            _ => return Err(anyhow!("Unsupported MP3 bitrate")),
        };
        let ids = format!("[{id}]");
        let level = quality.as_level();
        let params = HashMap::from([
            ("ids", ids.as_str()),
            ("level", level),
            ("encodeType", "mp3"),
        ]);
        let json = self
            .request(
                crate::Method::Post,
                "/api/song/enhance/player/url/v1",
                params,
                CryptoApi::Eapi,
                "",
                true,
            )
            .await
            .map_err(|_| anyhow!("Cannot obtain full audio; check login and availability"))?;
        let json: Value = serde_json::from_str(&json)?;
        ensure!(
            json["code"].as_i64() == Some(200),
            "Cannot obtain full audio"
        );
        let details = self
            .songs_detail(&[id])
            .await
            .map_err(|_| anyhow!("Cannot verify the song duration"))?;
        let duration = details
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.duration)
            .unwrap_or(0);
        let item = json["data"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["id"].as_u64() == Some(id)))
            .ok_or_else(|| anyhow!("Full audio is unavailable"))?;
        DownloadResource::parse(item, duration)
    }
    pub async fn playlist_tracks(&self, id: u64) -> Result<PlaylistTracks> {
        let id_string = id.to_string();
        let params = HashMap::from([("id", id_string.as_str()), ("n", "1000"), ("s", "0")]);
        let json = self
            .request(
                crate::Method::Post,
                "/weapi/v6/playlist/detail",
                params,
                CryptoApi::Weapi,
                "",
                true,
            )
            .await
            .map_err(|_| anyhow!("Cannot load the complete playlist"))?;
        let value: Value = serde_json::from_str(&json)?;
        ensure!(
            value["code"].as_i64() == Some(200),
            "Cannot load the complete playlist"
        );
        let list = &value["playlist"];
        let ids = list["trackIds"]
            .as_array()
            .ok_or_else(|| anyhow!("Complete playlist IDs are unavailable"))?
            .iter()
            .map(|v| {
                v["id"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("Invalid playlist song ID"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PlaylistTracks {
            id,
            name: list["name"].as_str().unwrap_or("Playlist").into(),
            track_ids: ids,
            total: list["trackCount"]
                .as_u64()
                .ok_or_else(|| anyhow!("Complete playlist count is unavailable"))?
                as usize,
        })
    }
    pub async fn download_to(
        &self,
        url: &str,
        path: &Path,
        cancelled: &AtomicBool,
        progress: impl Fn(u64, Option<u64>),
    ) -> Result<()> {
        ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
        let request = async {
            self.client
                .get_async(url)
                .await
                .map_err(|_| anyhow!("Network download failed"))
        };
        let cancel = async {
            wait_cancel(cancelled).await;
            Err(anyhow!("Download cancelled"))
        };
        let mut response = futures_lite::future::race(request, cancel).await?;
        ensure!(
            response.status().is_success(),
            "Audio server returned HTTP {}",
            response.status().as_u16()
        );
        let length = response
            .headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<u64>().ok());
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let result: Result<()> = async {
            let mut buffer = [0u8; 65536];
            let mut total = 0u64;
            loop {
                ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
                let read = response.body_mut().read(&mut buffer);
                let cancel = async {
                    wait_cancel(cancelled).await;
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "Download cancelled",
                    ))
                };
                let n = futures_lite::future::race(read, cancel)
                    .await
                    .map_err(|_| anyhow!("Audio transfer failed or was cancelled"))?;
                if n == 0 {
                    break;
                }
                file.write_all(&buffer[..n])?;
                total += n as u64;
                progress(total, length);
            }
            ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
            ensure!(
                total > 0 && length.is_none_or(|n| n == total),
                "Audio transfer was empty or incomplete"
            );
            file.sync_all()?;
            Ok(())
        }
        .await;
        drop(file);
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }
}
