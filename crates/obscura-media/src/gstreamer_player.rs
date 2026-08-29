#[cfg(feature = "gstreamer")]
use crate::media_types::{MediaSource, MediaType};
use crate::player::{PlayerState, MediaError};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, oneshot};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::Duration;
use std::sync::Arc;
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::Duration;
use std::pin::Pin;
use std::future::Future;

#[cfg(feature = "gstreamer")]
use gstreamer as gst;
#[cfg(feature = "gstreamer")]
use gstreamer::prelude::*;
#[cfg(feature = "gstreamer")]
use gstreamer::{Element, Pipeline, State, StateChangeReturn, ClockTime, SeekFlags, SeekType, Format};
#[cfg(feature = "gstreamer")]
use gstreamer_app::{AppSink, AppSinkCallbacks};
#[cfg(feature = "gstreamer")]
use gstreamer_video::VideoSink;
#[cfg(feature = "gstreamer")]
use gstreamer_audio::AudioSink;
use std::sync::Arc;
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::Duration;
use std::pin::Pin;
use std::future::Future;

/// GStreamer-based media player implementation for real video/audio playback
#[cfg(feature = "gstreamer")]
pub struct GStreamerPlayer {
    pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    state: Arc<tokio::sync::RwLock<crate::player::PlayerState>>,
    source: std::sync::RwLock<Option<crate::media_types::MediaSource>>,
    volume: tokio::sync::RwLock<f64>,
    current_time: Arc<std::sync::atomic::AtomicU64>, // nanoseconds
    duration: Arc<Mutex<Option<gst::ClockTime>>>,
    loop_playback: Arc<tokio::sync::RwLock<bool>>,
    muted: Arc<tokio::sync::RwLock<bool>>,
    volume: Arc<tokio::sync::RwLock<f64>>,
    pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    bus: Arc<Mutex<Option<gst::Bus>>>,
    bus_handler: Option<std::thread::JoinHandle<()>>,
    seek_sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    seek_receiver: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    audio_sink: Arc<Mutex<Option<gst::Element>>>,
    video_sink: Arc<Mutex<Option<gst::Element>>>,
    video_sink_appsink: Arc<Mutex<Option<gstreamer_app::AppSink>>>,
    frame_callback: Arc<Mutex<Option<Box<dyn Fn(&[u8], u32, u32) + Send + Sync>>>>,
}

impl Clone for GStreamerPlayer {
    fn clone(&self) -> Self {
        Self {
            pipeline: self.pipeline.clone(),
            state: self.state.clone(),
            source: self.source.clone(),
            volume: self.volume.clone(),
            current_time: self.current_time.clone(),
            duration: self.duration.clone(),
            loop_playback: self.loop_playback.clone(),
            muted: self.muted.clone(),
            volume: self.volume.clone(),
            pipeline: self.pipeline.clone(),
            bus: self.bus.clone(),
            bus_handler: None,
            seek_sender: self.seek_sender.clone(),
            seek_receiver: self.seek_receiver.clone(),
            audio_sink: self.audio_sink.clone(),
            video_sink: self.video_sink.clone(),
            video_sink_appsink: self.video_sink_appsink.clone(),
            frame_callback: self.frame_callback.clone(),
        }
    }
}

