use crate::player::{PlayerState, MediaError};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;

#[cfg(feature = "gstreamer")]
use gstreamer as gst;
#[cfg(feature = "gstreamer")]
use gstreamer::prelude::*;
#[cfg(feature = "gstreamer")]
use gstreamer::{Element, Pipeline, State, ClockTime, SeekFlags, Format};

#[cfg(feature = "gstreamer")]
pub struct GStreamerPlayer {
    pipeline: Arc<Mutex<Option<Pipeline>>>,
    state: Arc<RwLock<PlayerState>>,
    source: std::sync::RwLock<Option<crate::media_types::MediaSource>>,
    volume: RwLock<f64>,
    current_time: Arc<AtomicU64>,
    loop_playback: RwLock<bool>,
    muted: RwLock<bool>,
}

#[cfg(feature = "gstreamer")]
impl Clone for GStreamerPlayer {
    fn clone(&self) -> Self {
        Self {
            pipeline: self.pipeline.clone(),
            state: self.state.clone(),
            source: self.source.clone(),
            volume: self.volume.clone(),
            current_time: self.current_time.clone(),
            loop_playback: self.loop_playback.clone(),
            muted: self.muted.clone(),
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
            source: std::sync::RwLock::new(None),
            volume: RwLock::new(1.0),
            current_time: Arc::new(AtomicU64::new(0)),
            loop_playback: RwLock::new(false),
            muted: RwLock::new(false),
        })
    }

    pub async fn load(&self, source: crate::media_types::MediaSource) -> Result<(), MediaError> {
        *self.source.write().await = Some(source.clone());
        *self.state.write().await = PlayerState::Loading;

        let pipeline = match &source {
            crate::media_types::MediaSource::Url(url) => {
                let uridecodebin = gst::ElementFactory::make("uridecodebin")
                    .name("source")
                    .property("uri", url.as_str())
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

                Ok(pipeline)
            }
            _ => Err(MediaError::UnsupportedType("Only URL sources supported".to_string())),
        }?;

        pipeline.set_state(State::Playing)
            .map_err(|e| MediaError::PlaybackError(format!("start: {e}")))?;

        *self.pipeline.lock().unwrap() = Some(pipeline);
        Ok(())
    }

    pub async fn play(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Playing;
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            p.set_state(State::Playing)
                .map_err(|e| MediaError::PlaybackError(format!("{e}")))?;
        }
        Ok(())
    }

    pub async fn pause(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Paused;
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            p.set_state(State::Paused)
                .map_err(|e| MediaError::PlaybackError(format!("{e}")))?;
        }
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), MediaError> {
        *self.state.write().await = PlayerState::Idle;
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            p.set_state(State::Null)
                .map_err(|e| MediaError::PlaybackError(format!("{e}")))?;
        }
        Ok(())
    }

    pub async fn seek(&self, time: f64) -> Result<(), MediaError> {
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            let time_ns = (time * 1_000_000_000.0) as u64;
            p.seek_simple(
                SeekFlags::FLUSH | SeekFlags::KEY_UNIT,
                Format::Time,
                ClockTime::from_nseconds(time_ns),
            ).map_err(|e| MediaError::PlaybackError(format!("{e}")))?;
            self.current_time.store(time_ns, Ordering::Relaxed);
        }
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
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            if let Ok((_, pos)) = p.query_position(Format::Time) {
                return pos.nseconds() as f64 / 1_000_000_000.0;
            }
        }
        self.current_time.load(Ordering::Relaxed) as f64 / 1_000_000_000.0
    }

    pub async fn get_duration(&self) -> f64 {
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            if let Ok((_, dur)) = p.query_duration(Format::Time) {
                return dur.nseconds() as f64 / 1_000_000_000.0;
            }
        }
        0.0
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
        crate::media_types::MediaType::can_play_type(mime)
    }

    pub fn shutdown(&self) {
        if let Some(ref p) = *self.pipeline.lock().unwrap() {
            let _ = p.set_state(State::Null);
        }
    }
}

#[cfg(not(feature = "gstreamer"))]
pub struct GStreamerPlayer;

#[cfg(not(feature = "gstreamer"))]
impl GStreamerPlayer {
    pub fn new() -> Result<Self, MediaError> {
        Err(MediaError::InitError("GStreamer feature not enabled".to_string()))
    }
}
