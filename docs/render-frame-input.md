# Frame ingestion and rendering

**Status:** `draft` — 首期几何范围和持久 scene 上下文为 `confirmed`；
帧池、发布/提交边界及快照时机为 `proposed`。

**Scope:** 逐帧画面和几何如何进入渲染，如何引用 scene 状态，以及输入资源的生命周期。

**Depends on:** [Protocol conventions](protocol-conventions.md)、[Execution model](execution-model.md)、
[Coordinate contract](../server/docs/coordinates.md)。

The candidate business contracts in #57-#60 reuse this ownership boundary.
[Calibration](calibration-control.md) assigns an immutable calibration/tracking
binding at tracker admission; [Tracking](tracking-control.md) retains it for the
associated frame. Render submission must resolve compatible C/world/scene state,
not substitute whichever calibration is active when a result arrives. The exact
submission schema and shared candidate time/identity alignment remain with
#12/#21/#22/#36.

## Confirmed decisions

- 首期只支持 pinhole，不包含畸变参数，也不预留含糊的占位字段。
- 渲染器维护 `scene` 状态上下文，对象、材质、变换、光照、透明度和播放状态由其他指令修改，
  不逐帧重复输入完整对象清单。
- 输出规格和颜色处理等由预先定义的配置确定，不逐帧变化。
- 外部真实场景遮挡、深度或分割输入延后讨论，不加入本阶段逐帧合同。
  这不取消虚拟物体之间正常的深度测试。
- 渲染核心不以静态 atlas 或 calibration 记录作为必需输入。

本阶段输入按 pinhole 图像域解释，不宣称对实际带畸变的信号已经完成畸变校正。

## Input layers

| Layer | Contents | Update boundary |
| --- | --- | --- |
| `render_configuration` | 输入/输出规格、像素格式和颜色处理等。 | 运行前确定，重配置流程另行定义。 |
| `scene_context` | 对象、材质、变换、光照、透明度、动画及播放状态。 | 状态修改指令。 |
| `frame_input` | `FrameKey`、`image_ref`、逻辑时间、Ti 和 C。 | 每帧关联，允许成批调度。 |

这里列出逻辑角色，不冻结字段名或序列化形状。
Ti 将球场地面坐标映射到对应图像 SNC；C、Ti 和 scene 对象使用相容的世界坐标。
即使动画状态位于 scene 中，每帧动画求值仍使用明确的逻辑时间，不使用机器墙钟。
相同逻辑输入与 scene 决定目标画面，不未经约定要求跨 GPU 的像素逐位一致。

## Logical operations

画面写入与渲染提交解耦，以下为操作边界，尚未确定公开方法名：

| Operation | Input, logical | Result, logical | Successful effect |
| --- | --- | --- | --- |
| `Publish frame` | `FrameKey`、已写入的图像资源及可读依赖。 | 与源帧关联的 `image_ref`，包含可解析的就绪条件。 | 画面引用可供消费者使用，不表示已经渲染。 |
| `Submit render` | 一帧或一批帧的引用、逻辑时间及匹配的 Ti/C。 | 提交回执，关联帧身份及实际绑定的 `scene_revision`。 | 渲染工作已受理并绑定一致 scene 状态，不表示已经输出。 |

真正完成的结果需关联输入帧、实际 scene 版本及输出画面；其返回、通知或输出队列机制待定。
具体 params/result/error schema 在下一轮方法设计中确定，不以逻辑表代替可实现的协议。
这些操作不一定逐帧经过公共控制入口；原生适配器可以直接调用相同内部契约。
申请 slot、上传像素和 GPU 同步也不必全部暴露为用户控制指令。

## Frame association and scheduling

```text
Capture / decode → Frame storage → Published image reference ─┐
                                                             ├→ Schedule → Render → Output
Tracking → Ti/C associated with FrameKey ────────────────────┘
                                                                           ↑
                                                                    Scene context
```

图像与几何可以独立到达，按 `FrameKey` 关联，不能用到达顺序或缓存序号配对。
C 不一定由 tracking 每帧重复传送，但提交时必须解析到与该帧 Ti 相容的值。
运行时可以利用有界延迟窗口等待几何并批量处理，输出仍保持既定连续节奏。
缺失/迟到几何的行为待定，不默认使用旧 Ti 或合成占位相机。现有模拟（含跟踪 worker，
见 [Tracking worker](../server/docs/tracking-worker.md)）在等待窗口内拿不到几何时跳过该帧、不渲染，输出口重复上一帧；
这仍是模拟行为，不是正式播出的决定。

