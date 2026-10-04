mod common;
use gstreamer::{self as gst, prelude::*};
use netease_cloud_music_gtk4::download::audio::*;
use std::{path::Path, sync::atomic::AtomicBool};
fn sample(path: &Path, encoding: &str) {
    gst::init().unwrap();
    let launch = format!(
        "audiotestsrc num-buffers=200 ! audio/x-raw,rate=44100,channels=2 ! audioconvert ! {encoding} ! filesink location=\"{}\"",
        path.display()
    );
    let pipeline = gst::parse::launch(&launch).unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let message = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(15),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    pipeline.set_state(gst::State::Null).unwrap();
    if let gst::MessageView::Error(err) = message.view() {
        panic!("sample error: {}", err.error());
    }
}
#[test]
fn produces_verified_mp3_from_aac_flac_and_mp3() {
    for (ext, encoder) in [
        (
            "aac",
            "avenc_aac ! aacparse ! audio/mpeg,stream-format=adts",
        ),
        ("flac", "flacenc"),
        ("mp3", "lamemp3enc target=bitrate cbr=true bitrate=128"),
    ] {
        let temp = common::TempDir::new();
        let input = temp.0.join(format!("input.{ext}"));
        let output = temp.0.join("output.mp3");
        sample(&input, encoder);
        let info = prepare_mp3(
            &input,
            &output,
            &common::song(1),
            192,
            Some(1),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(info.mp3);
        assert_eq!(info.sample_rate, 44100);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bitrate, 192000);
        assert!(info.duration_ms > 4000);
        let tag = id3::Tag::read_from_path(&output).unwrap();
        use id3::TagLike;
        assert_eq!(tag.version(), id3::Version::Id3v23);
        assert_eq!(tag.title(), Some("歌名"));
        assert_eq!(tag.track(), Some(1));
    }
}
#[test]
fn short_song_is_valid_but_preview_and_truncation_are_rejected() {
    assert!(verify_complete(10000, 10000, false).is_ok());
    assert!(verify_complete(10000, 10000, true).is_err());
    assert!(verify_complete(90000, 100000, false).is_err());
    assert!(verify_complete(10000, 0, false).is_err());
}
#[test]
fn cancellation_never_creates_completed_audio() {
    let temp = common::TempDir::new();
    let output = temp.0.join("output.mp3");
    assert!(
        prepare_mp3(
            &temp.0.join("input"),
            &output,
            &common::song(1),
            192,
            None,
            &AtomicBool::new(true),
            |_| {}
        )
        .is_err()
    );
    assert!(!output.exists());
}
#[test]
fn retains_matching_mp3_frames_without_reencoding() {
    let temp = common::TempDir::new();
    let input = temp.0.join("input.mp3");
    let output = temp.0.join("output.mp3");
    sample(&input, "lamemp3enc target=bitrate cbr=true bitrate=192");
    prepare_mp3(
        &input,
        &output,
        &common::song(1),
        192,
        None,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let original = std::fs::read(&input).unwrap();
    let tagged = std::fs::read(&output).unwrap();
    let offset = 10
        + ((tagged[6] as usize) << 21
            | (tagged[7] as usize) << 14
            | (tagged[8] as usize) << 7
            | tagged[9] as usize);
    assert_eq!(&tagged[offset..], original.as_slice());
}
#[test]
fn scanning_uses_embedded_song_tags() {
    let temp = common::TempDir::new();
    let input = temp.0.join("input.mp3");
    sample(&input, "lamemp3enc target=bitrate cbr=true bitrate=192");
    let output = temp.0.join("numbered.mp3");
    prepare_mp3(
        &input,
        &output,
        &common::song(1),
        192,
        None,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let target = netease_cloud_music_gtk4::download::model::TargetLocation::capture(
        &temp.0, false, None, None,
    )
    .unwrap();
    let items = netease_cloud_music_gtk4::download::storage::scan(&target).unwrap();
    assert_eq!(
        items
            .iter()
            .find(|s| s.files[0].relative == Path::new("numbered.mp3"))
            .unwrap()
            .title,
        "歌手 - 歌名"
    );
}
