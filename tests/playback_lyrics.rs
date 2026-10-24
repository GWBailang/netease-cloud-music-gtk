mod common;
use futures_lite::future::block_on;
use netease_cloud_music_gtk4::playback_lyrics::{LyricsRequest, load as load_if_current};

async fn load(
    cache: &std::path::Path,
    song_id: u64,
    refresh: bool,
    fetch: impl std::future::Future<Output = anyhow::Result<(Vec<String>, Vec<String>)>>,
) -> anyhow::Result<Vec<(u64, String)>> {
    load_if_current(cache, song_id, refresh, fetch, || true).await
}

#[test]
fn refresh_bypasses_cache_and_replaces_translation() {
    let temp = common::TempDir::new();
    let first = block_on(load(&temp.0, 1, false, async {
        Ok((
            vec!["[00:01:250][00:02]original".into()],
            vec!["[00:01.250]translation".into()],
        ))
    }))
    .unwrap();
    assert_eq!(
        first,
        vec![
            (1250, "original\n".into()),
            (1250, "translation\n".into()),
            (2000, "original\n".into())
        ]
    );
    let cached = block_on(load(&temp.0, 1, false, async {
        panic!("cache hit must not fetch")
    }))
    .unwrap();
    assert_eq!(first, cached);
    let refreshed = block_on(load(&temp.0, 1, true, async {
        Ok((vec!["new untimed lyrics".into()], vec![]))
    }))
    .unwrap();
    assert_eq!(refreshed, vec![(0, "new untimed lyrics\n".into())]);
    assert_eq!(
        block_on(load(&temp.0, 1, false, async {
            panic!("refresh must replace cache")
        }))
        .unwrap(),
        refreshed
    );
}

#[test]
fn broken_cache_is_refetched_and_failed_refresh_preserves_good_cache() {
    let temp = common::TempDir::new();
    std::fs::write(temp.0.join("2.json"), b"broken").unwrap();
    let lyrics = block_on(load(&temp.0, 2, false, async {
        Ok((vec!["[01:02.3]good".into()], vec![]))
    }))
    .unwrap();
    assert_eq!(lyrics, vec![(62300, "good\n".into())]);
    assert!(block_on(load(&temp.0, 2, true, async { anyhow::bail!("offline") })).is_err());
    assert_eq!(
        block_on(load(&temp.0, 2, false, async {
            panic!("keep cache on failure")
        }))
        .unwrap(),
        lyrics
    );
    assert!(
        block_on(load(&temp.0, 3, false, async { Ok((vec![], vec![])) }))
            .unwrap()
            .is_empty()
    );
    assert!(
        !temp.0.join("3.json").exists(),
        "an empty response must not poison future retries"
    );
}

#[test]
fn unwritable_cache_does_not_hide_downloaded_lyrics() {
    let temp = common::TempDir::new();
    let path = temp.0.join("file");
    std::fs::write(&path, b"not a directory").unwrap();
    assert_eq!(
        block_on(load(&path, 1, false, async {
            Ok((vec!["line".into()], vec![]))
        }))
        .unwrap(),
        vec![(0, "line\n".into())]
    );
}

#[test]
fn only_latest_song_or_refresh_request_can_update_display() {
    let mut state = LyricsRequest::default();
    let first = state.begin(1, 1500, false).unwrap();
    assert!(
        state.begin(1, 2000, false).is_none(),
        "position updates must not fetch again"
    );
    assert_eq!(state.position(), 2000);
    let second = state.begin(2, 0, false).unwrap();
    assert!(!state.is_current(first));
    let refresh = state.begin(2, 1000, true).unwrap();
    assert!(!state.is_current(second));
    assert!(state.is_current(refresh));
}

#[test]
fn superseded_response_cannot_overwrite_or_delete_refreshed_cache() {
    for old_is_empty in [false, true] {
        let temp = common::TempDir::new();
        let (tx, rx) = async_channel::bounded::<()>(1);
        let generation = std::cell::Cell::new(1);
        let older = load_if_current(
            &temp.0,
            1,
            true,
            async {
                rx.recv().await.unwrap();
                Ok((
                    if old_is_empty {
                        vec![]
                    } else {
                        vec!["old".into()]
                    },
                    vec![],
                ))
            },
            || generation.get() == 1,
        );
        let mut older = std::pin::pin!(older);
        assert!(block_on(futures_lite::future::poll_once(&mut older)).is_none());
        generation.set(2);
        let newer = block_on(load(&temp.0, 1, true, async {
            Ok((vec!["fresh".into()], vec![]))
        }))
        .unwrap();
        tx.try_send(()).unwrap();
        block_on(older).unwrap();
        assert_eq!(
            block_on(load(&temp.0, 1, false, async {
                panic!("latest cache must survive")
            }))
            .unwrap(),
            newer
        );
    }
}