## Frame storage and lifetime

建议使用预分配的有界帧池。实现可以是多个二维纹理，也可以是 `texture_2d_array`；
纹理数组要求相容的尺寸和格式，但不应成为公共协议的固定存储形式。

| Concept | Invariant |
| --- | --- |
| `FrameKey` | 源帧身份，不随存储位置或输出顺序变化。 |
| `image_ref` | 当前画面资源的引用，不能误读为已复用 slot 中的新画面。 |
| Slot / layer | 可复用存储位置，不是源帧身份或跨进程可直接使用的资源标识。 |
| Readiness dependency | 消费者读取前必须满足生产者写入依赖；不强制 CPU 每帧阻塞等待。 |
| Lifetime | 所有引用者及相关 GPU 操作结束后才允许复用，不以提交成功作为释放条件。 |

可采用 opaque handle 或 slot 加 generation 来防止过期引用，具体编码尚未选定。
模拟实现可用非 GPU 存储，但要保留同样的身份、就绪和复用语义。
GPU 写入就绪不代表图像对应的 tracking 结果也已就绪。

默认分离输入与输出资源，不原地覆盖输入，以免破坏 tracking、录制等消费者。
帧池容量需覆盖在途工作和消费者持有量，不能直接把 25 帧延迟当作 25 个 slot 的充分容量。
池满时的等待、拒绝或丢弃策略必须明确，不能静默覆盖仍在使用的帧。

## Scene binding

渲染隐式使用当前 scene，但“当前”必须有一致取值点。
候选方式是在处理渲染提交时绑定一次 scene 快照，并记录 `scene_revision`。
该提交执行期间的后续 scene 修改不改变已绑定快照。
单一 scene 版本的批次可以共用该快照，但每帧仍按自己的逻辑时间求值动画。

如果后续支持精确逻辑位置的 scene 切换，跨越切换边界的批次需拆分或分别绑定版本；
不能因批次大小不同而改变已经约定的帧生效边界。
候选快照机制不要求调用者逐帧传 scene，也不意味着需要公开任意历史 scene 查询接口。

## Errors

需要明确过期 `image_ref`、帧身份不匹配、图像规格不相容、无效或不相容 Ti/C、
资源耗尽以及写入/渲染失败的处理。
错误不能以读取错误 slot、覆盖在用帧或默认几何来掩盖。
具体错误码、任务失败结果、池满和 deadline 策略尚未冻结。

## Simulation acceptance

- 打乱图像和 Ti 的到达顺序，仍按源帧正确关联。
- 模拟 slot 复用，旧引用不能读到新画面；有消费者持有时不能覆盖。
- 模拟图像仍在写入，渲染须满足依赖后读取。
- 渲染提交后修改 scene，已绑定提交不出现新旧状态混合。
- 改变批次大小仍保留已约定的 scene 切换边界和帧关联。
- 在没有 atlas、畸变和真实场景深度输入的条件下验证首期路径。

## Open decisions

- 公开方法名、上下文身份、提交/完成结果和字段级 schema。
- 帧引用及就绪条件的表示、数据进入方式和本地进程边界。
- scene 快照的精确绑定时机、修改顺序和回执。
- 帧池容量、消费者释放、池满、取消及超时策略。
- 批量提交的部分失败策略和输出画面的生命周期。

## Concrete initial profile

[Initial scene and frame control contract](scene-frame-contract.md) supplies the
initial #12 control/submission schemas, resource/effect boundaries and synthetic
conformance scenarios. Its method/type/lifecycle profile is now explicitly
approved for #20/#21 synthetic implementation with registered resources and a
trusted local controller. That limited approval does not resolve the broader
production questions above or approve #63. Existing renderer features remain
separate from runtime control and actual program-output acceptance.

The opt-in [tracked camera simulation](../server/docs/sdi-simulation.md) adds an
optional per-frame `lens` and `frame_warp` to `ti_c` geometry.
It does not change the pinhole initial profile above.
