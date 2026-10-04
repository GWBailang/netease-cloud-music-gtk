use super::model::*;
use anyhow::{Result, bail, ensure};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::BTreeMap;

static TIME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[(\d+):(\d{1,2})(?:[.:](\d{1,3}))?\]").unwrap());
static META: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[(?:ar|al|ti|by|offset|length|re|ve):[^\]]*\]").unwrap());
#[derive(Debug, Clone, Default)]
struct Part {
    text: Vec<String>,
    events: Vec<(u64, String)>,
}
#[derive(Debug, Clone, Default)]
pub struct LyricDocument {
    original: Part,
    translated: Part,
}

fn parse_part(lines: &[String]) -> Part {
    let mut part = Part::default();
    for line in lines {
        let text = META.replace_all(line, "");
        let text = TIME.replace_all(&text, "").trim().to_owned();
        if text.is_empty() {
            continue;
        }
        part.text.push(text.clone());
        for caps in TIME.captures_iter(line) {
            let Some(minutes) = caps[1].parse::<u64>().ok() else {
                continue;
            };
            let Some(seconds) = caps[2].parse::<u64>().ok().filter(|n| *n < 60) else {
                continue;
            };
            let fraction = caps
                .get(3)
                .map(|s| {
                    let raw = s.as_str();
                    raw.parse::<u64>().unwrap_or(0) * 10u64.pow(3 - raw.len() as u32)
                })
                .unwrap_or(0);
            if let Some(time) = minutes
                .checked_mul(60000)
                .and_then(|n| n.checked_add(seconds * 1000 + fraction))
            {
                part.events.push((time, text.clone()));
            }
        }
    }
    part.events.sort_by_key(|(time, _)| *time);
    part
}
impl LyricDocument {
    pub fn parse(original: &[String], translated: &[String]) -> Self {
        Self {
            original: parse_part(original),
            translated: parse_part(translated),
        }
    }
    pub fn has_translation(&self) -> bool {
        !self.translated.text.is_empty()
    }
    pub fn render(
        &self,
        format: LyricFormat,
        mode: LyricMode,
        encoding: TextEncoding,
        duration_ms: Option<u64>,
    ) -> Result<Vec<LyricOutput>> {
        ensure!(!self.original.text.is_empty(), "No lyrics available");
        let suffix = format!(".{}", format.extension());
        let mut part = self.original.clone();
        if mode == LyricMode::Bilingual {
            part.events.extend(self.translated.events.clone());
            part.events.sort_by_key(|(time, _)| *time);
            if !part.events.is_empty() {
                part.text = part.events.iter().map(|(_, s)| s.clone()).collect();
            } else {
                part.text = (0..part.text.len().max(self.translated.text.len()))
                    .flat_map(|i| {
                        [self.original.text.get(i), self.translated.text.get(i)]
                            .into_iter()
                            .flatten()
                            .cloned()
                    })
                    .collect();
            }
        }
        let mut outputs = vec![LyricOutput {
            suffix,
            bytes: encode(&render_part(&part, format, duration_ms)?, encoding)?,
        }];
        if mode == LyricMode::Separate && self.has_translation() {
            outputs.push(LyricOutput {
                suffix: format!(".translated.{}", format.extension()),
                bytes: encode(
                    &render_part(&self.translated, format, duration_ms)?,
                    encoding,
                )?,
            });
        }
        Ok(outputs)
    }
}
fn render_part(part: &Part, format: LyricFormat, duration: Option<u64>) -> Result<String> {
    if format == LyricFormat::Txt {
        return Ok(format!("{}\r\n", part.text.join("\r\n")));
    }
    ensure!(
        !part.events.is_empty(),
        "Timed lyrics are unavailable; choose TXT"
    );
    if format == LyricFormat::Lrc {
        return Ok(part
            .events
            .iter()
            .map(|(t, s)| {
                format!(
                    "[{:02}:{:02}.{:02}]{}\r\n",
                    t / 60000,
                    t / 1000 % 60,
                    t / 10 % 100,
                    s
                )
            })
            .collect());
    }
    let mut grouped: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
    for (time, text) in &part.events {
        grouped.entry(*time).or_default().push(text);
    }
    let events: Vec<_> = grouped.into_iter().collect();
    let mut output = String::new();
    let mut count = 0;
    for (i, (start, text)) in events.iter().enumerate() {
        let end = events
            .get(i + 1)
            .map(|(t, _)| *t)
            .unwrap_or_else(|| duration.unwrap_or(start.saturating_add(3000)));
        if end <= *start {
            continue;
        }
        count += 1;
        output.push_str(&format!(
            "{count}\r\n{} --> {}\r\n{}\r\n\r\n",
            srt_time(*start),
            srt_time(end),
            text.join("\r\n")
        ));
    }
    ensure!(!output.is_empty(), "No valid subtitle intervals");
    Ok(output)
}
fn srt_time(t: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        t / 3600000,
        t / 60000 % 60,
        t / 1000 % 60,
        t % 1000
    )
}
fn encode(text: &str, encoding: TextEncoding) -> Result<Vec<u8>> {
    if encoding == TextEncoding::Utf8 {
        return Ok(text.as_bytes().to_vec());
    }
    let (bytes, _, errors) = encoding_rs::GBK.encode(text);
    if errors {
        bail!("Some characters cannot be represented in GBK; retry with UTF-8");
    }
    Ok(bytes.into_owned())
}
