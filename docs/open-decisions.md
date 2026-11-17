# Open decisions

**Status:** `open` — 讨论议题索引，不是已承诺的实现清单。

**Scope:** 按业务、执行和技术层次跟踪未决事项，决定后的细节更新到对应主题文档。

**Depends on:** [Documentation guide](README.md)。

## Product and module boundaries

已确认边界以 [System architecture](system-architecture.md) 为准。

| ID | Question | Impact |
| --- | --- | --- |
| B1 | 配置如何保存，运行状态以谁为准，多入口控制如何协调？ | 配置、状态与控制模型。 |
| B2 | session 表示什么，与比赛、球场、机位、扫描任务是什么关系？ | 数据归属、复用和生命周期。 |
| B3 | 首期支持单路主相机，还是多相机/含切镜头的节目流？ | 校准选择、帧关联与直播运行。 |
| B4 | 首期内容包括图片、视频、3D 模型和模型动画中的哪些？ | 素材工作流与运行时能力。 |
| B5 | 投放时间是人工参考、定时开关，还是完整编排，现场如何覆盖？ | 时间线、权限与控制语义。 |
| B6 | 测试与正式播出如何区分，现场修改是否允许直接影响输出？ | 确认流程和状态切换。 |
| B7 | 扫描或校准不合格、直播期间丢失跟踪时，如何处置？ | 阶段出口与失败策略。 |

## Behavior and data models

Concrete candidates now live in [Business contexts](business-context.md) (#57),
[Recording](recording-control.md) (#58), [Calibration](calibration-control.md)
(#59) and [Tracking](tracking-control.md) (#60). Their `proposed` sections are
review inputs, not resolved entries in this index. Shared identity/error and
time/effect candidates remain in the protocol and execution documents; #12/#36
must reconcile them before wire implementation.

| Candidate | Approval still required |
| --- | --- |
| #57 | Single-camera/static-content release boundary, catalog/runtime ownership, live edit restrictions, rehearsal isolation and recovery policy. |
| #58 | Caller-selected task identity, recording profile, exclusive output directory, loss/timeout policy and metadata sufficiency. |
| #59 | Complete record encoding, local import/storage, immutable versions, load binding and restart behavior. |
| #60 | Bounded native adapter contract, reference-context lifetime, production transport and tracking-loss/output policy. |

| Topic | Open scope | Owner |
| --- | --- | --- |
| Workflow handoffs | 方案发布、校准选择、人工确认、阶段回退及重新测试条件。 | [Business workflow](business-workflow.md) |
| Functional ownership | 预览执行位置、持久化、离线任务提交与结果接入。 | [Component responsibilities](component-responsibilities.md) |
| Shared messages | ID 约束、错误码分配、schema 演进与传输适配。 | [Protocol conventions](protocol-conventions.md) |
| Execution | 时间编码、时间线映射、生效记录、冲突顺序、保护时钟与批处理调度。 | [Execution model](execution-model.md) |
| Recording | 产物组织、录制规格、时间范围、错误策略及候选 payload 的确认。 | [Cam1 input recording](recording-control.md) |
| Calibration | 任务/记录 schema、机位与坐标绑定、load 生效边界及存储方式。 | [Calibration records](calibration-control.md) |
| Frame rendering | 帧引用、scene 快照、帧池/数据入口、提交结果和失败策略。 | [Frame ingestion and rendering](render-frame-input.md) |

后续按实际讨论补充业务文档，尚不创建占位接口：

- 球场、session、相机配置、布置方案与校准结果的身份和版本。
- 球场坐标系修正与广告位模型位置修正的作用域、组合和撤销。
- 素材支持能力、实例管理、成组操作、视频与动画播放。
- 光照、透明度、虚拟视角和实际输出预览。
- 操作权限、重连、重复操作及多客户端仲裁。

## Video and integration

| Topic | Required decision |
| --- | --- |
| SDI timing | 输入、处理与输出时间如何保留、关联和映射。 |
| Buffers | 采集格式、所有权、就绪、释放及 GPU 互操作。 |
| Signal format | 分辨率、帧率、逐行/隔行及去隔行责任。 |
| Color | 像素格式、色域、传递函数、范围及转换。 |
| Compositing | 运行时内部的叠加层、合成、音频和附属数据处理。 |
| Latency | 视频、几何和动画时刻如何关联，deadline 和超时如何处理。 |
| Recording | 输入/输出录制需求、诊断关联与资源隔离。 |
| Deployment | 模块部署、素材与校准交付、控制和媒体通道。 |
| Geometry ingestion | 对接已有 pinhole 坐标契约，细化图像域、Ti/C 关联与几何不可用状态。 |
| Failure recovery | 输入与硬件失败的处理，是否需要外部旁路。渲染进程故障已由 [Live process architecture](live-process-architecture.md) 决定（clean feed + worker 重启）。 |

渲染服务的具体部署及双输出提案由 [PR #4](https://github.com/xiaoyxue/chr/pull/4)
单独推进；其方案仍需与这里的产品需求和执行约束对齐。

## Deferred extensions

畸变模型、逐帧真实场景遮挡/深度输入不纳入当前首期合同，后续再定义，
不在当前 payload 中添加缺乏语义的可选占位字段。
