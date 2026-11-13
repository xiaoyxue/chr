# Live process overview / 实时进程总览

**Status:** `implemented` on Linux (#100). Details: [architecture](live-process-architecture.md),
[implementation plan](live-process-implementation.md).

- [English](#english)
- [中文](#中文)

## English

The live service runs on Linux as three processes. Windows PCs only preview its
SRT output (for example in VLC).

```mermaid
flowchart TB
    input["Input video<br/>MP4 / GStreamer (physical SDI later, #80)"]
    control["Web UI / Agent"]

    subgraph server["chr-server (package chr-runtime, no GPU)"]
        direction TB
        scene["Retained scene owner · frame scheduling · CFR pacing"]
        supervisor["Supervisor<br/>timeout / crash → clean feed (CPU BT.709 → UYVY)<br/>or repeat last frame, counted"]
    end

    subgraph worker["chr-renderer-cli worker (GPU, wgpu)"]
        render["SceneAdapter binds snapshot →<br/>PBR / shadows / MSAA / composition → UYVY<br/>exits on device loss; restarts in ~1 s after kill"]
    end

    tap[("program tap<br/>chr-shm, 8 slots<br/>$XDG_RUNTIME_DIR/chr/program.tap")]

    subgraph monitor["chr-monitor (separate process)"]
        encode["Follow tap → H.264 (openh264 / GStreamer)"]
    end

    webrtc["WebRTC (ws 127.0.0.1:8789)"]
    srt["SRT (--srt, e.g. :18890)<br/>preview in VLC on Windows"]

    input --> server
    control -- "JSON-RPC (ws 127.0.0.1:8787 / NativeSocket)" --> server
    server -- "JSON lines: render / done / failed" --> worker
    server -- "input ring (chr-shm, 4 slots)" --> worker
    worker -- "output ring (chr-shm, 4 slots)" --> server
    server -- "program frames" --> tap
    tap --> monitor
    encode --> webrtc
    encode --> srt
```

Key points:

- **Fault isolation:** the GPU is used only in the worker. If the worker crashes,
  hangs or loses its device, program output continues. A frame the worker misses
  by its deadline (3/4 of a frame by default) goes out as clean feed (the input
  without CG); CG returns when the worker restarts.
- **Monitoring is decoupled:** `chr-monitor` only reads the program tap and never
  feeds back. Encoding and streaming cannot stall program output, and the monitor
  restarts on its own.
- **Shared memory (`chr-shm`):** single-writer, many-reader rings on mapped
  files; torn reads are discarded. A 1080p RGBA frame costs about 1.3 ms to
  write and 1.1 ms to read.
- **Code layout:** `server/renderer` (`chr-renderer`, the library),
  `server/runtime` (`chr-server` and `chr-renderer-cli`), `monitor/`
  (`chr-monitor`), `common/` (protocol and IPC), `server/viewer` (local window
  and PNG export).
- **Offline rendering:** `chr-renderer-cli render` renders one frame, identical
  to the worker's output.

Not done yet: physical SDI input/output, genlock and fixed delay (#80), hardware
H.264 encoding (#3) and a hot-standby worker.

## 中文

直播服务在 Linux 上以三个进程运行；Windows 电脑只用来预览它的 SRT 输出（例如用 VLC）。

```mermaid
flowchart TB
    input["输入视频<br/>MP4 / GStreamer（将来接物理 SDI，#80）"]
    control["Web UI / Agent"]

    subgraph server["chr-server（包名 chr-runtime，无 GPU）"]
        direction TB
        scene["场景所有者（retained scene）· 帧调度 · 恒定帧率节拍"]
        supervisor["Supervisor<br/>超时 / 崩溃 → clean feed（CPU BT.709 → UYVY）<br/>或重复上一帧，并计数"]
    end

    subgraph worker["chr-renderer-cli worker（GPU，wgpu）"]
        render["SceneAdapter 绑定场景快照 →<br/>PBR / 阴影 / MSAA / 合成 → UYVY<br/>设备丢失即退出；被 kill 后约 1 秒重启"]
    end

    tap[("program tap<br/>chr-shm，8 个 slot<br/>$XDG_RUNTIME_DIR/chr/program.tap")]

    subgraph monitor["chr-monitor（独立进程）"]
        encode["跟随 tap → H.264（openh264 / GStreamer）"]
    end

    webrtc["WebRTC（ws 127.0.0.1:8789）"]
    srt["SRT（--srt，例如 :18890）<br/>在 Windows 上用 VLC 预览"]

    input --> server
    control -- "JSON-RPC（ws 127.0.0.1:8787 / NativeSocket）" --> server
    server -- "JSON lines：render / done / failed" --> worker
    server -- "输入 ring（chr-shm，4 个 slot）" --> worker
    worker -- "输出 ring（chr-shm，4 个 slot）" --> server
    server -- "节目帧" --> tap
    tap --> monitor
    encode --> webrtc
    encode --> srt
```

要点：

- **故障隔离：** 只有 worker 使用 GPU。worker 崩溃、卡住或丢失设备时，节目输出不中断：
  worker 没在截止时间（默认帧周期的 3/4）内交付的帧改发 clean feed（不带 CG 的原始画面），
  worker 重启后自动接回 CG。
- **监看解耦：** `chr-monitor` 只读 program tap，从不回写。编码和推流不会拖慢节目输出，
  monitor 也可以单独重启。
- **共享内存（`chr-shm`）：** 基于映射文件的单写多读环形缓冲，读到被覆盖了一半的帧直接丢弃。
  1080p RGBA 一帧写约 1.3 ms、读约 1.1 ms。
- **代码分层：** `server/renderer`（`chr-renderer`，渲染库）、`server/runtime`
  （`chr-server` 和 `chr-renderer-cli`）、`monitor/`（`chr-monitor`）、`common/`
  （协议和 IPC）、`server/viewer`（本地窗口和 PNG 导出）。
- **离线渲染：** `chr-renderer-cli render` 单独渲染一帧，结果与 worker 渲染的一致。

尚未完成：物理 SDI 输入/输出、genlock 与固定延迟（#80），硬件 H.264 编码（#3），以及热备 worker。
