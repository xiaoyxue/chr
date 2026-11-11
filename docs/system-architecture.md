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
| 离线场景建模 | 读取扫描视频，产出已注册到球场的 atlas 图像/Ti 对及同参考系下的光心 C。 | 可能在仓库外；chr 负责准备输入和接入结果。 |
| 实时追踪服务 | 使用标定参考模型分析实时视频，提供逐帧 Ti；特征、索引和匹配策略属于内部细节。 | 可能在仓库外；双向接入协议在 chr 侧需要明确。 |

Local Server 属于控制侧，不另计为第五个大模块。离线与实时分析也不因名称中有
“服务”就要求远端部署；可否本地运行、使用什么传输方式均未冻结。

## Control and data flow

```text
用户
 ↓
WebUI ↔ Local Server     ← 控制侧
             ↕
      实时渲染 CLI/服务   ← 实时侧

chr → 扫描视频 → 离线场景建模 → 校准/场地参考结果 → chr
chr → 实时视频 → 实时追踪服务 → 逐帧 Ti → chr
```

实时视频链路由现场视频运行时组织：采集的视频交给追踪服务，返回结果用于对应帧的渲染合成，
随后输出视频。扫描录制也由该运行时组织执行。控制侧负责业务配置、操作入口和状态呈现。
离线建模任务提交、文件交付与结果管理的具体分工仍待讨论。
标定记录和核心指令见 [Calibration records](calibration-control.md)。
首期 pinhole 范围、scene 上下文和帧输入分层见
[Frame ingestion and rendering](render-frame-input.md)；畸变和外部遮挡/深度输入延后。

以上是组件责任，不要求把采集、编码、追踪通信、渲染与输出放在同一线程或同一子进程中。

控制侧与实时侧可考虑 WebSocket/WebRTC 或其他适当协议。
为 CLI 跨进程控制，也可考虑直接使用本地文件 socket；具体机制与必要性待定。
不能在这里默认所有控制、素材和视频都走同一种通道。

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
当前渲染工作区已经导入，不能再视为只有占位目录：

| Path | Implemented responsibility |
| --- | --- |
| `render/crates/chr-core/` | 共享坐标数学、GPU 初始化与 wgpu 渲染。 |
| `render/native/chr-app/` | 原生窗口、事件、呈现、无窗口导出与文件写入的应用壳。 |

遵守 [render/AGENTS.md](../render/AGENTS.md)：渲染算法留在 core，交付壳不维护另一套渲染实现。
现有 [renderer](../render/README.md) 是原生/离屏三角形切片，不是已实现的 SDI、录制、
实时追踪或浏览器服务。已有坐标契约见 [Coordinate contract](../render/docs/coordinates.md)。

`client/src/` 目前仍是占位目录；`common/src/` 和 `algorithm/src/` 的实现栈未定。
后续 WebUI/Local Server 集成应结合实际实现细化，而不是在此重复搭建。
渲染服务部署提案 #4 不改变这里记录的现有路径或自动确认产品级待决问题。
