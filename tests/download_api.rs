mod common;
use ncm_api::{DownloadResource, MusicApi, PlaylistTracks};
use netease_cloud_music_gtk4::download::api::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[test]
fn preserves_preview_and_rejects_empty_links() {
    let json = serde_json::json!({"id":1,"url":"https://example.test/audio","type":"aac","br":128000,"time":30000,"freeTrialInfo":{"start":0,"end":30}});
    let resource = DownloadResource::parse(&json, 100000).unwrap();
    assert!(resource.preview);
    assert_eq!(resource.expected_duration_ms, 100000);
    assert!(DownloadResource::parse(&serde_json::json!({"id":1,"url":null}), 100000).is_err());
}
#[test]
fn full_collection_keeps_missing_and_deduplicates_in_order() {
    let tracks = PlaylistTracks {
        id: 1,
        name: "list".into(),
        track_ids: vec![3, 1, 3, 2],
        total: 4,
    };
    let snapshot = assemble_collection(tracks, vec![common::song(1), common::song(3)]).unwrap();
    assert_eq!(
        snapshot.items.iter().map(|s| s.song_id).collect::<Vec<_>>(),
        vec![3, 1, 2]
    );
    assert!(snapshot.items[2].error.is_some());
    assert_eq!(snapshot.duplicates, 1);
    assert!(
        assemble_collection(
            PlaylistTracks {
                id: 1,
                name: "bad".into(),
                track_ids: vec![1],
                total: 1001
            },
            vec![common::song(1)]
        )
        .is_err()
    );
}
#[test]
fn enumerates_more_than_one_thousand_tracks() {
    let tracks = PlaylistTracks {
        id: 1,
        name: "large".into(),
        track_ids: (1..=1001).collect(),
        total: 1001,
    };
    let snapshot = assemble_collection(tracks, (1..=1001).map(common::song).collect()).unwrap();
    assert_eq!(snapshot.items.len(), 1001);
    assert_eq!(snapshot.items[1000].song_id, 1001);
}
#[test]
fn playlist_keeps_all_returned_tracks_when_total_understates_the_list() {
    let snapshot = assemble_collection(
        PlaylistTracks {
            id: 1,
            name: "reported playlist".into(),
            track_ids: (1..=29).collect(),
            total: 28,
        },
        (1..=29).map(common::song).collect(),
    )
    .expect("a surplus track must not block an otherwise complete playlist");
    assert_eq!(snapshot.items.len(), 29);
    assert_eq!(snapshot.items.last().unwrap().song_id, 29);
    assert!(snapshot.items.iter().all(|item| item.song.is_some()));
}

fn server(response: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/audio", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0; 4096];
        let _ = stream.read(&mut buf);
        let _ = stream.write_all(response);
    });
    url
}
#[test]
fn streaming_http_errors_and_truncation_are_failures() {
    let temp = common::TempDir::new();
    let mut client = MusicApi::new(2);
    for response in [
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort".as_slice(),
    ] {
        let url = server(response);
        client.set_proxy(&url).unwrap();
        let result = futures_lite::future::block_on(client.download_to(
            &url,
            &temp.0.join("part"),
            &AtomicBool::new(false),
            |_, _| {},
        ));
        let message = result.unwrap_err().to_string();
        if response.starts_with(b"HTTP/1.1 404") {
            assert!(message.contains("HTTP 404"), "{message}");
        } else {
            assert!(message.contains("transfer"), "{message}");
        }
    }
}
#[test]
fn cancelled_stream_never_succeeds() {
    let temp = common::TempDir::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let url = server(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678");
    let mut client = MusicApi::new(2);
    client.set_proxy(&url).unwrap();
    let result = futures_lite::future::block_on(client.download_to(
        &url,
        &temp.0.join("part"),
        &cancel,
        move |_, _| {
            signal.store(true, Ordering::Relaxed);
        },
    ));
    assert!(result.is_err());
    assert!(cancel.load(Ordering::Relaxed));
}

#[test]
fn song_detail_request_ids_are_json_and_preserve_large_ids() {
    let query = MusicApi::song_detail_ids(&[3422762511, 3411982875]);
    let parsed: serde_json::Value = serde_json::from_str(&query)
        .expect("song-detail c parameter must be a JSON array, without extra escaping");
    assert_eq!(
        parsed,
        serde_json::json!([{"id":3422762511u64},{"id":3411982875u64}])
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&MusicApi::song_detail_ids(&[])).unwrap(),
        serde_json::json!([])
    );
}

