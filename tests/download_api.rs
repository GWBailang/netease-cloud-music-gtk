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
