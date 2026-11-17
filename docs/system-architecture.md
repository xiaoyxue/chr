# System architecture

**Status:** `draft` — 产品边界和四模块划分为 `confirmed`，部署与接口细节仍有候选方案。

**Scope:** 产品目标、逻辑模块、数据流以及它们与当前代码的关系。

**Depends on:** [Development environment](development.md)。

## Confirmed decisions

chr 初期有两大交付组件：

- **控制侧：WebUI + Local Server**。主要用户界面是 WebUI，Local Server
  为其提供本地能力，首期可以读取本地数据。该组件待集成，本文不规定其内部实现。
- **实时渲染侧：本地 CLI/服务**。可独立运行，作为现场视频运行时组织 SDI 采集、
  追踪服务接入、渲染合成、输出及录制任务。与控制侧跨进程通信，CLI 也可作为辅助控制入口。

历史 Fusion 是 Public Web Server，首期先抽象掉，不引入对其部署的依赖。
除服务端静态场景建模外，内容管理、现场准备、录制、调整、预览与直播控制均在 chr
产品范围内；实时追踪算法也可能在外部，但输入与结果接入是 chr 的职责。

## Logical modules

| Module | Responsibility | Repository scope |
| --- | --- | --- |
| 控制 WebUI（含 Local Server） | 广告位信息管理、业务操作、现场工作流、状态呈现和交互预览。 | chr；WebUI 与 Local Server 的内部拆分后续细化。 |
| 实时渲染服务（现场视频运行时） | 组织 SDI 采集、实时追踪接入、逐帧渲染合成、视频输出及录制任务，执行运行时控制。 | chr，以本地 CLI/服务形式运行。 |
| 离线场景建模 | 读取机位的扫场视频，产出该机位的 V/Q map（特征点、各视图的 V 和锚定球场的 T_ref）及同参考系下的光心 C；见 [vchao 的 atlas、V、Ti 与 T_ref](vchao-atlas.zh-CN.md)。 | 在仓库外（vchao）；chr 负责准备输入和接入结果：map 交给跟踪 worker，C 经 `--tracking-centre` 交给 `chr-server`。 |
| 实时追踪服务 | 使用标定参考模型分析实时视频，提供逐帧 Ti；特征、索引和匹配策略属于内部细节。 | 作为 `chr-server` 监管的跟踪 worker 进程运行（`server/runtime/src/worker/tracking/`；契约、镜头选择与帧/相机配对等 chr 侧逻辑在纯逻辑 crate `server/tracker/`（`chr-tracker`）），协议是 vchao 的 VchaoFrame v1 原始帧契约，与语言无关（经 GStreamer unixfd socket 传输）；vchao 的 `pitch-analysis` 按同一契约原样接入（#148），启动时加载离线建好的 V/Q map；回放已记录 Ti 的 `chr-tracking-sim-cli` 保留为替身。见 [Tracking worker](../server/docs/tracking-worker.md)。 |

Local Server 属于控制侧，不另计为第五个大模块。离线与实时分析也不因名称中有
“服务”就要求远端部署；可否本地运行、使用什么传输方式均未冻结。

## Control and data flow

```text
用户
 ↓
WebUI ↔ Local Server     ← 控制侧
             ↕
      实时渲染 CLI/服务   ← 实时侧

chr → 扫场视频 → 离线场景建模（vchao）→ V/Q map + T_ref、光心 C → chr
chr → 实时视频 → 跟踪 worker（vchao pitch-analysis，使用该 map）→ 逐帧 Ti → chr
```

实时视频链路由现场视频运行时组织：采集的视频交给追踪服务，返回结果用于对应帧的渲染合成，
随后输出视频。扫描录制也由该运行时组织执行。控制侧负责业务配置、操作入口和状态呈现。
离线建模任务提交、文件交付与结果管理的具体分工仍待讨论。
标定记录和核心指令见 [Calibration records](calibration-control.md)。
首期 pinhole 范围、scene 上下文和帧输入分层见
[Frame ingestion and rendering](render-frame-input.md)；畸变和外部遮挡/深度输入延后。

以上是组件责任，不要求把采集、编码、追踪通信、渲染与输出放在同一线程或同一子进程中。
实时渲染侧的进程划分（可重启渲染 worker、独立监看进程）见 [Live process architecture](live-process-architecture.md)。

现有运行时通过 NativeSocket 和 WebSocket 接收控制，两者使用同一处理路径。
客户端通过独立的 `chr-monitor` 进程以 WebRTC 接收 SDI 输出的监看画面；它不是另一个控制通道，
`chr-monitor` 也不控制节目输出。
已实现的 IPC profile 由通用协议文档指向权威实现。未来素材、视频输入等数据通道仍需各自定义，
不能默认它们全部复用控制通道。

业务命令及其 effect 应独立于发起来源：WebUI、CLI 或固定 trigger 均可发起操作。
上图是主要用户交互路径，不排除其他控制来源。先明确业务语义，再细化模块适配和传输方式。
通用消息规范见 [Protocol conventions](protocol-conventions.md)；
逻辑时间与模拟要求见 [Execution model](execution-model.md)。

## Preparation and live paths

准备路径：

