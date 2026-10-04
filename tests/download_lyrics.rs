use netease_cloud_music_gtk4::download::{lyrics::*, model::*};
fn lines(s: &str) -> Vec<String> {
    s.lines().map(str::to_owned).collect()
}
#[test]
fn expands_timestamps_and_aligns_translation() {
    let doc = LyricDocument::parse(
        &lines("[00:01.00][00:02.00]原文\n[00:03:20]结尾"),
        &lines("[00:01.00]translation"),
    );
    let output = doc
        .render(
            LyricFormat::Lrc,
            LyricMode::Bilingual,
            TextEncoding::Utf8,
            Some(5000),
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(output[0].bytes.clone()).unwrap(),
        "[00:01.00]原文\r\n[00:01.00]translation\r\n[00:02.00]原文\r\n[00:03.20]结尾\r\n"
    );
}
#[test]
fn txt_without_timestamps_is_available() {
    let doc = LyricDocument::parse(&lines("[ar:歌手]\n没有时间的歌词"), &[]);
    assert_eq!(
        doc.render(
            LyricFormat::Txt,
            LyricMode::Original,
            TextEncoding::Utf8,
            None
        )
        .unwrap()[0]
            .bytes,
        "没有时间的歌词\r\n".as_bytes()
    );
    assert!(
        doc.render(
            LyricFormat::Lrc,
            LyricMode::Original,
            TextEncoding::Utf8,
            None
        )
        .is_err()
    );
    assert!(
        doc.render(
            LyricFormat::Srt,
            LyricMode::Original,
            TextEncoding::Utf8,
            None
        )
        .is_err()
    );
}
#[test]
fn subtitles_merge_same_start_and_end_after_three_seconds() {
    let doc = LyricDocument::parse(&lines("[00:01.00]一\n[00:01.00]二"), &[]);
    let output = doc
        .render(
            LyricFormat::Srt,
            LyricMode::Original,
            TextEncoding::Utf8,
            None,
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(output[0].bytes.clone()).unwrap(),
        "1\r\n00:00:01,000 --> 00:00:04,000\r\n一\r\n二\r\n\r\n"
    );
}
#[test]
fn gbk_never_silently_replaces_characters() {
    let doc = LyricDocument::parse(&lines("[00:01.00]歌词🎵"), &[]);
    assert!(
        doc.render(
            LyricFormat::Lrc,
            LyricMode::Original,
            TextEncoding::Gbk,
            None
        )
        .is_err()
    );
    let chinese = LyricDocument::parse(&lines("[00:01.00]歌词"), &[]);
    assert!(
        chinese
            .render(
                LyricFormat::Lrc,
                LyricMode::Original,
                TextEncoding::Gbk,
                None
            )
            .is_ok()
    );
}
#[test]
fn separate_translation_has_its_own_suffix() {
    let doc = LyricDocument::parse(&lines("[00:01.00]原"), &lines("[00:01.00]译"));
    let output = doc
        .render(
            LyricFormat::Txt,
            LyricMode::Separate,
            TextEncoding::Utf8,
            None,
        )
        .unwrap();
    assert_eq!(output[1].suffix, ".translated.txt");
    assert_eq!(output[1].bytes, "译\r\n".as_bytes());
}