impl GStreamerPlayer {
    pub fn new() -> Result<Self, crate::player::MediaError> {
        // Initialize GStreamer
        gst::init().map_err(|e| crate::player::MediaError::InitError(format!("GStreamer init failed: {}", e)))?;

        Ok(Self {
            pipeline: Arc::new(Mutex::new(None)),
            state: Arc::new(tokio::sync::RwLock::new(crate::player::PlayerState::Idle)),
            source: std::sync::RwLock::new(None),
            volume: tokio::sync::RwLock::new(1.0),
            current_time: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            duration: Arc::new(Mutex::new(None)),
            loop_playback: Arc::new(tokio::sync::RwLock::new(false)),
            muted: Arc::new(tokio::sync::RwLock::new(false)),
            volume: Arc::new(tokio::sync::RwLock::new(1.0)),
            pipeline: Arc::new(Mutex::new(None)),
            bus: Arc::new(Mutex::new(None)),
            bus_handler: None,
            seek_sender: Arc::new(Mutex::new(None)),
            seek_receiver: Arc::new(Mutex::new(None)),
            audio_sink: Arc::new(Mutex::new(None)),
            video_sink: Arc::new(Mutex::new(None)),
            video_sink_appsink: Arc::new(Mutex::new(None)),
            frame_callback: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a GStreamer pipeline for the given media source
    async fn create_pipeline(&self, source: &crate::media_types::MediaSource) -> Result<gst::Pipeline, crate::player::MediaError> {
        let pipeline = gst::Pipeline::new(Some("media-pipeline"));

        let uri = match source {
            crate::media_types::MediaSource::Url(url) => url.clone(),
            crate::media_types::MediaSource::Blob(data) => {
                return Err(crate::player::MediaError::UnsupportedType("Blob data not yet supported".to_string()));
            }
            crate::media_types::MediaSource::File(path) => {
                format!("file://{}", path)
            }
        };

        // Create uridecodebin for automatic format detection
        let uridecodebin = gst::ElementFactory::make("uridecodebin")
            .name("source")
            .property("uri", &uri)
            .build()
            .map_err(|e| crate::player::MediaError::InitError(format!("Failed to create uridecodebin: {}", e)))?;

        // Audio sink
        let audio_sink = gst::ElementFactory::make("autoaudiosink")
            .name("audio-sink")
            .build()
            .map_err(|e| crate::player::MediaError::InitError(format!("Failed to create audio sink: {}", e)))?;

        // Video sink with appsink for frame extraction
        let video_sink = gst::ElementFactory::make("appsink")
            .name("video-sink")
            .property("emit-signals", true)
            .property("max-buffers", 1u32)
            .property("drop", true)
            .property("sync", false)
            .build()
            .map_err(|e| crate::player::MediaError::InitError(format!("Failed to create video sink: {}", e)))?;

        let appsink = video_sink.clone().dynamic_cast::<gstreamer_app::AppSink>()
            .map_err(|_| crate::player::MediaError::InitError("Failed to cast to AppSink".into()))?;

        let appsink_callbacks = gstreamer_app::AppSinkCallbacks::new()
            .new_sample(move |appsink| {
                if let Some(sample) = appsink.pull_sample() {
                    if let Some(buffer) = sample.buffer() {
                        if let Some(info) = buffer.video_info() {
                            let width = info.width();
                            let height = info.height();
                            if let Ok(map) = buffer.map_readable() {
                                let data = map.as_slice().to_vec();
                                // TODO: Send frame to callback
                            }
                        }
                    }
                    gst::FlowSuccess::Ok
                })
                .build();

        appsink.set_callbacks(appsink_callbacks);

        // Build pipeline
        let pipeline = gst::Pipeline::new(Some("media-pipeline"));

        // Add elements to pipeline
        pipeline.add_many([
            &uridecodebin,
            &audio_sink,
            &video_sink,
        ]).map_err(|e| crate::player::MediaError::InitError(format!("Failed to add elements to pipeline: {}", e)))?;

        // Link uridecodebin to sinks dynamically
        let audio_sink_clone = audio_sink.clone();
        let video_sink_clone = video_sink.clone();
        uridecodebin.connect_pad_added(move |_, src_pad| {
            let caps = src_pad.current_caps();
            if let Some(caps) = caps {
                let structure = caps.structure(0).unwrap();
                let name = structure.name().as_str();
                if name.starts_with("audio/") {
                    let sink_pad = audio_sink.static_pad("sink").unwrap();
                    if !sink_pad.is_linked() {
                        let _ = src_pad.link(&sink_pad);
                    }
                } else if name.starts_with("video/") {
                    let sink_pad = video_sink.static_pad("sink").unwrap();
                    if !sink_pad.is_linked() {
                        let _ = src_pad.link(&sink_pad);
                    }
                }
            });

            pipeline.add_many([
                &uridecodebin,
                &audio_sink,
                &video_sink,
            ]).map_err(|e| crate::player::MediaError::InitError(format!("Failed to add elements to pipeline: {}", e)))?;

            Ok(pipeline)
        }

    /// Initialize the player with a media source
    pub async fn load(&mut self, source: crate::media_types::MediaSource) -> Result<(), crate::player::MediaError> {
        *self.source.write().await = Some(source.clone());

        let pipeline = self.create_pipeline(&source).await?;
        *self.pipeline.lock().await = Some(pipeline.clone());

        // Get bus for message handling
        let bus = pipeline.bus().expect("Pipeline should have a bus");
        *self.bus.lock().await = Some(bus.clone());

        // Start bus watch
        let state = self.state.clone();
        let pipeline_clone = pipeline.clone();
        let pipeline_clone2 = pipeline.clone();
        let state = self.state.clone();
        
        let bus_stream = bus.stream();
        let state_clone = state.clone();
        
        tokio::spawn(async move {
            let mut stream = bus_stream;
            while let Some(msg) = stream.next().await {
                use gst::MessageView;
                match msg.view() {
                    gst::MessageView::Eos(..) => {
                        *state.write().await = crate::player::PlayerState::Ended;
                    }
                    gst::MessageView::Error(err) => {
                        *state.write().await = crate::player::PlayerState::Error(err.to_string());
                    }
                    gst::MessageView::StateChanged(changed) => {
                        if changed.src().map(|o| o == pipeline_clone2).unwrap_or(false) {
                            let (_, new, _) = changed.old_new_pending();
                            let new_state = match new {
                                gst::State::Playing => crate::player::PlayerState::Playing,
                                gst::State::Paused => crate::player::PlayerState::Paused,
                                gst::State::Null => crate::player::PlayerState::Idle,
                                gst::State::Ready => crate::player::PlayerState::Loading,
                                _ => crate::player::PlayerState::Buffering,
                            };
                            *state_clone.write().await = new_state;
                        }
                    }
                    gst::MessageView::DurationChanged(..) => {
                        // Duration changed
                    }
                    _ => {}
                }
            });

        // Start pipeline
        pipeline.set_state(gst::State::Playing)
            .map_err(|e| crate::player::MediaError::PlaybackError(format!("Failed to start pipeline: {}", e)))?;

        *self.pipeline.lock().await = Some(pipeline);

        Ok(())
    }

    /// Play the media
    pub async fn play(&mut self) -> Result<(), crate::player::MediaError> {
        *self.state.write().await = crate::player::PlayerState::Playing;
        
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            pipeline.set_state(gst::State::Playing)
                .map_err(|e| crate::player::MediaError::PlaybackError(format!("Failed to play: {}", e)))?;
        }
        Ok(())
    }

    /// Pause playback
    pub async fn pause(&mut self) -> Result<(), crate::player::MediaError> {
        *self.state.write().await = crate::player::PlayerState::Paused;
        
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            pipeline.set_state(gst::State::Paused)
                .map_err(|e| crate::player::MediaError::PlaybackError(format!("Failed to pause: {}", e)))?;
        }
        Ok(())
    }

    /// Stop playback
    pub async fn stop(&mut self) -> Result<(), crate::player::MediaError> {
        *self.state.write().await = crate::player::PlayerState::Idle;
        
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            pipeline.set_state(gst::State::Null)
                .map_err(|e| crate::player::MediaError::PlaybackError(format!("Failed to stop: {}", e)))?;
        }
        Ok(())
    }

    /// Seek to a specific time
    pub async fn seek(&mut self, time: f64) -> Result<(), crate::player::MediaError> {
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            let time_ns = (time * 1_000_000_000.0) as u64;
            pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::Format::Time,
                gst::ClockTime::from_nseconds(time_ns),
            ).map_err(|e| crate::player::MediaError::PlaybackError(format!("Seek failed: {}", e)))?;
            
            self.current_time.store((time * 1_000_000_000.0) as u64, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// Set volume (0.0 to 1.0)
    pub async fn set_volume(&self, volume: f64) -> Result<(), crate::player::MediaError> {
        let volume = volume.clamp(0.0, 1.0);
        *self.volume.write().await = volume;
        
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            if let Some(audio_sink) = self.audio_sink.lock().unwrap().as_ref() {
                audio_sink.set_property("volume", volume);
            }
        }
        Ok(())
    }

    /// Set mute state
    pub async fn set_muted(&self, muted: bool) -> Result<(), crate::player::MediaError> {
        *self.muted.write().await = muted;
        
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            if let Some(audio_sink) = self.audio_sink.lock().unwrap().as_ref() {
                audio_sink.set_property("mute", muted);
            }
        }
        Ok(())
    }

    /// Set loop playback
    pub async fn set_loop(&self, looping: bool) -> Result<(), crate::player::MediaError> {
        *self.loop_playback.write().await = looping;
        Ok(())
    }

    /// Get current playback state
    pub async fn get_state(&self) -> crate::player::PlayerState {
        self.state.read().await.clone()
    }

    /// Get current playback time in seconds
    pub async fn get_current_time(&self) -> f64 {
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            if let Ok((_, position)) = pipeline.query_position(gst::Format::Time) {
                return position.nseconds() as f64 / 1_000_000_000.0;
            }
        }
        self.current_time.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1_000_000_000.0
    }

    /// Get total duration in seconds
    pub async fn get_duration(&self) -> f64 {
        if let Some(duration) = *self.duration.lock().unwrap() {
            duration.nseconds() as f64 / 1_000_000_000.0
        } else if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            if let Ok((_, duration)) = pipeline.query_duration(gst::Format::Time) {
                let dur = duration.nseconds() as f64 / 1_000_000_000.0;
                *self.duration.lock().unwrap() = Some(duration);
                return dur;
            }
        }
        0.0
    }

