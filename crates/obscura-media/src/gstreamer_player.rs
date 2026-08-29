use crate::player::{PlayerState, MediaError};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::sync::Mutex;
use std::collections::HashMap;

#[cfg(feature = "gstreamer")]
use gstreamer as gst;
#[cfg(feature = "gstreamer")]
use gstreamer::prelude::*;

/// GStreamer-based media player (servo-media parity).
#[cfg(feature = "gstreamer")]
pub struct GStreamerPlayer {
    pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    state: Arc<RwLock<PlayerState>>,
    volume: Arc<RwLock<f64>>,
    muted: Arc<RwLock<bool>>,
    loop_playback: Arc<RwLock<bool>>,
}

#[cfg(feature = "gstreamer")]
impl Clone for GStreamerPlayer {
    fn clone(&self) -> Self {
        Self {
            pipeline: self.pipeline.clone(),
            state: self.state.clone(),
            volume: self.volume.clone(),
            muted: self.muted.clone(),
            loop_playback: self.loop_playback.clone(),
        }
    }
}

#[cfg(feature = "gstreamer")]
impl GStreamerPlayer {
    pub fn new() -> Result<Self, MediaError> {
        gst::init().map_err(|e| MediaError::InitError(format!("GStreamer init failed: {e}")))?;
        Ok(Self {
            pipeline: Arc::new(Mutex::new(None)),
            state: Arc::new(RwLock::new(PlayerState::Idle)),
            volume: Arc::new(RwLock::new(1.0)),
            muted: Arc::new(RwLock::new(false)),
            loop_playback: Arc::new(RwLock::new(false)),
        })
    }

    pub async fn load(&self, source: crate::media_types::MediaSource) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Loading;

        let uri = match &source {
            crate::media_types::MediaSource::Url(url) => url.clone(),
            _ => return Err(MediaError::UnsupportedType("Only URL sources supported".to_string())),
        };

        let uridecodebin = gst::ElementFactory::make("uridecodebin")
            .name("source")
            .property("uri", uri.as_str())
            .build()
            .map_err(|e| MediaError::InitError(format!("uridecodebin: {e}")))?;

        let audio_sink = gst::ElementFactory::make("autoaudiosink")
            .name("audio-sink")
            .build()
            .map_err(|e| MediaError::InitError(format!("audio-sink: {e}")))?;

        let video_sink = gst::ElementFactory::make("fakesink")
            .name("video-sink")
            .build()
            .map_err(|e| MediaError::InitError(format!("video-sink: {e}")))?;

        let pipeline = gst::Pipeline::new(Some("media-pipeline"));
        pipeline.add_many([&uridecodebin, &audio_sink, &video_sink])
            .map_err(|e| MediaError::InitError(format!("add elements: {e}")))?;

        let audio_clone = audio_sink.clone();
        let video_clone = video_sink.clone();
        uridecodebin.connect_pad_added(move |_, src_pad| {
            if let Some(caps) = src_pad.current_caps() {
                if let Some(structure) = caps.structure(0) {
                    let name = structure.name().as_str();
                    if name.starts_with("audio/") {
                        let sink_pad = audio_clone.static_pad("sink").unwrap();
                        if !sink_pad.is_linked() {
                            let _ = src_pad.link(&sink_pad);
                        }
                    } else if name.starts_with("video/") {
                        let sink_pad = video_clone.static_pad("sink").unwrap();
                        if !sink_pad.is_linked() {
                            let _ = src_pad.link(&sink_pad);
                        }
                    }
                }
            }
        });

        pipeline.set_state(gst::State::Playing)
            .map_err(|e| MediaError::PlaybackError(format!("start: {e}")))?;

        *self.pipeline.lock().unwrap() = Some(pipeline);
        Ok(())
    }

    pub async fn play(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Playing;
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            let _ = p.set_state(gst::State::Playing);
        }
        Ok(())
    }

    pub async fn pause(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Paused;
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            let _ = p.set_state(gst::State::Paused);
        }
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Idle;
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            let _ = p.set_state(gst::State::Null);
        }
        Ok(())
    }

    pub async fn seek(&self, _time: f64) -> Result<(), MediaError> {
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

    pub async fn get_current_time(&self) -> f64 { 0.0 }
    pub async fn get_duration(&self) -> f64 { 0.0 }
    pub async fn get_volume(&self) -> f64 { *self.volume.read().await }
    pub async fn is_muted(&self) -> bool { *self.muted.read().await }
    pub async fn is_looping(&self) -> bool { *self.loop_playback.read().await }
    pub fn can_play_type(mime: &str) -> &'static str { crate::media_types::MediaType::can_play_type(mime) }
}
