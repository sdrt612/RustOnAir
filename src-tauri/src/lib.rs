pub mod commands;
pub mod engine;

use std::sync::Arc;
use commands::AppState;
use engine::MediaEngine;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 崩溃日志：panic 信息写入 crash.log，避免在 WebView 回调里 panic 导致无声闪退
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("[{:?}] {}\n{}\n\n", std::time::SystemTime::now(), info, std::backtrace::Backtrace::force_capture());
        let path = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("crash.log"))).unwrap_or_else(|| "crash.log".into());
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
    }));

    let engine = Arc::new(MediaEngine::new().expect("初始化 MediaEngine 失败"));
    let engine_for_exit = Arc::clone(&engine);

    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .on_window_event(move |_window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // 先正常结束推流（让 FLV/RTMP 收尾），再彻底退出进程，
                // 释放 GStreamer 管道、摄像头句柄与 18980 端口
                engine_for_exit.stop_streaming();
                std::process::exit(0);
            }
        })
        .manage(AppState { engine })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::set_pvw,
            commands::set_pgm,
            commands::switch_cut,
            commands::switch_auto,
            commands::toggle_ftb,
            commands::set_transition,
            commands::set_tbar,
            commands::set_orientation,
            commands::set_audio,
            commands::start_stream,
            commands::stop_stream,
        ])
        .run(tauri::generate_context!())
        .expect("运行 Tauri 应用程序时发生错误");
}
