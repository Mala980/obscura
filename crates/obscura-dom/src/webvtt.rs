//! WebVTT subtitle/track parsing (servo-webvtt parity).
//!
//! Parses WebVTT files into a structured representation that can be
//! used for subtitle rendering and accessibility.

use std::fmt;

/// A single WebVTT cue with timing and text.
#[derive(Debug, Clone)]
pub struct WebVttCue {
    pub id: Option<String>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub settings: Option<String>,
}

/// A parsed WebVTT document.
#[derive(Debug, Clone, Default)]
pub struct WebVttDocument {
    pub cues: Vec<WebVttCue>,
}

#[derive(Debug)]
pub enum WebVttError {
    InvalidTimestamp(String),
    MissingTimestamps,
    InvalidFormat(String),
}

impl fmt::Display for WebVttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebVttError::InvalidTimestamp(s) => write!(f, "invalid timestamp: {s}"),
            WebVttError::MissingTimestamps => write!(f, "missing timestamps in cue"),
            WebVttError::InvalidFormat(s) => write!(f, "invalid WebVTT format: {s}"),
        }
    }
}

impl std::error::Error for WebVttError {}

/// Parse a WebVTT timestamp (HH:MM:SS.mmm or MM:SS.mmm) into milliseconds.
pub fn parse_timestamp(ts: &str) -> Result<u64, WebVttError> {
    let ts = ts.trim();
    let parts: Vec<&str> = ts.split(':').collect();
    match parts.len() {
        3 => {
            let hours: u64 = parts[0].parse().map_err(|_| WebVttError::InvalidTimestamp(ts.to_string()))?;
            let mins: u64 = parts[1].parse().map_err(|_| WebVttError::InvalidTimestamp(ts.to_string()))?;
            let sec_ms: Vec<&str> = parts[2].split('.').collect();
            let secs: u64 = sec_ms[0].parse().map_err(|_| WebVttError::InvalidTimestamp(ts.to_string()))?;
            let ms: u64 = if sec_ms.len() > 1 {
                let ms_str = sec_ms[1];
                let padded = format!("{:0<3}", ms_str);
                padded[..3].parse().unwrap_or(0)
            } else {
                0
            };
            Ok(hours * 3600000 + mins * 60000 + secs * 1000 + ms)
        }
        2 => {
            let mins: u64 = parts[0].parse().map_err(|_| WebVttError::InvalidTimestamp(ts.to_string()))?;
            let sec_ms: Vec<&str> = parts[1].split('.').collect();
            let secs: u64 = sec_ms[0].parse().map_err(|_| WebVttError::InvalidTimestamp(ts.to_string()))?;
            let ms: u64 = if sec_ms.len() > 1 {
                let ms_str = sec_ms[1];
                let padded = format!("{:0<3}", ms_str);
                padded[..3].parse().unwrap_or(0)
            } else {
                0
            };
            Ok(mins * 60000 + secs * 1000 + ms)
        }
        _ => Err(WebVttError::InvalidTimestamp(ts.to_string())),
    }
}

impl WebVttDocument {
    /// Parse a WebVTT string into a document.
    pub fn parse(input: &str) -> Result<Self, WebVttError> {
        let mut doc = WebVttDocument::default();
        let mut lines = input.lines().peekable();

        // Skip the optional BOM
        if let Some(first) = lines.peek() {
            if first.starts_with('\u{feff}') {
                lines.next();
            }
        }

        // Skip the WEBVTT header line
        if let Some(first) = lines.next() {
            if !first.trim_start().starts_with("WEBVTT") {
                return Err(WebVttError::InvalidFormat(
                    "missing WEBVTT header".to_string(),
                ));
            }
        }

        // Skip header metadata (lines before first blank line)
        for line in &mut lines {
            if line.trim().is_empty() {
                break;
            }
        }

        // Parse cues
        let mut current_id: Option<String> = None;
        let mut current_settings: Option<String> = None;
        let mut timestamp_lines: Vec<String> = Vec::new();
        let mut text_lines: Vec<String> = Vec::new();
        let mut in_cue = false;

        for line in lines {
            let line = line.trim_end();

            if line.trim().is_empty() {
                // End of current cue
                if in_cue && !timestamp_lines.is_empty() {
                    if let Some(cue) = parse_cue(
                        current_id.take(),
                        current_settings.take(),
                        &timestamp_lines,
                        &text_lines,
                    )? {
                        doc.cues.push(cue);
                    }
                }
                timestamp_lines.clear();
                text_lines.clear();
                current_id = None;
                current_settings = None;
                in_cue = false;
                continue;
            }

            if !in_cue {
                // Could be a cue id or a timestamp line
                if line.contains("-->") {
                    timestamp_lines.push(line.to_string());
                    in_cue = true;
                } else {
                    // This is a cue id
                    current_id = Some(line.to_string());
                }
            } else {
                if line.contains("-->") {
                    timestamp_lines.push(line.to_string());
                } else if text_lines.is_empty() && line.starts_with("NOTE") {
                    // Skip NOTE blocks
                    continue;
                } else {
                    // Check if this line has settings (position, line, align)
                    if line.contains(':') && !line.contains("-->") {
                        // Could be cue settings
                        let parts: Vec<&str> = line.splitn(2, ':').collect();
                        if parts.len() == 2
                            && (parts[0].trim() == "position"
                                || parts[0].trim() == "line"
                                || parts[0].trim() == "align"
                                || parts[0].trim() == "size")
                        {
                            current_settings = Some(line.to_string());
                        } else {
                            text_lines.push(line.to_string());
                        }
                    } else {
                        text_lines.push(line.to_string());
                    }
                }
            }
        }

        // Handle last cue
        if in_cue && !timestamp_lines.is_empty() {
            if let Some(cue) = parse_cue(
                current_id.take(),
                current_settings.take(),
                &timestamp_lines,
                &text_lines,
            )? {
                doc.cues.push(cue);
            }
        }

        Ok(doc)
    }