```text
本地球场数据 + 素材 → 广告位布置 → 虚拟场地预览
SDI 扫描录制 → 高清晰视频 → 离线建模 → 结果接入
布置 + 校准结果 + 实际视频 → 现场微调与叠加测试
```

直播路径的职责依赖：

```text
输入视频 ─────────────────────────────────→ 合成 → 输出视频
    └→ 实时追踪服务 → 当帧 Ti + 相容 C → 素材运行时与渲染 ─┘
                                  ↑
                        广告位配置 + 实时控制
```

采集、合成与输出归现场视频运行时组织，但其内部进程、缓存和调度方式尚未冻结，
也不要求当前帧分析结果必须同步返回。视频、几何和渲染如何关联及处理时限属于后续接口设计。

实时执行的延迟窗口与批处理语义由 execution model 统一描述，不以图中的箭头顺序
要求每帧同步执行完所有阶段。
渲染核心不要求静态 atlas 作为输入；运行时向它提供帧图像引用及已解析的 Ti/C，
由渲染器维护的 scene 上下文决定对象状态。帧写入与渲染提交可以独立调度。

## Domain boundaries

| Concept | Boundary |
| --- | --- |
| 球场与 session | 球场基础信息长期存在，session 的覆盖范围与生命周期仍需定义。 |
| 布置方案与当前呈现状态 | 希望呈现的内容和实际输出状态不一定相同。 |
| 素材与实例 | 同一份图片、视频或模型可被多次使用，每次使用有独立状态。 |
| 相机配置与校准结果 | 同一球场可能有不同机位或多次扫描，不能仅凭球场 ID 隐式选择。 |
| 坐标系微调与广告位微调 | 前者影响场地注册，后者改变内容布置，不应混作同一操作。 |
| 原始追踪结果与人工修正 | 应能分辨算法输入和人工操作的效果；具体保存和组合方式待决。 |
| 虚拟视角预览与实际输出 | 浏览器查看虚拟场地不等于改变直播相机或证明直播合成正确。 |

## Current implementation

目录以 [Development environment](development.md) 和根 `AGENTS.md` 为准。
当前 Cargo 工作区位于仓库根，服务端与共享协议已经有可运行实现：

| Path | Implemented responsibility |
| --- | --- |
| `server/renderer/` | `chr-renderer`：共享坐标数学、GPU 初始化与 wgpu 渲染。 |
| `server/tracker/` | `chr-tracker`：跟踪纯逻辑——VchaoFrame 契约镜像与校验、pose 到相机的映射、镜头选择、帧与相机按采集序号配对及回放替身的逻辑；不含进程、GStreamer 或异步运行时。 |
| `server/runtime/` | `chr-runtime`（构建 `chr-server` 与 `chr-renderer-cli`）：无窗口控制服务、渲染管线与渲染 worker 监管、模拟 SDI 输出及共享内存节目 tap。 |
| `monitor/` | `chr-monitor`：可选的独立监看进程，读取节目 tap，编码后以 WebRTC/SRT 发送；停止或卡住不影响节目输出。 |
| `server/viewer/` | `chr-app`：原生窗口、事件、呈现、无窗口导出与文件写入的应用壳。 |
| `common/` | `chr-common`：共享控制消息、方法和 NativeSocket/WebSocket IPC 实现。 |
| `common/shm/` | `chr-shm`：进程间共享内存帧环（采集输入 ring 与节目 tap）；唯一允许 `unsafe` 的 crate（仅限其映射模块）。 |
| `client/example/render-harness/` | 浏览器示例客户端和验证 harness，不等同于完整业务 WebUI。 |

遵守 [server/AGENTS.md](../server/AGENTS.md)：渲染算法留在 renderer，交付壳不维护另一套渲染实现。
现有 [server](../server/README.md) 输出三角形的模拟 SDI 画面，并把每个实际发出的输出 slot
写入共享内存节目 tap；`chr-monitor` 编码该 tap 并发送给浏览器或 SRT 接收端。
当前编码使用 CPU readback 后的软件编码，是开发实现，不是已验证的硬件零拷贝链路。
`chr-server` 不创建 GPU 设备：每帧由它启动并监管的 `chr-renderer-cli worker` 进程渲染；worker 超时、失败或重启期间输出 clean feed（Linux 已验证，Windows 未验证）。
`chr-server` 与渲染、跟踪两个 worker 之间经 GStreamer `unixfdsink`/`unixfdsrc` socket 通信：每条消息是 buffer 上 `GstCustomMeta` 中的 JSON，帧以 memfd 文件描述符传递；一帧采集画面只拷贝一次，两个 worker 共用（#141）。
实际 SDI 采集/输出硬件、视频输入、录制、真实的实时追踪算法和 calibration 管线仍未实现；
跟踪 worker 的进程、协议与监管已实现，目前用回放替身代替算法。
已有坐标契约见 [Coordinate contract](../server/docs/coordinates.md)。

`client/src/` 目前仍是占位目录；`common/` 已是 Rust 协议的权威实现，TypeScript 客户端镜像其类型。
离线场景建模不在本仓库；实时追踪的接入已由跟踪 worker 协议定义（#128）。后续业务 WebUI/Local Server 集成应结合现有协议与示例，
而不是在此重复搭建或把示例客户端当作完整产品。
渲染服务部署提案 #4 不改变这里记录的现有路径或自动确认产品级待决问题。
