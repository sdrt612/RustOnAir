import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Layers } from 'lucide-react';
import './App.css';

const PREVIEW = 'http://127.0.0.1:18980/stream';

type Kind = 'mix' | 'push' | 'wipe' | 'slide' | 'split' | 'circle' | 'dip' | 'v_wipe';

interface SourceInfo { id: number; name: string; is_active: boolean; fps: number }
interface SwitcherState {
  pvw_id: number; pgm_id: number; kind: Kind; transition_progress: number;
  is_in_transition: boolean; duration_ms: number; ftb: boolean;
}
interface StreamStats {
  is_streaming: boolean; target: string; bitrate_kbps: number; fps: number; uptime_seconds: number;
  frame_count: number; dropped_frames: number; out_width: number; out_height: number;
  mic_active: boolean; error: string | null;
}
interface OutputConfig { orientation: 'landscape' | 'portrait'; canvas_w: number; canvas_h: number }
interface Status { switcher: SwitcherState; stream: StreamStats; output: OutputConfig; sources: SourceInfo[] }

// 转场效果图标（简化示意）
const FX: { kind: Kind; label: string; icon: ReactNode }[] = [
  { kind: 'mix', label: '混合', icon: <><rect x="2" y="3" width="18" height="14" rx="2" fill="#2f7bff55" /><rect x="10" y="3" width="18" height="14" rx="2" fill="#9db8ff66" /></> },
  { kind: 'push', label: '推入', icon: <><rect x="2" y="3" width="14" height="14" fill="#2f7bff88" /><rect x="16" y="3" width="12" height="14" fill="#cfe0ff99" /></> },
  { kind: 'wipe', label: '擦除', icon: <><rect x="2" y="3" width="26" height="14" fill="#2f7bff55" /><rect x="2" y="3" width="13" height="14" fill="#cfe0ffaa" /></> },
  { kind: 'slide', label: '滑动', icon: <><rect x="2" y="3" width="26" height="14" fill="#2f7bff55" /><rect x="12" y="3" width="16" height="14" fill="#cfe0ffaa" /></> },
  { kind: 'split', label: '划像', icon: <><rect x="2" y="3" width="26" height="14" fill="#2f7bff55" /><rect x="9" y="3" width="12" height="14" fill="#cfe0ffaa" /></> },
  { kind: 'circle', label: '圆形', icon: <><rect x="2" y="3" width="26" height="14" fill="#2f7bff55" /><circle cx="15" cy="10" r="6" fill="#cfe0ffaa" /></> },
  { kind: 'dip', label: '淡黑', icon: <><rect x="2" y="3" width="26" height="14" fill="#10192b" /><rect x="2" y="3" width="13" height="14" fill="#2f7bff55" /></> },
  { kind: 'v_wipe', label: '下擦', icon: <><rect x="2" y="3" width="26" height="14" fill="#2f7bff55" /><rect x="2" y="3" width="26" height="7" fill="#cfe0ffaa" /></> },
];

const fmtUptime = (s: number) =>
  [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60].map((n) => String(n).padStart(2, '0')).join(':');

/** MJPEG 预览：断流时自动重连 */
function Feed({ path, alt }: { path: string; alt: string }) {
  const [nonce, setNonce] = useState(0);
  return (
    <img
      key={`${path}-${nonce}`}
      src={`${PREVIEW}/${path}${nonce ? `?t=${nonce}` : ''}`}
      alt={alt}
      draggable={false}
      onError={() => setTimeout(() => setNonce(Date.now()), 1000)}
    />
  );
}

