// 媒体引擎门面：统一调度 采集源 / 切换台 / 节目渲染 / 预览服务 / 推流
pub mod compose;
pub mod mixer;
pub mod program;
pub mod server;
pub mod source;
pub mod streamer;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use mixer::Switcher;
use program::Program;
use server::PreviewServer;
use source::{Canvas, CameraSource, SourceInfo, SourceKind};
use streamer::{StreamConfig, StreamStats, Streamer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Landscape,
    Portrait,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub orientation: Orientation,
    pub canvas_w: u32,
    pub canvas_h: u32,
}

impl OutputConfig {
    fn from_canvas(c: Canvas) -> Self {
        Self {
            orientation: if c.is_portrait() { Orientation::Portrait } else { Orientation::Landscape },
            canvas_w: c.w,
            canvas_h: c.h,
        }
    }
}

pub struct MediaEngine {
    sources: Arc<RwLock<Vec<CameraSource>>>,
    switcher: Arc<Switcher>,
    program: Arc<Program>,
    streamer: Arc<Streamer>,
    _preview_server: PreviewServer,
    canvas: RwLock<Canvas>,
    /// 切换方向时会重启采集，串行化避免并发重启
    reconfig: Mutex<()>,
}

fn default_sources() -> Vec<CameraSource> {
    let pat = |id: usize, name: &str, p: &str| {
        CameraSource::new(id, name.to_string(), SourceKind::TestPattern { pattern: p.to_string() })
    };
    vec![
        CameraSource::new(
            1,
            "CAM 1".to_string(),
            SourceKind::Webcam { device_name: "Integrated Camera".to_string() },
        ),
        pat(2, "CAM 2", "smpte"),
        pat(3, "CAM 3", "ball"),
        pat(4, "CAM 4", "pinwheel"),
        pat(5, "CAM 5", "circular"),
        pat(6, "CAM 6", "colors"),
    ]
}

impl MediaEngine {
    pub fn new() -> Result<Self, String> {
        gstreamer::init().map_err(|e| format!("初始化 GStreamer 失败: {}", e))?;

        let canvas = Canvas::landscape();
        let mut sources = default_sources();
        Self::start_sources(&mut sources, canvas);

        let sources = Arc::new(RwLock::new(sources));
        // 默认 PVW 为 CAM 1，PGM 为 CAM 2
        let switcher = Arc::new(Switcher::new(1, 2));
        let program = Arc::new(Program::new());
        program.start(canvas, Arc::clone(&sources), Arc::clone(&switcher))?;

        let preview_server = PreviewServer::new(18980);
        preview_server.start(Arc::clone(&sources), Arc::clone(&switcher), Arc::clone(&program))?;

        Ok(Self {
            sources,
            switcher,
            program,
            streamer: Arc::new(Streamer::new()),
            _preview_server: preview_server,
            canvas: RwLock::new(canvas),
            reconfig: Mutex::new(()),
        })
    }

    /// 启动全部采集；真实摄像头不可用时降级为测试信号，保证 Demo 始终有画面
    fn start_sources(sources: &mut [CameraSource], canvas: Canvas) {
        for s in sources.iter_mut() {
            if let Err(e) = s.start(canvas) {
                eprintln!("启动 {} 失败，降级为测试信号: {}", s.name, e);
                s.fallback_to_pattern("snow");
                let _ = s.start(canvas);
            }
        }
    }

    pub fn get_sources(&self) -> Vec<SourceInfo> {
        self.sources.read().iter().map(|s| s.get_info()).collect()
    }

    pub fn get_switcher(&self) -> Arc<Switcher> {
        Arc::clone(&self.switcher)
    }

    pub fn get_output(&self) -> OutputConfig {
        OutputConfig::from_canvas(*self.canvas.read())
    }

    /// 切换横/竖屏：重建画布、重启采集与节目渲染。推流中不允许切换。
    pub fn set_orientation(&self, orientation: Orientation) -> Result<OutputConfig, String> {
        let _guard = self.reconfig.lock();
        if self.streamer.is_streaming() {
            return Err("正在推流，请先停止推流再切换横/竖屏".to_string());
        }
        let target = match orientation {
            Orientation::Landscape => Canvas::landscape(),
            Orientation::Portrait => Canvas::portrait(),
        };
        if *self.canvas.read() == target {
            return Ok(OutputConfig::from_canvas(target));
        }

        self.program.stop();
        {
            let mut srcs = self.sources.write();
            for s in srcs.iter_mut() {
                s.stop();
            }
            Self::start_sources(&mut srcs, target);
        }
        *self.canvas.write() = target;
        self.program
            .start(target, Arc::clone(&self.sources), Arc::clone(&self.switcher))?;
        Ok(OutputConfig::from_canvas(target))
    }

    pub fn start_streaming(&self, config: StreamConfig) -> Result<(), String> {
        let _guard = self.reconfig.lock();
        let canvas = *self.canvas.read();
        self.streamer.start(config, canvas, &self.program)
    }

    pub fn stop_streaming(&self) {
        self.streamer.stop();
    }

    pub fn set_audio(&self, volume: f64, mute: bool) {
        self.streamer.set_audio(volume, mute);
    }

    pub fn get_stream_stats(&self) -> StreamStats {
        self.streamer.get_stats()
    }
}