    /// Get all cues that are active at a given time in milliseconds.
    pub fn cues_at_time(&self, time_ms: u64) -> Vec<&WebVttCue> {
        self.cues
            .iter()
            .filter(|c| time_ms >= c.start_ms && time_ms <= c.end_ms)
            .collect()
    }
}

fn parse_cue(
    id: Option<String>,
    settings: Option<String>,
    timestamp_lines: &[String],
    text_lines: &[String],
) -> Result<Option<WebVttCue>, WebVttError> {
    let ts_line = timestamp_lines.first().ok_or(WebVttError::MissingTimestamps)?;
    let parts: Vec<&str> = ts_line.split("-->").collect();
    if parts.len() < 2 {
        return Err(WebVttError::MissingTimestamps);
    }

    let start_ms = parse_timestamp(parts[0])?;
    // The end timestamp may have settings after it
    let end_part = parts[1].split_whitespace().next().unwrap_or("");
    let end_ms = parse_timestamp(end_part)?;

    let text = text_lines.join("\n");

    Ok(Some(WebVttCue {
        id,
        start_ms,
        end_ms,
        text,
        settings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_webvtt() {
        let input = r#"WEBVTT

00:00:01.000 --> 00:00:04.000
Hello, world!

00:00:05.000 --> 00:00:08.000
Goodbye, world!
"#;
        let doc = WebVttDocument::parse(input).unwrap();
        assert_eq!(doc.cues.len(), 2);
        assert_eq!(doc.cues[0].text, "Hello, world!");
        assert_eq!(doc.cues[0].start_ms, 1000);
        assert_eq!(doc.cues[0].end_ms, 4000);
        assert_eq!(doc.cues[1].text, "Goodbye, world!");
    }

    #[test]
    fn parse_webvtt_with_id() {
        let input = r#"WEBVTT

cue1
00:00:01.000 --> 00:00:04.000
First cue

cue2
00:00:05.000 --> 00:00:08.000
Second cue
"#;
        let doc = WebVttDocument::parse(input).unwrap();
        assert_eq!(doc.cues.len(), 2);
        assert_eq!(doc.cues[0].id.as_deref(), Some("cue1"));
        assert_eq!(doc.cues[1].id.as_deref(), Some("cue2"));
    }

    #[test]
    fn cues_at_time_works() {
        let input = r#"WEBVTT

00:00:01.000 --> 00:00:04.000
First

00:00:03.000 --> 00:00:06.000
Second
"#;
        let doc = WebVttDocument::parse(input).unwrap();
        let at_2s = doc.cues_at_time(2000);
        assert_eq!(at_2s.len(), 1);
        assert_eq!(at_2s[0].text, "First");

        let at_3_5s = doc.cues_at_time(3500);
        assert_eq!(at_3_5s.len(), 2);
    }

    #[test]
    fn parse_timestamps() {
        assert_eq!(parse_timestamp("01:30:00.000").unwrap(), 5400000);
        assert_eq!(parse_timestamp("00:05:30.500").unwrap(), 330500);
        assert_eq!(parse_timestamp("02:15.000").unwrap(), 135000);
    }
}