#[test]
#[ignore = "manual: needs NCM_READONLY_COOKIE_FILE; downloads into temporary directories"]
fn live_reported_songs_have_verifiable_download_resources() {
    let client = live_client();

    for id in [3422762511, 3411982875] {
        let details = futures_lite::future::block_on(client.songs_detail(&[id])).unwrap();
        let song = details
            .iter()
            .find(|s| s.id == id)
            .expect("requested song missing");
        assert!(song.duration > 0);
        let resource = futures_lite::future::block_on(client.download_resource(id, 192)).unwrap();
        assert_eq!(resource.expected_duration_ms, song.duration);
        assert!(!resource.preview, "only a preview is available");
        let temp = common::TempDir::new();
        let source = temp.0.join("source");
        let output = temp.0.join("verified.mp3");
        let cancelled = AtomicBool::new(false);
        futures_lite::future::block_on(client.download_to(
            &resource.url,
            &source,
            &cancelled,
            |_, _| {},
        ))
        .unwrap();
        let audio = netease_cloud_music_gtk4::download::audio::prepare_mp3(
            &source,
            &output,
            song,
            192,
            None,
            &cancelled,
            |_| {},
        )
        .unwrap();
        netease_cloud_music_gtk4::download::audio::verify_complete(
            audio.duration_ms,
            song.duration,
            false,
        )
        .unwrap();
        assert!(audio.mp3 && audio.constant_bitrate);
        assert_eq!(
            (audio.bitrate, audio.sample_rate, audio.channels),
            (192000, 44100, 2)
        );
        println!(
            "Verified song {id}: {} bytes, {}ms, MP3 CBR 192kbps, 44.1kHz stereo",
            std::fs::metadata(output).unwrap().len(),
            audio.duration_ms
        );
    }
}

fn live_client() -> MusicApi {
    let cookie_path = std::env::var("NCM_READONLY_COOKIE_FILE")
        .expect("set NCM_READONLY_COOKIE_FILE to the existing application cookie file");
    let file = std::fs::File::open(cookie_path).unwrap();
    let store = cookie_store::serde::json::load(std::io::BufReader::new(file)).unwrap();
    let jar = ncm_api::CookieJar::default();
    for base in [
        "https://music.163.com/",
        "https://music.163.com/eapi/clientlog",
        "https://music.163.com/eapi/feedback",
        "https://music.163.com/api/clientlog",
        "https://music.163.com/api/feedback",
        "https://music.163.com/neapi/clientlog",
        "https://music.163.com/neapi/feedback",
        "https://music.163.com/weapi/clientlog",
        "https://music.163.com/weapi/feedback",
        "https://music.163.com/wapi/clientlog",
        "https://music.163.com/wapi/feedback",
        "https://music.163.com/openapi/clientlog",
    ] {
        let url = base.parse().unwrap();
        for c in store.matches(&url) {
            let cookie = ncm_api::CookieBuilder::new(c.name(), c.value())
                .domain("music.163.com")
                .path(c.path().unwrap_or("/"))
                .build()
                .unwrap();
            jar.set(cookie, &base.parse().unwrap()).unwrap();
        }
    }
    MusicApi::from_cookie_jar(jar, 2)
}

#[test]
#[ignore = "manual: needs NCM_READONLY_COOKIE_FILE, NCM_LIVE_PLAYLIST_ID and NCM_LIVE_EXPECTED_TRACKS"]
fn live_reported_playlist_can_be_enumerated() {
    let client = live_client();
    let id = std::env::var("NCM_LIVE_PLAYLIST_ID")
        .expect("set the reported playlist ID")
        .parse()
        .unwrap();
    let expected: usize = std::env::var("NCM_LIVE_EXPECTED_TRACKS")
        .expect("set the expected playlist size")
        .parse()
        .unwrap();
    let snapshot = futures_lite::future::block_on(complete_collection(&client, id)).unwrap();
    assert_eq!(snapshot.items.len(), expected);
    assert!(
        snapshot.items.iter().all(|item| item.song.is_some()),
        "some track details are missing"
    );
    println!(
        "Verified playlist {id}: {} tracks, {} duplicate entries",
        snapshot.items.len(),
        snapshot.duplicates
    );
}
