// 本地 HTTP 预览服务器：以 multipart/x-mixed-replace (MJPEG) 向前端 <img> 推送画面
// 视频帧不经过 Tauri IPC，浏览器原生解码，开销极低
use parking_lot::RwLock;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::mixer::Switcher;
use super::program::Program;
use super::source::CameraSource;

pub struct PreviewServer {
    is_running: Arc<AtomicBool>,
    port: u16,
}

#[derive(Clone, Copy)]
enum StreamTarget {
    Source(usize),
    Pvw,
    Pgm,
}

#[derive(Clone)]
struct Ctx {
    sources: Arc<RwLock<Vec<CameraSource>>>,
    switcher: Arc<Switcher>,
    program: Arc<Program>,
}

impl Ctx {
    fn frame(&self, t: StreamTarget) -> Option<Arc<Vec<u8>>> {
        match t {
            StreamTarget::Pgm => self.program.latest_jpeg(),
            StreamTarget::Pvw => {
                let id = self.switcher.get_state().pvw_id;
                self.sources.read().iter().find(|s| s.id == id).and_then(|s| s.latest_jpeg())
            }
            StreamTarget::Source(id) => self.sources.read().iter().find(|s| s.id == id).and_then(|s| s.latest_jpeg()),
        }
    }
}

impl PreviewServer {
    pub fn new(port: u16) -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            port,
        }
    }

    pub fn start(
        &self,
        sources: Arc<RwLock<Vec<CameraSource>>>,
        switcher: Arc<Switcher>,
        program: Arc<Program>,
    ) -> Result<(), String> {
        let addr = format!("127.0.0.1:{}", self.port);
        let mut retries = 0;
        let listener = loop {
            match TcpListener::bind(&addr) {
                Ok(l) => break l,
                Err(e) => {
                    retries += 1;
                    if retries >= 10 {
                        return Err(format!("绑定端口 {} 失败 (重试 10 次仍被占用): {}", self.port, e));
                    }
                    thread::sleep(Duration::from_millis(200));
                }
            }
        };
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;

        let is_running = Arc::clone(&self.is_running);
        is_running.store(true, Ordering::SeqCst);
        let ctx = Ctx { sources, switcher, program };

        thread::spawn(move || {
            while is_running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let ctx = ctx.clone();
                        thread::spawn(move || handle_client(stream, ctx));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Ok(())
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

fn handle_client(mut stream: TcpStream, ctx: Ctx) {
    // Windows 下非阻塞监听器 accept 出的连接也是非阻塞的，必须显式改回阻塞
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut buffer = [0u8; 2048];
    let n = match stream.read(&mut buffer) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buffer[..n]);
    let raw_path = req.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("");
    let path = raw_path.split('?').next().unwrap_or(raw_path);

    let target = if let Some(id) = path.strip_prefix("/stream/source/") {
        match id.parse::<usize>() {
            Ok(id) => StreamTarget::Source(id),
            Err(_) => return,
        }
    } else if path.starts_with("/stream/pvw") {
        StreamTarget::Pvw
    } else if path.starts_with("/stream/pgm") {
        StreamTarget::Pgm
    } else {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    };

    let header = "HTTP/1.1 200 OK\r\n\
                  Connection: close\r\n\
                  Cache-Control: no-cache, no-store, must-revalidate\r\n\
                  Content-Type: multipart/x-mixed-replace; boundary=mjpegframe\r\n\
                  Access-Control-Allow-Origin: *\r\n\r\n";
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }

    // 帧没变化就不重复发送，节省带宽；写超时 / 断开则退出线程
    let mut last: Option<Arc<Vec<u8>>> = None;
    loop {
        if let Some(frame) = ctx.frame(target) {
            let changed = last.as_ref().map_or(true, |l| !Arc::ptr_eq(l, &frame));
            if changed {
                let part = format!(
                    "--mjpegframe\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                    frame.len()
                );
                if stream.write_all(part.as_bytes()).is_err()
                    || stream.write_all(&frame).is_err()
                    || stream.write_all(b"\r\n").is_err()
                {
                    break;
                }
                last = Some(frame);
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}