export default function App() {
  const [st, setSt] = useState<Status | null>(null);
  const [now, setNow] = useState(new Date());
  const [tbar, setTbar] = useState(0);
  const dragging = useRef(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);

  const [rtmpUrl, setRtmpUrl] = useState('rtmp://live.example.com/live');
  const [streamKey, setStreamKey] = useState('');
  const [bitrate, setBitrate] = useState(2500);
  const [preset, setPreset] = useState(720);
  const [useMic, setUseMic] = useState(false);
  const [volume, setVolume] = useState(100);
  const [mute, setMute] = useState(false);

  const showError = useCallback((e: unknown) => {
    setToast(String(e));
    setTimeout(() => setToast(null), 5000);
  }, []);
  const call = useCallback(
    (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args).catch(showError),
    [showError],
  );

  // 轮询状态（一次 IPC 取全部）
  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const s = await invoke<Status>('get_status');
        if (!alive) return;
        setSt(s);
        if (!dragging.current) setTbar(s.switcher.transition_progress);
      } catch (e) { /* 引擎启动瞬间可能失败，下一轮重试 */ }
    };
    tick();
    const t = setInterval(tick, 120);
    const c = setInterval(() => setNow(new Date()), 1000);
    return () => { alive = false; clearInterval(t); clearInterval(c); };
  }, []);

  const sw = st?.switcher;
  const stream = st?.stream;
  const portrait = st?.output.orientation === 'portrait';
  const streaming = !!stream?.is_streaming;

  // 快捷键：1-6 选 PVW，空格 CUT，回车 AUTO，F = FTB
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName.toLowerCase();
      if (tag === 'input' || tag === 'select' || tag === 'textarea') return;
      if (e.key >= '1' && e.key <= '6') call('set_pvw', { sourceId: Number(e.key) });
      else if (e.code === 'Space') { e.preventDefault(); call('switch_cut'); }
      else if (e.code === 'Enter') { e.preventDefault(); call('switch_auto'); }
      else if (e.key.toLowerCase() === 'f') call('toggle_ftb');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [call]);

  // 音频参数变化时同步到引擎（推流中实时生效）
  useEffect(() => { call('set_audio', { volume: volume / 100, mute }); }, [volume, mute, call]);

  const setTransition = (kind: Kind, durationMs: number) => call('set_transition', { kind, durationMs });

  const changeOrientation = async (o: 'landscape' | 'portrait') => {
    if (st?.output.orientation === o) return;
    setBusy('正在切换画布并重启采集…');
    try { await invoke('set_orientation', { orientation: o }); } catch (e) { showError(e); }
    setBusy(null);
  };

  const startStream = () =>
    call('start_stream', { rtmpUrl, streamKey, bitrateKbps: bitrate, heightPreset: preset, useMic });

  const outSize = (() => {
    const short = preset, long = Math.round((preset * 16) / 9);
    return portrait ? `${short}×${long}` : `${long}×${short}`;
  })();

  return (
    <div className={`studio ${portrait ? 'portrait' : ''}`} style={{ ['--ar' as string]: portrait ? '9 / 16' : '16 / 9' }}>
      {/* ===== 顶栏 ===== */}
      <header className="topbar">
        <div className="brand">
          <Layers color="#2f7bff" size={22} />
          <div>RustOnAir Pro<br /><small>LIVE PRODUCTION STUDIO</small></div>
        </div>
        <nav className="tabs">
          <button className="tab active">制作</button>
          <button className="tab" disabled title="开发中">推流</button>
          <button className="tab" disabled title="开发中">录像</button>
          <button className="tab" disabled title="开发中">设置</button>
        </nav>
        <div className="top-right">
          <div className="clock">
            <b>{now.toLocaleTimeString('zh-CN', { hour12: false })}</b>
            <span>{now.toLocaleDateString('zh-CN')}</span>
          </div>
          <div className={`pill ${stream?.error ? 'err' : streaming ? 'ok' : ''}`}>
            <span className="dot" />
            {stream?.error ? '推流异常' : streaming ? '推流正常' : '待机'}
          </div>
          <div className={`live-badge ${streaming ? 'on' : ''}`}>LIVE</div>
        </div>
      </header>

      {/* ===== 主体 ===== */}
      <div className="main">
        <div className={`monitors ${portrait ? 'portrait' : ''}`}>
          <section className="monitor pvw">
            <div className="monitor-head"><span>PVW · CAM {sw?.pvw_id}</span><span className="muted">预监</span></div>
            <div className="screen"><Feed path="pvw" alt="PVW" /></div>
          </section>
          <section className="monitor pgm">
            <div className="monitor-head">
              <span>PGM · CAM {sw?.pgm_id}</span>
              <span className="tag-live">{sw?.ftb ? 'FTB' : 'ON AIR'}</span>
            </div>
            <div className="screen">
              <Feed path="pgm" alt="PGM" />
              {sw?.ftb && <div className="ftb-mask">FADE TO BLACK</div>}
            </div>
          </section>
        </div>

        <aside className="side">
          <div className="card">
            <div className="card-title">转场切换 <small>TRANSITION</small></div>
            <div className="trans-main">
              <button className="btn cut" onClick={() => call('switch_cut')} title="空格">CUT</button>
              <button className="btn auto" onClick={() => call('switch_auto')} title="回车">AUTO</button>
              <button className={`btn ftb ${sw?.ftb ? 'on' : ''}`} onClick={() => call('toggle_ftb')} title="F">FTB</button>
            </div>
            <div className="fx-grid">
              {FX.map((f) => (
                <button key={f.kind} className={`fx ${sw?.kind === f.kind ? 'active' : ''}`}
                  onClick={() => setTransition(f.kind, sw?.duration_ms ?? 1000)}>
                  <svg viewBox="0 0 30 20">{f.icon}</svg>{f.label}
                </button>
              ))}
            </div>
            <div className="dur">时长
              <input type="range" min={200} max={3000} step={100} value={sw?.duration_ms ?? 1000}
                onChange={(e) => setTransition(sw?.kind ?? 'mix', Number(e.target.value))} />
              <b>{((sw?.duration_ms ?? 1000) / 1000).toFixed(1)}s</b>
            </div>
          </div>
          <div className="card tbar-card">
            <div className="card-title" style={{ marginBottom: 0 }}>T-BAR <small>手动转场</small></div>
            <div className="tbar-wrap">
              <input className="tbar" type="range" min={0} max={1} step={0.01} value={tbar}
                onPointerDown={() => { dragging.current = true; }}
                onPointerUp={() => { dragging.current = false; }}
                onChange={(e) => { const v = Number(e.target.value); setTbar(v); call('set_tbar', { progress: v }); }} />
            </div>
            <span className="muted">{Math.round(tbar * 100)}%</span>
          </div>
        </aside>
      </div>

      {/* ===== 多画面监看 ===== */}
      <section className={`card multiview ${portrait ? 'portrait' : ''}`}>
        <div className="card-title">多画面监看 <small>MULTI-VIEW · 点击=设为PVW，PGM 按钮=直切播出</small></div>
        <div className="tiles">
          {st?.sources.map((s) => {
            const isPgm = sw?.pgm_id === s.id, isPvw = sw?.pvw_id === s.id;
            return (
              <div key={s.id} className={`tile ${isPgm ? 'is-pgm' : isPvw ? 'is-pvw' : ''}`}
                onClick={() => call('set_pvw', { sourceId: s.id })}>
                <div className="thumb">
                  <Feed path={`source/${s.id}`} alt={s.name} />
                  <span className="num">{s.id} {s.name}</span>
                  <span className="fps">{s.is_active ? `${s.fps.toFixed(0)} fps` : '无信号'}</span>
                  <span className="btns">
                    <button className="p" onClick={(e) => { e.stopPropagation(); call('set_pvw', { sourceId: s.id }); }}>PVW</button>
                    <button className="g" onClick={(e) => { e.stopPropagation(); call('set_pgm', { sourceId: s.id }); }}>PGM</button>
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </section>

      {/* ===== 底部：输出 / 音频 / 推流 ===== */}
      <div className="bottom">
        <div className="card">
          <div className="card-title">输出设置 <small>OUTPUT</small></div>
          <div className="row">
            <label>画面方向</label>
            <div className="seg">
              <button className={!portrait ? 'active' : ''} disabled={streaming} onClick={() => changeOrientation('landscape')}>横屏 16:9</button>
              <button className={portrait ? 'active' : ''} disabled={streaming} onClick={() => changeOrientation('portrait')}>竖屏 9:16</button>
            </div>
          </div>
          <div className="out-preview">
            <div className={`shape ${portrait ? 'p' : 'l'}`}>{outSize}</div>
            <div className="hint">
              推流分辨率 {outSize} · 30fps<br />
              横竖屏会居中裁切各机位画面。<br />
              {streaming ? '推流中不可切换方向' : '切换会重启采集（约 1–2 秒）'}
            </div>
          </div>
        </div>

        <div className="card">
          <div className="card-title">音频 <small>AUDIO</small></div>
          <div className="mixer">
            <div className="chan">
              <span>MIC</span>
              <div className="seg" style={{ marginTop: 14 }}>
                <button className={useMic ? 'active' : ''} disabled={streaming} onClick={() => setUseMic(!useMic)}>
                  {useMic ? '麦克风 开' : '静音轨'}
                </button>
              </div>
              <span className="hint" style={{ textAlign: 'center' }}>推流时生效<br />失败自动降级静音</span>
            </div>
            <div className="chan">
              <span>MASTER</span>
              <input type="range" min={0} max={200} value={volume} onChange={(e) => setVolume(Number(e.target.value))} />
              <span>{volume}%</span>
              <button className={`mute ${mute ? 'on' : ''}`} onClick={() => setMute(!mute)}>{mute ? '已静音' : '静音'}</button>
            </div>
          </div>
        </div>

        <div className="card">
          <div className="card-title">推流控制 <small>RTMP</small></div>
          <div className="row"><label>服务器</label>
            <input type="text" value={rtmpUrl} disabled={streaming} onChange={(e) => setRtmpUrl(e.target.value)} placeholder="rtmp://…" /></div>
          <div className="row"><label>推流码</label>
            <input type="password" value={streamKey} disabled={streaming} onChange={(e) => setStreamKey(e.target.value)} placeholder="平台分配的推流码" /></div>
          <div className="row"><label>画质</label>
            <select value={preset} disabled={streaming} onChange={(e) => setPreset(Number(e.target.value))}>
              <option value={720}>720p</option><option value={1080}>1080p</option>
            </select>
            <select value={bitrate} disabled={streaming} onChange={(e) => setBitrate(Number(e.target.value))}>
              <option value={1500}>1500 kbps</option><option value={2500}>2500 kbps</option>
              <option value={4000}>4000 kbps</option><option value={6000}>6000 kbps</option>
            </select>
          </div>
          <div className="stream-actions">
            <button className="btn start" disabled={streaming} onClick={startStream}>▶ 开始推流</button>
            <button className="btn stop" disabled={!streaming} onClick={() => call('stop_stream')}>■ 停止</button>
          </div>
          {streaming && stream ? (
            <div className="stats">
              <span>时长 <b>{fmtUptime(stream.uptime_seconds)}</b></span>
              <span>{stream.out_width}×{stream.out_height}</span>
              <span>帧率 <b>{stream.fps.toFixed(1)}</b></span>
              <span>丢帧 <b>{stream.dropped_frames}</b></span>
              <span>音频 <b>{stream.mic_active ? '麦克风' : '静音轨'}</b></span>
            </div>
          ) : null}
          {stream?.error && <div className="err-text">⚠ {stream.error}</div>}
        </div>
      </div>

      <footer className="footer">
        <span>RustOnAir v0.2 · Tauri v2 + Rust + GStreamer</span>
        <span>快捷键：1-6 选PVW · 空格 CUT · 回车 AUTO · F 淡黑</span>
      </footer>

      {toast && <div className="toast">{toast}</div>}
      {busy && <div className="busy">{busy}</div>}
    </div>
  );
}

