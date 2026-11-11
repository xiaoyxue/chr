# Calibration records

**Status:** `draft` — 标定产物及 load 即启用的业务方向已明确；任务状态、字段编码和错误细节为 `proposed`。

**Scope:** 扫描录像到机位标定记录的生成、查询和加载，不定义建模或 tracking 算法。

**Depends on:** [Protocol conventions](protocol-conventions.md)、[Execution model](execution-model.md)、
[Cam1 input recording](recording-control.md)、[Coordinate contract](../render/docs/coordinates.md)。

## Confirmed decisions

对外的完整 calibration 是某个机位在指定球场坐标系下的静态标定记录：

- `atlas` 是参考图像集合，每张图都带有已完成球场注册的 `Ti`，
  即球场地面 XY 到该参考图 SNC 的映射，不是尚未注册的任意拼接图。
- 光心 `C` 位于同一个球场世界坐标系中。
- `calibration_id` 标识记录，`camera_id` 标识对应机位。
- 图像尺寸、图像域及球场坐标上下文必须可确定，不能仅凭 ID 猜测。

特征提取、索引、匹配策略和缓存属于建模/tracking 内部，不作为公共业务字段暴露。
内部可保存额外算法数据，但其布局不成为渲染接口的依赖。
本阶段使用 pinhole 模型，不加入畸变参数。

`calibration.load` 读取并启用已有完整记录，不引入单独的 `calibration.activate`，
也不建模“多份已 load 但尚未 active”的集合。磁盘上可以存在多份历史记录。

## Methods

以下限定逻辑输入、输出与 effect，不宣称字段级 schema 已冻结。

| Method | Params, logical | Result, logical | Successful effect |
| --- | --- | --- | --- |
| `calibration.create` | 扫描视频及可用的配套元数据引用、`camera_id`、可解析的球场坐标上下文。 | 新记录的 `calibration_id`。 | 已受理生成任务，不表示建模完成。 |
| `calibration.get` | `calibration_id`。 | 任务/记录状态，完成时提供标定产物，失败时提供原因。 | 返回记录快照。 |
| `calibration.load` | 已完成记录的引用及对应机位绑定上下文。 | 当前使用的 `calibration_id` 和 `camera_id`。 | 记录已读取并成为该机位当前使用的标定。 |

视频通过路径或资产引用交付的具体方式待定，不在这里规定视频字节的传输方式。
球场尺寸和坐标上下文若已经由工作上下文绑定，可以不在每次 create 中重复传入。
同样，load 的机位可从记录解析，但必须检查与目标输入的绑定相容。

## Effects and state

候选执行语义：

- create 受理时就分配稳定的 `calibration_id`，以同一 ID 查询进度和最终产物，
  暂不额外引入独立的任务 ID。create 只生成记录，不隐式替换当前使用的标定。
- get 区分生成中、完整记录和失败；精确状态值、进度及错误 schema 后续定义。
- load 成功结果表示切换已生效，而不是只确认读文件请求已入队。
  读取或验证失败不破坏旧记录；切换过程中不能混用新旧坐标、Ti 或 C。
- 内部准备和资源切换可以异步执行，但不因此增加公开的 prepare/activate 工作流。

对已在途帧的版本归属、load 的具体生效边界和重复调用行为仍待细化。
完整产物无法得到时必须报告未完成或失败，不能伪造 Ti 或 C。
本轮不另加 ground-only 的成功结果变体。

## Result contents

| Item | Required meaning |
| --- | --- |
| `calibration_id` | 当前任务及其生成记录的稳定身份。 |
| `camera_id` | 记录对应的机位身份，不替代输入流实例身份。 |
| `atlas` | 图像与各自 Ti 的明确配对；图像引用、尺寸及 SNC 图像域可解析。 |
| `C` | 与全部 Ti 和球场定义相容的世界坐标光心。 |
| World context | 球场坐标定义、尺寸及必要版本关联，可直接保存或引用。 |

此表是逻辑结果合同，不固定图片容器、矩阵数字编码、记录目录或元数据格式。
create/load 的引用结果和 get 的状态联合 payload 需在下一轮字段级设计中补齐；
不能把此表直接视为已经可实现的完整 schema。

## Tracking and rendering boundary

```text
Scan recording → calibration.create → Calibration record
Calibration record → calibration.load → Tracking reference context
Input frame + Tracking reference context → Per-frame Ti
Input image reference + Ti + compatible C → Rendering
```

tracking 使用参考图与内部匹配资源；渲染核心只需要当帧图像和已解析的 Ti/C，
不必加载 atlas 或接受 `calibration_id` 作为必需输入。
C 可以由运行时从当前机位配置取得，但必须与该帧 Ti 使用的参考系和版本相容。
静态记录可作为来源信息，不能代替当帧输入或成为隐式的图像匹配步骤。

## Errors

需区分视频不可读取、机位/球场上下文不相容、建模无法形成完整结果、
记录不存在或不完整、加载资源失败等情况。
任务生成失败与 create/get/load 操作被拒绝分别表达；具体错误码及 data 尚未分配。

## Simulation acceptance

- 使用合成视频和合成标定结果验证 create/get/load，不依赖真实 SDI 或特定建模算法。
- 检查每张参考图与 Ti 配对，并拒绝参考系不相容的结果。
- 模拟 load 失败，旧记录继续有效；成功切换不让同一帧混用新旧参数。
- 直接提供匹配的图像、Ti 和 C，可在没有 atlas 的条件下驱动渲染。

## Open decisions

- 视频/记录引用、球场上下文和机位绑定的精确表示。
- 任务状态、进度、错误码及完整 response payload schema。
- 记录存储、版本、重试、取消及恢复行为。
- load 生效边界、在途帧归属和坐标系现场修正。
