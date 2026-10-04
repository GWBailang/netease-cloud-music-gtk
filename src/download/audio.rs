use anyhow::{Result, anyhow, ensure};
use gstreamer::{self as gst, prelude::*};
use gstreamer_pbutils::{Discoverer, DiscovererResult, prelude::*};
use gtk::{gio, prelude::*};
use id3::TagLike;
use ncm_api::SongInfo;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub mp3: bool,
    pub sample_rate: u32,
    pub channels: u32,
    pub bitrate: u32,
    pub duration_ms: u64,
    pub constant_bitrate: bool,
}
pub fn check_plugins() -> Result<()> {
    gst::init()?;
    for name in [
        "filesrc",
        "filesink",
        "decodebin",
        "audioconvert",
        "audioresample",
        "capsfilter",
        "lamemp3enc",
        "mpegaudioparse",
    ] {
        ensure!(
            gst::ElementFactory::find(name).is_some(),
            "Missing GStreamer plugin: {name}"
        );
    }
    Ok(())
}
pub fn probe(path: &Path) -> Result<AudioInfo> {
    gst::init()?;
    let uri = gio::File::for_path(path.canonicalize()?).uri();
    let info = Discoverer::new(gst::ClockTime::from_seconds(10))?
        .discover_uri(&uri)
        .map_err(|_| anyhow!("Cannot decode the audio file; check GStreamer decoder plugins"))?;
    ensure!(
        info.result() == DiscovererResult::Ok,
        "Incomplete or unsupported audio file"
    );
    let streams = info.audio_streams();
    let stream = streams
        .first()
        .ok_or_else(|| anyhow!("No audio stream found"))?;
    let caps = stream
        .caps()
        .ok_or_else(|| anyhow!("Missing audio format"))?;
    let structure = caps
        .structure(0)
        .ok_or_else(|| anyhow!("Missing audio format"))?;
    let mp3 = structure.name() == "audio/mpeg"
        && structure.get::<i32>("mpegversion").ok() == Some(1)
        && structure.get::<i32>("layer").ok() == Some(3);
    let duration_ms = info.duration().map(|d| d.mseconds()).unwrap_or(0);
    ensure!(duration_ms > 0, "Unknown audio duration");
    let (constant_bitrate, frame_rate) = if mp3 {
        mp3_frames(path).unwrap_or((false, 0))
    } else {
        (false, 0)
    };
    Ok(AudioInfo {
        mp3,
        sample_rate: stream.sample_rate(),
        channels: stream.channels(),
        bitrate: if frame_rate > 0 {
            frame_rate
        } else {
            stream.bitrate()
        },
        duration_ms,
        constant_bitrate,
    })
}
fn mp3_frames(path: &Path) -> Result<(bool, u32)> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut first = [0; 10];
    file.read_exact(&mut first)?;
    let mut offset = if &first[..3] == b"ID3" {
        10 + ((first[6] as u64) << 21
            | (first[7] as u64) << 14
            | (first[8] as u64) << 7
            | first[9] as u64)
    } else {
        0
    };
    let mut nominal = 0;
    let mut frames = 0;
    let mut constant = true;
    while offset + 4 <= size {
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0; 4];
        file.read_exact(&mut header)?;
        if &header[..3] == b"TAG" {
            break;
        }
        ensure!(
            header[0] == 0xff && header[1] & 0xfe == 0xfa,
            "Invalid MPEG Layer III frame"
        );
        let rate = [44100u32, 48000, 32000, 0][((header[2] >> 2) & 3) as usize];
        let bitrate = [
            0u32, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
        ][(header[2] >> 4) as usize]
            * 1000;
        ensure!(rate > 0 && bitrate > 0, "Invalid MP3 frame parameters");
        if nominal == 0 {
            nominal = bitrate;
        } else if nominal != bitrate {
            constant = false;
        }
        let length = (144 * bitrate / rate) as u64 + ((header[2] >> 1) & 1) as u64;
        ensure!(offset + length <= size, "Truncated MP3 frame");
        offset += length;
        frames += 1;
    }
    ensure!(frames > 0, "No valid MP3 frames");
    Ok((constant, nominal))
}
pub fn verify_complete(actual_ms: u64, expected_ms: u64, preview: bool) -> Result<()> {
    ensure!(
        !preview,
        "Only a preview is available; full song was not downloaded"
    );
    ensure!(
        expected_ms > 0 && actual_ms > 0,
        "Cannot verify full audio duration"
    );
    ensure!(
        actual_ms.abs_diff(expected_ms) <= 2000u64.max(expected_ms / 100),
        "Audio duration does not match the full song"
    );
    Ok(())
}
struct PipelineGuard(gst::Pipeline);
impl Drop for PipelineGuard {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}
pub fn prepare_mp3(
    input: &Path,
    output: &Path,
    song: &SongInfo,
    bitrate: u32,
    ordinal: Option<usize>,
    cancelled: &AtomicBool,
    progress: impl Fn(f64),
) -> Result<AudioInfo> {
    ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
    ensure!(
        [128, 192, 320].contains(&bitrate),
        "Unsupported MP3 bitrate"
    );
    check_plugins()?;
    let source = probe(input)?;
    let result = (|| -> Result<AudioInfo> {
        if source.mp3
            && source.sample_rate == 44100
            && source.channels == 2
            && source.bitrate == bitrate * 1000
            && source.constant_bitrate
        {
            fs::copy(input, output)?;
        } else {
            let pipeline = gst::Pipeline::new();
            let guard = PipelineGuard(pipeline.clone());
            let src = gst::ElementFactory::make("filesrc")
                .property(
                    "location",
                    input
                        .to_str()
                        .ok_or_else(|| anyhow!("Audio path is not valid UTF-8"))?,
                )
                .build()?;
            let decode = gst::ElementFactory::make("decodebin").build()?;
            let convert = gst::ElementFactory::make("audioconvert").build()?;
            let resample = gst::ElementFactory::make("audioresample").build()?;
            let caps = gst::Caps::builder("audio/x-raw")
                .field("format", "S16LE")
                .field("rate", 44100i32)
                .field("channels", 2i32)
                .build();
            let filter = gst::ElementFactory::make("capsfilter")
                .property("caps", caps)
                .build()?;
            let encoder = gst::ElementFactory::make("lamemp3enc")
                .property_from_str("target", "bitrate")
                .property("cbr", true)
                .property("bitrate", bitrate as i32)
                .build()?;
            let parser = gst::ElementFactory::make("mpegaudioparse").build()?;
            let sink = gst::ElementFactory::make("filesink")
                .property(
                    "location",
                    output
                        .to_str()
                        .ok_or_else(|| anyhow!("Audio path is not valid UTF-8"))?,
                )
                .build()?;
            pipeline.add_many([
                &src, &decode, &convert, &resample, &filter, &encoder, &parser, &sink,
            ])?;
            src.link(&decode)?;
            gst::Element::link_many([&convert, &resample, &filter, &encoder, &parser, &sink])?;
            let convert_clone = convert.clone();
            decode.connect_pad_added(move |_, pad| {
                let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
                if caps
                    .structure(0)
                    .is_some_and(|s| s.name().starts_with("audio/"))
                {
                    if let Some(sink) = convert_clone.static_pad("sink") {
                        if !sink.is_linked() {
                            let _ = pad.link(&sink);
                        }
                    }
                }
            });
            pipeline.set_state(gst::State::Playing)?;
            let bus = pipeline
                .bus()
                .ok_or_else(|| anyhow!("Missing conversion bus"))?;
            let started = Instant::now();
            loop {
                ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
                ensure!(
                    started.elapsed()
                        < Duration::from_millis(source.duration_ms.saturating_mul(2) + 30000),
                    "Audio conversion timed out"
                );
                if let Some(message) = bus.timed_pop_filtered(
                    gst::ClockTime::from_mseconds(100),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                ) {
                    match message.view() {
                        gst::MessageView::Eos(_) => break,
                        gst::MessageView::Error(_) => {
                            return Err(anyhow!("Audio conversion failed; check decoder plugins"));
                        }
                        _ => {}
                    }
                }
                if let Some(position) = pipeline.query_position::<gst::ClockTime>() {
                    progress(
                        (position.mseconds() as f64 / source.duration_ms as f64).clamp(0.0, 1.0),
                    );
                }
            }
            drop(guard);
        }
        ensure!(!cancelled.load(Ordering::Relaxed), "Download cancelled");
        let mut tag = id3::Tag::new();
        tag.set_title(&song.name);
        tag.set_artist(&song.singer);
        tag.set_album(&song.album);
        if let Some(track) = ordinal {
            tag.set_track(track as u32);
        }
        tag.write_to_path(output, id3::Version::Id3v23)?;
        let info = probe(output)?;
        ensure!(
            info.mp3
                && info.sample_rate == 44100
                && info.channels == 2
                && info.bitrate == bitrate * 1000
                && info.constant_bitrate,
            "Output does not match MP3 settings"
        );
        verify_complete(info.duration_ms, source.duration_ms, false)?;
        File::open(output)?.sync_all()?;
        progress(1.0);
        Ok(info)
    })();
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}
