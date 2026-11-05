# 本地录制模块：订阅节目帧，编码为 H.264/AAC MP4 文件。
use super::program::{ArcBytes, Program};
use super::source::Canvas;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordConfig {
    pub path: PathBuf,
    pub bitrate_kbps: u32,
    pub height_preset: u32,
    pub use_mic: bool,
    pub volume: f64,
    pub mute: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordStats {
    pub is_recording: bool,
    pub path: String,
    pub bitrate_kbps: u32,
    pub fps: f32,
    pub duration_seconds: u64,
    pub frame_count: u64,
    pub dropped_frames: u64,
    pub out_width: u32,
    pub out_height: u32,
    pub mic_active: bool,
    pub error: Option<String>,
}

struct Session {
    pipeline: gst::Pipeline,
    started: Instant,
    path: PathBuf,
    out: (u32, u32),
    bitrate_kbps: u32,
    mic_active: bool,
}

pub struct Recorder {
    session: Mutex<Option<Session>>,
    is_recording: Arc<AtomicBool>,
    frames_pushed: Arc<AtomicU64>,
    frames_dropped: Arc<AtomicU64>,
    last_error: Arc<RwLock<Option<String>>>,
}

fn recording_size(canvas: Canvas, preset: u32) -> (u32, u32) {
    let short = if preset >= 1080 { 1080 } else { 720 };
    let long = short * 16 / 9;
    if canvas.is_portrait() { (short, long) } else { (long, short) }
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            is_recording: Arc::new(AtomicBool::new(false)),
            frames_pushed: Arc::new(AtomicU64::new(0)),
            frames_dropped: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }

    pub fn get_stats(&self) -> RecordStats {
        let running = self.is_recording();
        let guard = self.session.lock();
        let (duration, path, out, bitrate, mic) = match guard.as_ref() {
            Some(session) if running => (
                session.started.elapsed().as_secs(),
                session.path.display().to_string(),
                session.out,
                session.bitrate_kbps,
                session.mic_active,
            ),
            _ => (0, String::new(), (0, 0), 0, false),
        };
        let frames = self.frames_pushed.load(Ordering::Relaxed);
        RecordStats {
            is_recording: running,
            path,
            bitrate_kbps: bitrate,
            fps: if duration > 0 { frames as f32 / duration as f32 } else { 0.0 },
            duration_seconds: duration,
            frame_count: frames,
            dropped_frames: self.frames_dropped.load(Ordering::Relaxed),
            out_width: out.0,
            out_height: out.1,
            mic_active: mic,
            error: self.last_error.read().clone(),
        }
    }

    fn build_desc(
        canvas: Canvas,
        out: (u32, u32),
        path: &Path,
        cfg: &RecordConfig,
        mic: bool,
    ) -> String {
        let audio_src = if mic {
            "wasapi2src low-latency=true ! audioconvert ! audioresample".to_string()
        } else {
            "audiotestsrc wave=silence is-live=true".to_string()
        };
        format!(
            "mp4mux name=mux ! filesink location=\"{path}\" \
             appsrc name=src is-live=true format=time do-timestamp=true max-buffers=4 leaky-type=downstream \
             caps=video/x-raw,format=RGB,width={cw},height={ch},framerate=30/1 ! videoconvert ! videoscale ! \
             video/x-raw,width={ow},height={oh},format=I420 ! \
             x264enc bitrate={br} speed-preset=veryfast tune=zerolatency key-int-max=60 bframes=0 ! \
             video/x-h264,profile=main ! h264parse ! queue ! mux. \
             {audio} ! audio/x-raw,rate=44100,channels=2 ! volume name=vol volume={vol} mute={mute} ! audioconvert ! \
             avenc_aac bitrate=128000 ! aacparse ! queue ! mux.",
            path = path.display(),
            cw = canvas.w,
            ch = canvas.h,
            ow = out.0,
            oh = out.1,
            br = cfg.bitrate_kbps,
            audio = audio_src,
            vol = cfg.volume.clamp(0.0, 2.0),
            mute = cfg.mute,
        )
    }

    fn launch(desc: &str) -> Result<gst::Pipeline, String> {
        gst::parse::launch(desc)
            .map_err(|e| format!("创建录制管道失败: {}", e))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "无法转换为 gst::Pipeline".to_string())
    }

    pub fn start(&self, cfg: RecordConfig, canvas: Canvas, program: &Program) -> Result<(), String> {
        if self.is_recording() {
            return Ok(());
        }
        if cfg.bitrate_kbps == 0 || cfg.bitrate_kbps > 20000 {
            return Err("录制码率需在 1-20000 kbps 之间".to_string());
        }
        let parent = cfg.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| format!("创建录制目录失败: {}", e))?;
        if cfg.path.exists() {
            return Err("录制文件已存在，请更换文件名".to_string());
        }

        let out = recording_size(canvas, cfg.height_preset);
        let mut mic_active = cfg.use_mic;
        let mut pipeline = Self::launch(&Self::build_desc(canvas, out, &cfg.path, &cfg, mic_active));
        if mic_active {
            let failed = match &pipeline {
                Ok(pipeline) => pipeline.set_state(gst::State::Paused).is_err(),
                Err(_) => true,
            };
            if failed {
                if let Ok(pipeline) = &pipeline {
                    let _ = pipeline.set_state(gst::State::Null);
                }
                mic_active = false;
                pipeline = Self::launch(&Self::build_desc(canvas, out, &cfg.path, &cfg, false));
            }
        }
        let pipeline = pipeline?;
        let appsrc = pipeline
            .by_name("src")
            .and_then(|element| element.downcast::<gst_app::AppSrc>().ok())
            .ok_or("未找到录制 appsrc")?;

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("启动录制失败: {:?}", e))?;

        self.frames_pushed.store(0, Ordering::SeqCst);
        self.frames_dropped.store(0, Ordering::SeqCst);
        *self.last_error.write() = None;
        self.is_recording.store(true, Ordering::SeqCst);

        let rx = program.subscribe();
        let recording = Arc::clone(&self.is_recording);
        let pushed = Arc::clone(&self.frames_pushed);
        let dropped = Arc::clone(&self.frames_dropped);
        std::thread::Builder::new()
            .name("record-pump".into())
            .spawn(move || {
                while recording.load(Ordering::Relaxed) {
                    match rx.recv_timeout(std::time::Duration::from_millis(200)) {
                        Ok(frame) => {
                            if appsrc.push_buffer(gst::Buffer::from_slice(ArcBytes(frame))).is_ok() {
                                pushed.fetch_add(1, Ordering::Relaxed);
                            } else {
                                dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
                let _ = appsrc.end_of_stream();
            })
            .map_err(|e| e.to_string())?;
