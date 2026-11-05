// 节目渲染器：每 1/30 秒从切换台状态取出 PGM/PVW 两路原始帧，按转场混合、FTB 淡黑，
// 产出节目 RGB 帧（供推流）并编码成 JPEG（供界面 PGM 预览）
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::compose::{apply_fade, compose};
use super::mixer::Switcher;
use super::source::{Canvas, CameraSource, SharedFrame};

/// 零拷贝地把 Arc<Vec<u8>> 交给 GStreamer 缓冲区
pub struct ArcBytes(pub Arc<Vec<u8>>);
impl AsRef<[u8]> for ArcBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

pub struct Program {
    jpeg: SharedFrame,
    subscribers: Arc<Mutex<Vec<crossbeam_channel::Sender<Arc<Vec<u8>>>>>>,
    running: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Program {
    pub fn new() -> Self {
        Self {
            jpeg: Arc::new(RwLock::new(None)),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            handle: Mutex::new(None),
        }
    }

    pub fn latest_jpeg(&self) -> Option<Arc<Vec<u8>>> {
        self.jpeg.read().clone()
    }

    /// 订阅节目帧（推流器使用）。队列满时旧订阅者会丢帧而不是阻塞渲染线程。
    pub fn subscribe(&self) -> crossbeam_channel::Receiver<Arc<Vec<u8>>> {
        let (tx, rx) = crossbeam_channel::bounded(3);
        self.subscribers.lock().push(tx);
        rx
    }

    pub fn start(
        &self,
        canvas: Canvas,
        sources: Arc<RwLock<Vec<CameraSource>>>,
        switcher: Arc<Switcher>,
    ) -> Result<(), String> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // 预览编码管道：RGB -> 缩放 -> JPEG
        let (pw, ph) = canvas.preview_size();
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true max-buffers=2 leaky-type=downstream \
             caps=video/x-raw,format=RGB,width={w},height={h},framerate=30/1 ! videoconvert ! videoscale ! \
             video/x-raw,width={pw},height={ph} ! jpegenc quality=80 ! appsink name=sink max-buffers=1 drop=true sync=false",
            w = canvas.w,
            h = canvas.h,
            pw = pw,
            ph = ph
        );
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| {
                self.running.store(false, Ordering::SeqCst);
                format!("创建节目预览管道失败: {}", e)
            })?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "无法转换为 gst::Pipeline".to_string())?;
        let appsrc = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or("未找到 appsrc")?;
        let appsink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or("未找到 appsink")?;

        let jpeg_store = Arc::clone(&self.jpeg);
        appsink.set_callbacks(
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
            .map_err(|e| format!("启动节目预览管道失败: {:?}", e))?;

        let running = Arc::clone(&self.running);
        let subscribers = Arc::clone(&self.subscribers);
        let (w, h) = (canvas.w as usize, canvas.h as usize);

        let handle = std::thread::Builder::new()
            .name("program-renderer".into())
            .spawn(move || {
                let frame_dur = Duration::from_micros(33_333);
                let mut next = Instant::now();
                let black = Arc::new(vec![0u8; w * h * 3]);
                let mut ftb_level = 1.0f32; // 1=正常画面 0=全黑，平滑过渡
                let mut scratch: Vec<u8> = Vec::with_capacity(w * h * 3);

                while running.load(Ordering::Relaxed) {
                    let st = switcher.get_state();

                    let (a, b) = {
                        let src = sources.read();
                        let get = |id: usize| src.iter().find(|s| s.id == id).and_then(|s| s.latest_raw());
                        (get(st.pgm_id), if st.is_in_transition { get(st.pvw_id) } else { None })
                    };
                    let a = a.unwrap_or_else(|| Arc::clone(&black));

                    let mut frame: Arc<Vec<u8>> = match b {
                        Some(b) if st.is_in_transition => {
                            compose(&a, &b, w, h, st.kind, st.transition_progress, &mut scratch);
                            Arc::new(std::mem::replace(&mut scratch, Vec::with_capacity(w * h * 3)))
                        }
                        _ => a,
                    };

                    // FTB：每帧向目标亮度靠近 1/20（约 0.67 秒完成）
                    let target = if st.ftb { 0.0 } else { 1.0 };
                    ftb_level += (target - ftb_level).clamp(-0.05, 0.05);
                    if ftb_level < 0.999 {
                        let mut v = (*frame).clone();
                        apply_fade(&mut v, ftb_level);
                        frame = Arc::new(v);
                    }

                    // 分发给推流订阅者（满则丢帧，并清理已断开的订阅者）
                    {
                        let mut subs = subscribers.lock();
                        subs.retain(|tx| match tx.try_send(Arc::clone(&frame)) {
                            Ok(_) | Err(crossbeam_channel::TrySendError::Full(_)) => true,
                            Err(crossbeam_channel::TrySendError::Disconnected(_)) => false,
                        });
                    }
                    // 送入预览编码
                    let _ = appsrc.push_buffer(gst::Buffer::from_slice(ArcBytes(frame)));

                    next += frame_dur;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else if now - next > Duration::from_millis(200) {
                        next = now; // 落后太多就重新对齐，避免追帧风暴
                    }
                }
                let _ = pipeline.set_state(gst::State::Null);
            })
            .map_err(|e| e.to_string())?;
        *self.handle.lock() = Some(handle);
        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().take() {
            let _ = h.join();
        }
        self.subscribers.lock().clear();
        *self.jpeg.write() = None;
    }
}

