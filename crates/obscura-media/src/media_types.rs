use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum MediaType {
    Video,
    Audio,
    Application,
    Unknown,
}

impl MediaType {
    pub fn from_mime(mime: &str) -> Self {
        if mime.starts_with("video/") { MediaType::Video }
        else if mime.starts_with("audio/") { MediaType::Audio }
        else if mime.starts_with("application/") { MediaType::Application }
        else { MediaType::Unknown }
    }

    pub fn can_play_type(mime: &str) -> &'static str {
        match mime {
            "video/mp4" | "video/webm" | "video/ogg" | "video/quicktime" => "maybe",
            "audio/mpeg" | "audio/ogg" | "audio/wav" | "audio/webm" | "audio/aac" => "maybe",
            _ => "",
        }
    }
}

#[derive(Debug, Clone)]
pub enum MediaSource {
    Url(String),
    Blob(String),
    File(PathBuf),
    DataUri(String),
}
