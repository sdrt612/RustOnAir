// Tauri 命令接口：前端控制台调用 Rust 原生媒体引擎
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

use crate::engine::mixer::{SwitcherState, TransitionKind};
use crate::engine::source::SourceInfo;
use crate::engine::streamer::{StreamConfig, StreamStats};
use crate::engine::{MediaEngine, Orientation, OutputConfig};

pub struct AppState {
    pub engine: Arc<MediaEngine>,
}

/// 一次性返回界面轮询所需的全部状态，减少 IPC 次数
#[derive(Serialize)]
pub struct Status {
    pub switcher: SwitcherState,
    pub stream: StreamStats,
    pub output: OutputConfig,
    pub sources: Vec<SourceInfo>,
}

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Status {
    Status {
        switcher: state.engine.get_switcher().get_state(),
        stream: state.engine.get_stream_stats(),
        output: state.engine.get_output(),
        sources: state.engine.get_sources(),
    }
}

#[tauri::command]
pub fn set_pvw(state: State<'_, AppState>, source_id: usize) {
    state.engine.get_switcher().set_pvw(source_id);
}

#[tauri::command]
pub fn set_pgm(state: State<'_, AppState>, source_id: usize) {
    state.engine.get_switcher().set_pgm(source_id);
}

#[tauri::command]
pub fn switch_cut(state: State<'_, AppState>) {
    state.engine.get_switcher().cut();
}

#[tauri::command]
pub fn switch_auto(state: State<'_, AppState>) {
    state.engine.get_switcher().auto();
}

#[tauri::command]
pub fn toggle_ftb(state: State<'_, AppState>) -> bool {
    state.engine.get_switcher().toggle_ftb()
}

#[tauri::command]
pub fn set_transition(state: State<'_, AppState>, kind: TransitionKind, duration_ms: u64) {
    let sw = state.engine.get_switcher();
    sw.set_kind(kind);
    sw.set_duration(duration_ms);
}

#[tauri::command]
pub fn set_tbar(state: State<'_, AppState>, progress: f32) {
    state.engine.get_switcher().set_tbar(progress);
}

/// 切换横/竖屏会重启全部采集（约 1~2 秒），放到阻塞线程里执行以免卡住 UI
#[tauri::command]
pub async fn set_orientation(
    state: State<'_, AppState>,
    orientation: Orientation,
) -> Result<OutputConfig, String> {
    let engine = Arc::clone(&state.engine);
    tauri::async_runtime::spawn_blocking(move || engine.set_orientation(orientation))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn set_audio(state: State<'_, AppState>, volume: f64, mute: bool) {
    state.engine.set_audio(volume, mute);
}

#[tauri::command]
pub fn start_stream(
    state: State<'_, AppState>,
    rtmp_url: String,
    stream_key: String,
    bitrate_kbps: u32,
    height_preset: u32,
    use_mic: bool,
) -> Result<(), String> {
    state.engine.start_streaming(StreamConfig {
        rtmp_url,
        stream_key,
        bitrate_kbps,
        fps: 30,
        height_preset,
        use_mic,
    })
}

#[tauri::command]
pub fn stop_stream(state: State<'_, AppState>) {
    state.engine.stop_streaming();
}