    /// Set a callback for video frames
    pub fn set_frame_callback<F>(&self, callback: F)
    where
        F: Fn(&[u8], u32, u32) + Send + Sync + 'static,
    {
        *self.frame_callback.lock().unwrap() = Some(Box::new(callback));
    }

    /// Get current volume
    pub async fn get_volume(&self) -> f64 {
        *self.volume.read().await
    }

    /// Check if muted
    pub async fn is_muted(&self) -> bool {
        *self.muted.read().await
    }

    /// Check if looping
    pub async fn is_looping(&self) -> bool {
        *self.loop_playback.read().await
    }

    /// Check if can play a mime type
    pub fn can_play_type(mime: &str) -> &'static str {
        crate::media_types::MediaType::can_play_type(mime)
    }

    /// Get video dimensions
    pub async fn get_video_size(&self) -> Option<(u32, u32)> {
        if let Some(pipeline) = self.pipeline.lock().await.as_ref() {
            if let Some(video_sink) = self.video_sink.lock().unwrap().as_ref() {
                if let Some(pad) = video_sink.static_pad("sink") {
                    if let Some(caps) = pad.current_caps() {
                        if let Some(structure) = caps.structure(0) {
                            if let (Ok(width), Ok(height)) = (structure.get("width"), structure.get("height")) {
                                return Some((width, height));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Clean up resources
    pub async fn shutdown(&mut self) {
        let _ = self.stop().await;
        
        // Clean up bus handler
        if let Some(handle) = self.bus_handler.take() {
            let _ = handle.join();
        }

        // Clear pipeline
        if let Some(pipeline) = self.pipeline.lock().await.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}

impl Drop for GStreamerPlayer {
    fn drop(&mut self) {
        // Cleanup handled in shutdown
    }
}

// Provide a default implementation for when gstreamer feature is not enabled
#[cfg(not(feature = "gstreamer"))]
pub struct GStreamerPlayer;

#[cfg(not(feature = "gstreamer"))]
impl GStreamerPlayer {
    pub fn new() -> Result<Self, crate::player::MediaError> {
        Err(crate::player::MediaError::InitError("GStreamer feature not enabled".to_string()))
    }
}