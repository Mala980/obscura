use crate::media_types::{MediaType, MediaSource};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerState {
    Idle,
    Loading,
    Playing,
    Paused,
    Buffering,
    Ended,
    Error(String),
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("Failed to initialize media backend: {0}")]
    InitError(String),
    #[error("Unsupported media type: {0}")]
    UnsupportedType(String),
    #[error("Playback error: {0}")]
    PlaybackError(String),
    #[error("Network error: {0}")]
    NetworkError(String),
}

pub struct MediaPlayer {
    state: Arc<RwLock<PlayerState>>,
    source: Arc<RwLock<Option<MediaSource>>>,
    volume: Arc<RwLock<f64>>,
    current_time: Arc<RwLock<f64>>,
    duration: Arc<RwLock<f64>>,
    loop_playback: Arc<RwLock<bool>>,
    muted: Arc<RwLock<bool>>,
}

impl Clone for MediaPlayer {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            source: self.source.clone(),
            volume: self.volume.clone(),
            current_time: self.current_time.clone(),
            duration: self.duration.clone(),
            loop_playback: self.loop_playback.clone(),
            muted: self.muted.clone(),
        }
    }
}

impl MediaPlayer {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(PlayerState::Idle)),
            source: Arc::new(RwLock::new(None)),
            volume: Arc::new(RwLock::new(1.0)),
            current_time: Arc::new(RwLock::new(0.0)),
            duration: Arc::new(RwLock::new(0.0)),
            loop_playback: Arc::new(RwLock::new(false)),
            muted: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn load(&self, source: MediaSource) -> Result<(), MediaError> {
        *self.source.write().await = Some(source);
        *self.state.write().await = PlayerState::Loading;
        Ok(())
    }

    pub async fn play(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Playing;
        Ok(())
    }

    pub async fn pause(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Paused;
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Idle;
        Ok(())
    }

    pub async fn seek(&self, time: f64) -> Result<(), MediaError> {
        *self.current_time.write().await = time;
        Ok(())
    }

    pub async fn set_volume(&self, volume: f64) -> Result<(), MediaError> {
        *self.volume.write().await = volume.clamp(0.0, 1.0);
        Ok(())
    }

    pub async fn set_muted(&self, muted: bool) -> Result<(), MediaError> {
        *self.muted.write().await = muted;
        Ok(())
    }

    pub async fn set_loop(&self, looping: bool) -> Result<(), MediaError> {
        *self.loop_playback.write().await = looping;
        Ok(())
    }

    pub async fn get_state(&self) -> PlayerState {
        self.state.read().await.clone()
    }

    pub async fn get_current_time(&self) -> f64 {
        *self.current_time.read().await
    }

    pub async fn get_duration(&self) -> f64 {
        *self.duration.read().await
    }

    pub async fn get_volume(&self) -> f64 {
        *self.volume.read().await
    }

    pub async fn is_muted(&self) -> bool {
        *self.muted.read().await
    }

    pub async fn is_looping(&self) -> bool {
        *self.loop_playback.read().await
    }

    pub fn can_play_type(mime: &str) -> &'static str {
        MediaType::can_play_type(mime)
    }
}

impl Default for MediaPlayer {
    fn default() -> Self {
        Self::new()
    }
}
