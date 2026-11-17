# Live process overview / 实时进程总览

**Status:** `implemented` on Linux (#100, #141, #154). Details: [architecture](live-process-architecture.md),
[implementation plan](live-process-implementation.md), [GStreamer core](../server/docs/gst-core.md).

- [English](#english)
- [中文](#中文)

## English

The live service runs on Linux as five processes: `chr-sdi-in` captures,
`chr-server` runs the program on one GStreamer pipeline and supervises a tracking
worker and a render worker, and `chr-monitor` streams what the output sent.
Windows PCs only preview its SRT output (for example in VLC).

```mermaid
flowchart TB
    input["Camera or clip<br/>chr-sdi-in: file, test pattern or DeckLink"]
    control["pitch-studio / Web UI / Agent"]
    offline[("Before the match, offline (vchao)<br/>V/Q map + T_ref, optical centre C")]

    subgraph server["chr-server (package chr-runtime, no GPU)"]
        direction TB
        core["GStreamer core: capture → tracking hop → render hop<br/>per-slot selector: the frame's CG if it arrives by capture + D,<br/>else its own clean feed → output (simulated or DeckLink)"]
        supervisor["Supervisor: restarts a crashed worker<br/>and relinks it into the running pipeline"]
    end

    subgraph tracking["tracking worker"]
        estimate["pitch-analysis (vchao, GPU): frame → Ti or unavailable<br/>or chr-tracking-sim-cli: replays recorded Ti"]
    end

    subgraph worker["chr-renderer-cli worker (GPU, wgpu)"]
        render["description + camera → PBR / shadows / composition → UYVY<br/>exits on device loss; restarted after a crash"]
    end

    tap[("program tap<br/>chr-shm, 8 slots<br/>$XDG_RUNTIME_DIR/chr/program.tap")]

    subgraph monitor["chr-monitor (separate process)"]
        encode["Follow tap → ≤1080p H.264<br/>(NVENC, x264 or openh264)"]
    end

    webrtc["WebRTC (ws :8789, ICE-TCP :8790-8793)"]
    srt["SRT (:9001)<br/>preview in VLC on Windows"]

    input -- "unixfd socket: memfd frames + audio" --> server
    control -- "JSON-RPC (ws :8787 / NativeSocket)" --> server
    offline -- "map, loaded at start" --> tracking
    offline -- "C (tracking_centre)" --> server
    server -- "unixfd socket: frame N + VchaoFrame" --> tracking
    tracking -- "frame N + Ti or unavailable" --> server
    server -- "unixfd socket: render + frame N + camera" --> worker
    worker -- "done + UYVY frame (memfd) / failed" --> server
    server -- "every output slot" --> tap
    tap --> monitor
    encode --> webrtc
    encode --> srt
```

Key points:

- **Fixed delay, per-slot clean feed:** every frame leaves its capture time
  plus the program delay D (`--program-delay-ms`) later, with its own CG if that
  arrived in time, else with its own clean feed (the input without CG). A frame
  never borrows another frame's camera.
- **Fault isolation:** the GPU is used only in the workers. If a worker crashes,
  hangs or loses its device, program output continues as clean feed; the worker
  is restarted and relinked into the running pipeline, and CG returns.
- **Monitoring is decoupled:** `chr-monitor` only reads the program tap and never
  feeds back. Encoding and streaming cannot stall program output, and the monitor
  restarts on its own.
- **Transport:** `chr-sdi-in` and both workers talk to `chr-server` over
  GStreamer `unixfdsink`/`unixfdsrc` sockets. Each message is JSON in a buffer's
  custom meta; frames are memfd memory passed as file descriptors, so a captured
  frame is written once and shared by `chr-server`, both workers and the delay
  queue.
- **Shared memory (`chr-shm`):** only the program tap, a single-writer,
  many-reader ring; torn reads are discarded.
- **Tracking:** vchao's `pitch-analysis` estimates each frame's `Ti` against a
  V/Q map built offline for the station; chr-server gets the station's optical
  centre C from `--tracking-centre`. `chr-tracking-sim-cli` replays a clip's
  recorded `Ti` and estimates nothing. See
  [tracking worker](../server/docs/tracking-worker.md) and
  [vchao atlas, V, Ti and T_ref](vchao-atlas.md).
- **Code layout:** `server/renderer` (`chr-renderer`, the library),
  `server/tracker` (`chr-tracker`, tracking logic), `server/runtime`
  (`chr-server`, `chr-renderer-cli`, `chr-tracking-sim-cli`, `chr-sdi-in`),
  `monitor/` (`chr-monitor`), `common/` (protocol, IPC and `chr-shm`),
  `server/viewer` (local window and PNG export).
- **Generated programs** (triangle, `--synthetic-program`, `--pitch-demo`,
  `--software-video`) have no input and keep an internal clock.
- **Offline rendering:** `chr-renderer-cli render` renders one frame, identical
  to the worker's output.

Not done yet: physical SDI capture and output, with the DeckLink code paths,
genlock and embedded audio verified on a card (#80, #161); vchao tracking with
a map of an onsite station (#148); a hot-standby worker.

## 中文

直播服务在 Linux 上以五个进程运行：`chr-sdi-in` 负责采集；`chr-server` 在一条 GStreamer 流水线上运行节目，
并监管一个跟踪 worker 和一个渲染 worker；`chr-monitor` 推流输出端实际发出的画面。
Windows 电脑只用来预览它的 SRT 输出（例如用 VLC）。

```mermaid
flowchart TB
    input["摄像机或视频片段<br/>chr-sdi-in：文件、测试图或 DeckLink"]
    control["pitch-studio / Web UI / Agent"]
    offline[("赛前离线（vchao）<br/>V/Q map + T_ref、光心 C")]

    subgraph server["chr-server（包名 chr-runtime，无 GPU）"]
        direction TB
        core["GStreamer core：采集 → 跟踪环节 → 渲染环节<br/>逐 slot 选择：CG 在 采集时间 + D 之前到达就用它，<br/>否则用该帧自己的 clean feed → 输出（模拟或 DeckLink）"]
        supervisor["Supervisor：重启崩溃的 worker，<br/>并把它重新接入正在运行的流水线"]
    end

    subgraph tracking["跟踪 worker"]
        estimate["pitch-analysis（vchao，GPU）：帧 → Ti 或 unavailable<br/>或 chr-tracking-sim-cli：回放已记录的 Ti"]
    end

    subgraph worker["chr-renderer-cli worker（GPU，wgpu）"]
        render["description + 相机 → PBR / 阴影 / 合成 → UYVY<br/>设备丢失即退出；崩溃后重启"]
    end

    tap[("program tap<br/>chr-shm，8 个 slot<br/>$XDG_RUNTIME_DIR/chr/program.tap")]

    subgraph monitor["chr-monitor（独立进程）"]
        encode["跟随 tap → ≤1080p H.264<br/>（NVENC、x264 或 openh264）"]
    end

    webrtc["WebRTC（ws :8789，ICE-TCP :8790-8793）"]
    srt["SRT（:9001）<br/>在 Windows 上用 VLC 预览"]

    input -- "unixfd socket：memfd 帧 + 音频" --> server
    control -- "JSON-RPC（ws :8787 / NativeSocket）" --> server
    offline -- "map，启动时加载" --> tracking
    offline -- "C（tracking_centre）" --> server
    server -- "unixfd socket：第 N 帧 + VchaoFrame" --> tracking
    tracking -- "第 N 帧 + Ti 或 unavailable" --> server
    server -- "unixfd socket：render + 第 N 帧 + 相机" --> worker
    worker -- "done + UYVY 帧（memfd）/ failed" --> server
    server -- "每个输出 slot" --> tap
    tap --> monitor
    encode --> webrtc
    encode --> srt
```

要点：

- **固定延迟，逐 slot 的 clean feed：** 每一帧在采集时间加节目延迟 D（`--program-delay-ms`）时发出：
  它的 CG 及时到达就带 CG，否则发它自己的 clean feed（不带 CG 的原始画面）。一帧绝不借用别的帧的相机。
- **故障隔离：** 只有 worker 使用 GPU。worker 崩溃、卡住或丢失设备时，节目输出以 clean feed 继续；
  worker 重启并重新接入正在运行的流水线后，CG 自动恢复。
- **监看解耦：** `chr-monitor` 只读 program tap，从不回写。编码和推流不会拖慢节目输出，
  monitor 也可以单独重启。
- **传输：** `chr-sdi-in` 和两个 worker 都通过 GStreamer `unixfdsink`/`unixfdsrc` socket 与
  `chr-server` 通信。每条消息是 buffer 自定义 meta 里的 JSON；帧是以文件描述符传递的 memfd 内存，
  所以一帧采集画面只写一次，由 `chr-server`、两个 worker 和延迟队列共用。
- **共享内存（`chr-shm`）：** 只剩 program tap，一个单写多读的环形缓冲，读到被覆盖了一半的帧直接丢弃。
- **跟踪：** vchao 的 `pitch-analysis` 依据为该机位离线建好的 V/Q map 估计每一帧的 `Ti`；
  `chr-server` 通过 `--tracking-centre` 得到该机位的光心 C。`chr-tracking-sim-cli` 回放片段已记录的
  `Ti`，不做任何估计。见 [tracking worker](../server/docs/tracking-worker.md) 和
  [vchao 的 atlas、V、Ti 与 T_ref](vchao-atlas.zh-CN.md)。
- **代码分层：** `server/renderer`（`chr-renderer`，渲染库）、`server/tracker`（`chr-tracker`，跟踪逻辑）、
  `server/runtime`（`chr-server`、`chr-renderer-cli`、`chr-tracking-sim-cli`、`chr-sdi-in`）、
  `monitor/`（`chr-monitor`）、`common/`（协议、IPC 和 `chr-shm`）、`server/viewer`（本地窗口和 PNG 导出）。
- **生成类节目**（三角形、`--synthetic-program`、`--pitch-demo`、`--software-video`）没有输入，
  使用内部时钟。
- **离线渲染：** `chr-renderer-cli render` 单独渲染一帧，结果与 worker 渲染的一致。

尚未完成：物理 SDI 采集和输出，包括在真实板卡上验证 DeckLink 代码路径、genlock 和嵌入音频（#80、#161）；
使用现场机位 map 的 vchao 跟踪（#148）；热备 worker。
