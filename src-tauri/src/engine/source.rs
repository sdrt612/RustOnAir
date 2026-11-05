// 媒体源模块：多路 GStreamer 采集管道，每路同时产出
//   1) 画布尺寸的 RGB 原始帧（供合成器混合/推流）
//   2) 缩小后的 JPEG 帧（供界面预览）
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// 合成画布：横屏 1280x720，竖屏 720x1280
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canvas {
    pub w: u32,
    pub h: u32,
}

impl Canvas {
    pub fn landscape() -> Self {
        Self { w: 1280, h: 720 }
    }
    pub fn portrait() -> Self {
        Self { w: 720, h: 1280 }
    }
    pub fn is_portrait(&self) -> bool {
        self.h > self.w
    }
    /// 预览缩略尺寸（长边 640）
    pub fn preview_size(&self) -> (u32, u32) {
        if self.is_portrait() {
            (360, 640)
        } else {
            (640, 360)
        }
    }
    /// 宽高比，如 (16, 9) / (9, 16)
    pub fn aspect(&self) -> (u32, u32) {
        if self.is_portrait() {
            (9, 16)
        } else {
            (16, 9)
        }
    }
    pub fn frame_len(&self) -> usize {
        (self.w * self.h * 3) as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SourceKind {
    Webcam { device_name: String },
    TestPattern { pattern: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: usize,
    pub name: String,
    pub kind: SourceKind,
    pub is_active: bool,
    pub fps: f32,
}

pub type SharedFrame = Arc<RwLock<Option<Arc<Vec<u8>>>>>;

pub struct CameraSource {
    pub id: usize,
    pub name: String,
    pub kind: SourceKind,
    pipeline: Option<gst::Pipeline>,
    latest_raw: SharedFrame,
    latest_jpeg: SharedFrame,
    frame_count: Arc<AtomicU64>,
    is_running: Arc<AtomicBool>,
    fps_probe: Mutex<(Instant, u64, f32)>,
}

impl CameraSource {
    pub fn new(id: usize, name: String, kind: SourceKind) -> Self {
        Self {
            id,
            name,
            kind,
            pipeline: None,
            latest_raw: Arc::new(RwLock::new(None)),
            latest_jpeg: Arc::new(RwLock::new(None)),
            frame_count: Arc::new(AtomicU64::new(0)),
            is_running: Arc::new(AtomicBool::new(false)),
            fps_probe: Mutex::new((Instant::now(), 0, 0.0)),
        }
    }

    pub fn get_info(&self) -> SourceInfo {
        // 每次至少间隔 0.5 秒重新估算一次帧率
        let mut probe = self.fps_probe.lock();
        let now = Instant::now();
        let count = self.frame_count.load(Ordering::Relaxed);
        let dt = now.duration_since(probe.0).as_secs_f32();
        if dt >= 0.5 {
            probe.2 = (count.saturating_sub(probe.1)) as f32 / dt;
            probe.0 = now;
            probe.1 = count;
        }
        let running = self.is_running.load(Ordering::Relaxed);
        SourceInfo {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.clone(),
            is_active: running,
            fps: if running { probe.2 } else { 0.0 },
        }
    }

    pub fn latest_raw(&self) -> Option<Arc<Vec<u8>>> {
        self.latest_raw.read().clone()
    }

    pub fn latest_jpeg(&self) -> Option<Arc<Vec<u8>>> {
        self.latest_jpeg.read().clone()
    }

    fn build_pipeline_desc(&self, canvas: Canvas) -> String {
        let (aw, ah) = canvas.aspect();
        let (pw, ph) = canvas.preview_size();
        let head = match &self.kind {
            SourceKind::Webcam { device_name } => format!("mfvideosrc device-name=\"{}\"", device_name),
            // 测试信号直接按画布尺寸生成，竖屏时画面内容完整
            SourceKind::TestPattern { pattern } => format!(
                "videotestsrc pattern={} is-live=true ! video/x-raw,width={},height={},framerate=30/1",
                pattern, canvas.w, canvas.h
            ),
        };
        format!(
            "{head} ! videoconvert ! aspectratiocrop aspect-ratio={aw}/{ah} ! videoscale ! \
             video/x-raw,width={w},height={h} ! videorate ! video/x-raw,framerate=30/1 ! \
             videoconvert ! video/x-raw,format=RGB ! tee name=t \
             t. ! queue max-size-buffers=2 leaky=downstream ! appsink name=raw max-buffers=1 drop=true sync=false \
             t. ! queue max-size-buffers=2 leaky=downstream ! videoscale ! video/x-raw,width={pw},height={ph} ! \
             videoconvert ! jpegenc quality=75 ! appsink name=jpg max-buffers=1 drop=true sync=false",
            head = head,
            aw = aw,
            ah = ah,
            w = canvas.w,
            h = canvas.h,
            pw = pw,
            ph = ph,
        )
    }

    pub fn start(&mut self, canvas: Canvas) -> Result<(), String> {
        if self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }
        let desc = self.build_pipeline_desc(canvas);
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| format!("解析管道失败: {}", e))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "无法转换为 gst::Pipeline".to_string())?;

        let get_sink = |name: &str| -> Result<gst_app::AppSink, String> {
            pipeline
                .by_name(name)
                .ok_or_else(|| format!("未找到 appsink {}", name))?
                .downcast::<gst_app::AppSink>()
                .map_err(|_| format!("{} 不是 AppSink", name))
        };

        let raw_store = Arc::clone(&self.latest_raw);
        let count = Arc::clone(&self.frame_count);
        let expected = canvas.frame_len();
        get_sink("raw")?.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(buffer) = sample.buffer() {
                        if let Ok(map) = buffer.map_readable() {
                            // 尺寸不符（协商异常）的帧直接丢弃，保证合成器永远拿到合法数据
                            if map.size() == expected {
                                *raw_store.write() = Some(Arc::new(map.as_slice().to_vec()));
                                count.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let jpeg_store = Arc::clone(&self.latest_jpeg);
        get_sink("jpg")?.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(buffer) = sample.buffer() {
                        if let Ok(map) = buffer.map_readable() {
                            *jpeg_store.write() = Some(Arc::new(map.as_slice().to_vec()));
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("启动采集管道失败: {:?}", e))?;

        self.pipeline = Some(pipeline);
        *self.latest_raw.write() = None;
        *self.latest_jpeg.write() = None;
        self.frame_count.store(0, Ordering::SeqCst);
        *self.fps_probe.lock() = (Instant::now(), 0, 0.0);
        self.is_running.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.is_running.store(false, Ordering::SeqCst);
    }

    /// 用测试信号替换当前源（摄像头不可用时降级）
    pub fn fallback_to_pattern(&mut self, pattern: &str) {
        self.kind = SourceKind::TestPattern { pattern: pattern.to_string() };
    }
}

impl Drop for CameraSource {
    fn drop(&mut self) {
        self.stop();
    }
}
