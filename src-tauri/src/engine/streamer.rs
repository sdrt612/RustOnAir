// 推流模块：订阅节目帧 -> H.264(+AAC) -> FLV -> RTMP（或写入本地 FLV 文件用于验证）
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use super::program::{ArcBytes, Program};
use super::source::Canvas;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    pub rtmp_url: String,
    pub stream_key: String,
    pub bitrate_kbps: u32,
    pub fps: u32,
    /// 输出分辨率的短边：720 或 1080（方向由画布决定）
    pub height_preset: u32,
    /// 是否采集麦克风；否则推送静音音轨（多数平台要求必须有音轨）
    pub use_mic: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            rtmp_url: "rtmp://127.0.0.1:1935/live".to_string(),
            stream_key: "stream".to_string(),
            bitrate_kbps: 2500,
            fps: 30,
            height_preset: 720,
            use_mic: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStats {
    pub is_streaming: bool,
    pub target: String,
    pub bitrate_kbps: u32,
    pub fps: f32,
    pub uptime_seconds: u64,
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
    target: String,
    out: (u32, u32),
    cfg: StreamConfig,
    mic_active: bool,
}

pub struct Streamer {
    session: Mutex<Option<Session>>,
    is_streaming: Arc<AtomicBool>,
    frames_pushed: Arc<AtomicU64>,
    frames_dropped: Arc<AtomicU64>,
    last_error: Arc<RwLock<Option<String>>>,
    volume: RwLock<(f64, bool)>,
}

/// 输出分辨率：按画布方向取 720p / 1080p
pub fn output_size(canvas: Canvas, preset: u32) -> (u32, u32) {
    let short = if preset >= 1080 { 1080 } else { 720 };
    let long = short * 16 / 9;
    if canvas.is_portrait() {
        (short, long)
    } else {
        (long, short)
    }
}

