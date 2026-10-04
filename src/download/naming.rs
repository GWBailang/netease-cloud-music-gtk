use ncm_api::SongInfo;

pub fn sanitize_component(input: &str) -> String {
    let replaced: String = input
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = replaced.trim_matches([' ', '.']).to_owned();
    if name.is_empty() {
        return "Untitled".into();
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        name.insert(0, '_');
    }
    while name.len() > 180 {
        name.pop();
    }
    name.trim_end_matches([' ', '.']).to_owned()
}
pub fn song_basename(song: &SongInfo, ordinal: Option<(usize, usize)>) -> String {
    let prefix = ordinal
        .map(|(n, total)| format!("{n:0width$} ", width = total.to_string().len().max(3)))
        .unwrap_or_default();
    sanitize_component(&format!("{prefix}{} - {}", song.singer, song.name))
}
