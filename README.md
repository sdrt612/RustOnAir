# RustOnAir (基于 Rust + Tauri v2 + GStreamer 的高性能多机位融媒体导播台)

类似于 **vMix / OBS / 芯象 (Cgangs)** 的现代化多机位导播推流系统。

---

## 架构概览

```
┌────────────────────────────────────────────────────────┐
│                   Tauri v2 (React 18 + TS)             │
│        [PVW 预监]  [CUT / AUTO / T-Bar]  [PGM 主输出]  │
│        [机位 1]    [机位 2]   [机位 3]    [机位 4] 监视墙 │
└───────────▲──────────────────────────────▲─────────────┘
            │ Tauri IPC Commands           │ 零开销 HTTP MJPEG 预览流
            ▼                              ▼
┌────────────────────────────────────────────────────────┐
│              Rust Native 媒体引擎 (MediaEngine)        │
│  ├─ Switcher 导播状态机 (Flip-Flop Bus, T-Bar, Cut/Fade)│
│  ├─ PreviewServer (本地超低延迟 MJPEG 串流分发)         │
│  ├─ SourceManager (多路 GStreamer 管道并发采集)         │
│  │   ├─ 机位 1: 真实摄像头 (mfvideosrc MediaFoundation)│
│  │   ├─ 机位 2: SMPTE 标准彩条测试信号 (ColorBar)      │
│  │   ├─ 机位 3: 动感小球信号 (Ball Test Pattern)        │
│  │   └─ 机位 4: 数字网格/噪波 (Pinwheel / Snow)        │
│  └─ Streamer 推流器 (H.264 + AAC 硬件/软件编码推流 RTMP)│
└────────────────────────────────────────────────────────┘
```

---

## 核心功能与亮点

1. **零 IPC 瓶颈的高性能预览架构**：
   - 视频帧绝不走 Tauri WebView 的 JSON IPC 序列化；
   - 引擎内置轻量流服务，向 UI 的 `<img>` 标签直接推送 MJPEG，Chromium 硬件解码，4 机位 + PVW + PGM 同时运行 CPU 占用极低。

2. **广播级切换台操作体系**：
   - **PVW (预监)** / **PGM (播出)** 双监视器；
   - **CUT (硬切)**：瞬间翻转当前预监机位与播出机位；
   - **AUTO (渐变)**：支持 300ms / 500ms / 1000ms 动态淡入淡出转场；
   - **T-BAR (推拉杆)**：支持手动拖拽精确控制转场进度；
   - **快捷键支持**：
     - `[1]` / `[2]` / `[3]` / `[4]`：切换 PVW 预监机位；
     - `[空格键 Space]`：执行 CUT 硬切；
     - `[回车键 Enter]`：执行 AUTO 自动转场。

3. **RTMP 直播推流**：
   - 支持自定义 RTMP 推流地址（B站、斗鱼、视频号、Twitch、本地 MediaMTX / NGINX）；
   - 支持码率动态调节（1500k ~ 6000k）、帧率与时长统计。

---

## 启动方式

### 方式一：直接运行已编译的二进制 Demo
双击项目根目录下的 **`run_demo.bat`** 即可瞬间启动导播台。

### 方式二：开发模式启动 (带热重载)
在 PowerShell 中运行：
```powershell
$env:Path += ";$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_x86_64\bin;$env:USERPROFILE\.cargo\bin"
cd e:\rustonair
npx tauri dev
```

---

## 后续进阶路线 (AI 赋能)

1. **在线包装 (Online Graphics)**：
   - 引入 SVG / HTML Canvas 图层混流；
   - 预设人名条、台标、实时比分牌组件。
2. **AI 自动镜头切换 (Auto Director)**：
   - 引入 ONNX Runtime (`ort` crate) 运行人脸检测 / 说话人检测模型；
   - 根据画面中哪位主播在发话、是否有大动作，自动触发 AUTO 切换到对应机位。
3. **AI 实时语音转字幕 (Live Subtitle)**：
   - 集成 `whisper.cpp` (通过 Rust 绑定)；
   - 采集主麦克风音频流进行实时中文识别，以双语字幕条形式自动叠加在 PGM 画面底部。