impl Streamer {
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            is_streaming: Arc::new(AtomicBool::new(false)),
            frames_pushed: Arc::new(AtomicU64::new(0)),
            frames_dropped: Arc::new(AtomicU64::new(0)),
            last_error: Arc::new(RwLock::new(None)),
            volume: RwLock::new((1.0, false)),
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.is_streaming.load(Ordering::SeqCst)
    }

    pub fn get_stats(&self) -> StreamStats {
        let running = self.is_streaming();
        let guard = self.session.lock();
        let (uptime, target, out, bitrate, mic) = match guard.as_ref() {
            Some(s) if running => (
                s.started.elapsed().as_secs(),
                s.target.clone(),
                s.out,
                s.cfg.bitrate_kbps,
                s.mic_active,
            ),
            _ => (0, String::new(), (0, 0), 0, false),
        };
        let frames = self.frames_pushed.load(Ordering::Relaxed);
        StreamStats {
            is_streaming: running,
            target,
            bitrate_kbps: bitrate,
            fps: if uptime > 0 { frames as f32 / uptime as f32 } else { 0.0 },
            uptime_seconds: uptime,
            frame_count: frames,
            dropped_frames: self.frames_dropped.load(Ordering::Relaxed),
            out_width: out.0,
            out_height: out.1,
            mic_active: mic,
            error: self.last_error.read().clone(),
        }
    }

    /// 调整推流音量/静音（推流中实时生效）
    pub fn set_audio(&self, volume: f64, mute: bool) {
        *self.volume.write() = (volume.clamp(0.0, 2.0), mute);
        if let Some(s) = self.session.lock().as_ref() {
            if let Some(v) = s.pipeline.by_name("vol") {
                v.set_property("volume", volume.clamp(0.0, 2.0));
                v.set_property("mute", mute);
            }
        }
    }

    fn build_desc(cfg: &StreamConfig, canvas: Canvas, out: (u32, u32), full_url: &str, mic: bool) -> String {
        let sink = if let Some(path) = full_url.strip_prefix("file:") {
            format!("filesink location=\"{}\"", path.trim_start_matches("//"))
        } else {
            format!("rtmp2sink location=\"{}\"", full_url)
        };
        let audio_src = if mic {
            "wasapi2src low-latency=true ! audioconvert ! audioresample".to_string()
        } else {
            "audiotestsrc wave=silence is-live=true".to_string()
        };
        format!(
            "flvmux name=mux streamable=true ! {sink} \
             appsrc name=src is-live=true format=time do-timestamp=true max-buffers=4 leaky-type=downstream \
             caps=video/x-raw,format=RGB,width={cw},height={ch},framerate={fps}/1 ! videoconvert ! videoscale ! \
             video/x-raw,width={ow},height={oh},format=I420 ! \
             x264enc bitrate={br} speed-preset=veryfast tune=zerolatency key-int-max={gop} bframes=0 ! \
             video/x-h264,profile=main ! h264parse ! queue ! mux. \
             {audio} ! audio/x-raw,rate=44100,channels=2 ! volume name=vol ! audioconvert ! \
             avenc_aac bitrate=128000 ! aacparse ! queue ! mux.",
            cw = canvas.w,
            ch = canvas.h,
            ow = out.0,
            oh = out.1,
            fps = cfg.fps,
            br = cfg.bitrate_kbps,
            gop = cfg.fps * 2,
            audio = audio_src,
            sink = sink,
        )
    }

    fn launch(desc: &str) -> Result<gst::Pipeline, String> {
        gst::parse::launch(desc)
            .map_err(|e| format!("创建推流管道失败: {}", e))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "无法转换为 gst::Pipeline".to_string())
    }

    pub fn start(&self, cfg: StreamConfig, canvas: Canvas, program: &Program) -> Result<(), String> {
        if self.is_streaming() {
            return Ok(());
        }
        let out = output_size(canvas, cfg.height_preset);
        let full_url = if cfg.stream_key.is_empty() {
            cfg.rtmp_url.clone()
        } else {
            format!("{}/{}", cfg.rtmp_url.trim_end_matches('/'), cfg.stream_key)
        };
        if full_url.starts_with("rtmp") == false && full_url.starts_with("file:") == false {
            return Err("推流地址必须以 rtmp:// 或 rtmps:// 开头".to_string());
        }

        // 优先麦克风；麦克风管道无法进入 Playing 时自动降级为静音音轨
        let mut mic_active = cfg.use_mic;
        let mut pipeline = Self::launch(&Self::build_desc(&cfg, canvas, out, &full_url, mic_active));
        if mic_active {
            let failed = match &pipeline {
                Ok(p) => p.set_state(gst::State::Paused).is_err(),
                Err(_) => true,
            };
            if failed {
                if let Ok(p) = &pipeline {
                    let _ = p.set_state(gst::State::Null);
                }
                mic_active = false;
                pipeline = Self::launch(&Self::build_desc(&cfg, canvas, out, &full_url, false));
            }
        }
        let pipeline = pipeline?;

        let appsrc = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or("未找到 appsrc")?;

        let (vol, mute) = *self.volume.read();
        if let Some(v) = pipeline.by_name("vol") {
            v.set_property("volume", vol);
            v.set_property("mute", mute);
        }

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("启动推流失败: {:?}", e))?;

        self.frames_pushed.store(0, Ordering::SeqCst);
        self.frames_dropped.store(0, Ordering::SeqCst);
        *self.last_error.write() = None;
        self.is_streaming.store(true, Ordering::SeqCst);

        // 帧泵线程：节目帧 -> appsrc
        let rx = program.subscribe();
        let streaming = Arc::clone(&self.is_streaming);
        let pushed = Arc::clone(&self.frames_pushed);
        let dropped = Arc::clone(&self.frames_dropped);
        std::thread::Builder::new()
            .name("stream-pump".into())
            .spawn(move || {
                while streaming.load(Ordering::Relaxed) {
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

        // 总线监视线程：出现错误（断线、编码失败等）时停止推流并把原因暴露给界面
        let bus = pipeline.bus().ok_or("推流管道没有 bus")?;
        let streaming = Arc::clone(&self.is_streaming);
        let err_slot = Arc::clone(&self.last_error);
        std::thread::Builder::new()
            .name("stream-bus".into())
            .spawn(move || {
                while streaming.load(Ordering::Relaxed) {
                    if let Some(msg) = bus.timed_pop_filtered(
                        gst::ClockTime::from_mseconds(200),
                        &[gst::MessageType::Error],
                    ) {
                        if let gst::MessageView::Error(e) = msg.view() {
                            let text = format!(
                                "{}（{}）",
                                e.error(),
                                e.src().map(|s| s.name().to_string()).unwrap_or_default()
                            );
                            eprintln!("推流错误: {}", text);
                            *err_slot.write() = Some(text);
                            streaming.store(false, Ordering::SeqCst);
                        }
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        *self.session.lock() = Some(Session {
            pipeline,
            started: Instant::now(),
            target: full_url.split('/').take(3).collect::<Vec<_>>().join("/"),
            out,
            cfg,
            mic_active,
        });
        Ok(())
    }

    pub fn stop(&self) {
        self.is_streaming.store(false, Ordering::SeqCst);
        if let Some(s) = self.session.lock().take() {
            // 发送 EOS 并短暂等待，让 FLV 收尾完整，再关闭管道
            s.pipeline.send_event(gst::event::Eos::new());
            if let Some(bus) = s.pipeline.bus() {
                let _ = bus.timed_pop_filtered(
                    gst::ClockTime::from_mseconds(1500),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                );
            }
            let _ = s.pipeline.set_state(gst::State::Null);
        }
    }
}
