# 静态场景标定：Cam1 输入录制

状态：录制产物边界、Cam1 术语及 JSON-RPC 2.0 消息规范已确认；
具体方法、参数、effect 和时间记录细节为候选方案。
本文先讨论与发起者及传输方式无关的业务协议，不是已实现接口或冻结的 wire schema。

## 已确认的讨论基础

- 球场主相机的输入 SDI 流称为来自 `Cam1` 的 SDI 流。
- 控制可以由 WebUI、CLI 或固定 trigger 发起，不把业务命令绑定到某一种来源。
- 用于静态场景建图/标定的录制产物是 **MP4 + 配套逐帧元数据，作为一个整体交付**。
  元数据编码、文件命名、目录组织以及是否打包成归档尚未确定。
- 先定义业务操作与 effect，再细化模块接口。运行时组织录制的总体职责保持不变。
- 控制消息采用 JSON-RPC 2.0；业务需支持模拟与重放，不依赖当前系统墙钟。
  通用约束见 [Protocol conventions](protocol-conventions.md)，
  时间与 effect 约束见 [Control effects and simulation](control-protocol.md)。

输入录制与延迟后的渲染输出录制是不同对象。本轮讨论 Cam1 输入录制，
不以实时渲染的输出节奏或输出时间戳替代原始输入身份。

## 候选命令

采用 `<namespace>.<command_name>` 命名，namespace 表达业务能力，
具体目标作为参数传入，而不是把调用者或设备端口写入命令名。

| 候选命令 | 意图 |
| --- | --- |
| `recording.start` | 创建并启动一次录制任务。 |
| `recording.stop` | 停止指定录制并完成文件收尾。 |
| `recording.get` | 查询录制状态及结果；查询本身不改变录制状态。 |

以下采用标准 JSON-RPC 2.0 封装；业务方法及参数仍是示例，不冻结输出路径表达：

```json
{
  "jsonrpc": "2.0",
  "id": "req-001",
  "method": "recording.start",
  "params": {
    "source": "Cam1",
    "output_path": "/recordings/cam1-scan.mp4"
  }
}
```

候选语义：

- JSON-RPC `id` 只关联请求与响应，不提供去重保证。安全重试所需的稳定业务身份、
  重复参数冲突、去重范围、保留时间及重启后的行为仍需定义。
- `Cam1` 是逻辑来源，不是采集卡端口或某次采集的唯一身份。任务应绑定实际输入流实例；
  断线重连、换源和输入格式变化如何处理仍待决。
- 返回独立的 `recording_id`；停止和查询针对该任务，而不是笼统停止 Cam1 的全部活动。
- 路径属于执行录制的机器。建议使用绝对路径并禁止隐式覆盖。
  产物已确定包含元数据，因此最终参数是 MP4 路径、目录还是其他目标描述仍需讨论。
- 录制内容的最大时长按选定的逻辑时间线表达，不以处理机器的执行速度决定。
  无输入、写入等运行保护时限单独定义，不复用内容时长。
  逻辑起止点、是否必填、缺省行为、时间线推进与精确帧数的关系尚未确认。

这取代早期将最大录制时长优先解释为机器单调时钟安全上限的候选方案。
示例暂不包含时长参数，等待逻辑时间编码与边界语义细化后再补充。

MP4 只确定容器，不确定编码质量。录制规格还需明确编码、位深、色度采样、
分辨率、帧率、扫描模式及可能的转换；不默认选择某个编码器或静默降低质量。

## 候选 effect 与结果语义

`start` 的 effect 是启动一个有生命周期的录制任务，不是立即返回完成的视频。
采用建立任务后返回的候选方式时，JSON-RPC `result` 表达任务建立结果，
后续完成/失败通过任务状态及可能的事件报告，不重复回复同一个调用。
建议区分以下可观察阶段，具体状态机与事件名留待进一步讨论：

| State | 可观察含义 |
| --- | --- |
| `preparing` | 已建立任务身份，正在准备资源；尚未承诺录到画面。 |
| `recording` | 已接收用于该录制的帧；源帧身份需要保留在录制记录中。 |
| `finalizing` | 已确定停止边界，编码器、容器和元数据仍在完成写入。 |
| `completed` | 视频与配套元数据均已完成并具备一致关联，可整体交付。 |
| `failed` | 报告原因及是否存在未完成产物，不能以文件存在代替成功。 |

候选约定：人工停止与达到最大时长使用同一收尾流程，并报告不同停止原因；
停止指令受理不代表产物已完成。失败前取得的部分数据是否可用，应与任务成功分开表达。

第一版可以从“准备完成后的下一个可采集帧开始”讨论，暂不预设回录缓存。
精确帧触发、实际开始/停止边界、准备中收到 stop 的处理、重复 stop 及断线后的行为尚待确定。
这种相对生效方式需要记录实际解析到的源帧和逻辑边界，才能在模拟中重放同一录制范围；
仅记录 RPC 到达时间不足以复现准备完成的位置。

## Response payload schemas

以下是最小可评审的业务 schema 草案，不把尚未确认的时间编码塞入 response。
使用 [Protocol conventions](protocol-conventions.md) 中的结构 schema 记法。
除显式标注的成员外均为必填；此草案未定义额外业务字段，扩展需同步修订文档。
所有 ID 均为非空字符串，引用同一录制时保持不变。

```typescript
type RecordingReference = {
  recording_id: string;
};

type RecordingArtifacts = {
  video_path: string;
  frame_metadata_path: string;
};

type RecordingFailure = {
  kind:
    | "source_lost"
    | "source_changed"
    | "encoding_failed"
    | "storage_failed"
    | "internal_error";
  message: string;
};

type RecordingSnapshot = RecordingReference & (
  | { state: "preparing" }
  | { state: "recording" }
  | { state: "finalizing" }
  | {
      state: "completed";
      artifacts: RecordingArtifacts;
    }
  | {
      state: "failed";
      failure: RecordingFailure;
      partial_paths: string[];
    }
);
```

`video_path` 与 `frame_metadata_path` 均为执行机器上的非空绝对路径；
前者指向 MP4，后者指向其配套逐帧元数据，具体文件布局和元数据编码仍待确认。
这里只定义两个角色，不要求元数据采用某个后缀或序列化格式。
`completed` 必须同时提供两者，不能用单个 MP4 路径声称整体已完成。

`partial_paths` 是已知的残留文件路径列表，允许为空，不是可交付产物列表。
`failed` 不携带 `artifacts`，不承诺残留文件完整或可用；`failure.message` 使用英文。
失败类型集合为候选，明确输入中断、超时等策略后需同步修订。

| Method | Result schema | Successful effect |
| --- | --- | --- |
| `recording.start` | `RecordingReference` | 已创建并受理录制任务；不承诺已取得首帧或完成文件。 |
| `recording.stop` | `RecordingReference` | 已受理对该任务的停止意图；不承诺已经达到停止边界或完成收尾。 |
| `recording.get` | `RecordingSnapshot` | 返回读取时的任务快照；查到 `failed` 仍是查询成功。 |

`stop` 对已终止任务以及重复请求的处理仍待确认；不能从相同 result 结构推断幂等性。
首末帧、生效逻辑时刻、停止原因和统计信息后续在相关语义明确后加入，
当前 schema 不宣称这些已成为 response 字段。

### RPC errors

标准解析和参数错误遵循通用规范。以下业务拒绝码及 data 是候选注册，
仅描述调用被拒绝，不用于替代运行中的 `RecordingFailure`：

| Code | Message | Data schema | Methods |
| --- | --- | --- | --- |
| `1001` | `Recording not found` | `{ "recording_id": string }` | `recording.stop`, `recording.get` |
| `1002` | `Source unavailable` | `{ "source": string }` | `recording.start` |
| `1003` | `Output unavailable` | `{ "path": string }` | `recording.start` |
| `1004` | `Recording resources unavailable` | `{ "source": string }` | `recording.start` |

上述四种错误的 `data` 必填，其字符串字段非空。其他 namespace 定义错误时需避免
这些候选码冲突；最终分配方式尚未冻结。`1003` 不等于允许覆盖或自动修复路径。
资源在受理后才失败时，报告任务 `failed`，不为已完成的 start RPC 追加错误 response。

### Response examples

`recording.start` 的任务受理结果：

```json
{
  "jsonrpc": "2.0",
  "id": "req-001",
  "result": {
    "recording_id": "rec-001"
  }
}
```

`recording.get` 查询到已完成产物；`.metadata` 只是示例名称，不指定编码：

```json
{
  "jsonrpc": "2.0",
  "id": "req-002",
  "result": {
    "recording_id": "rec-001",
    "state": "completed",
    "artifacts": {
      "video_path": "/recordings/cam1-scan.mp4",
      "frame_metadata_path": "/recordings/cam1-scan.metadata"
    }
  }
}
```

`recording.get` 查询到一个运行失败的任务：

```json
{
  "jsonrpc": "2.0",
  "id": "req-003",
  "result": {
    "recording_id": "rec-001",
    "state": "failed",
    "failure": {
      "kind": "storage_failed",
      "message": "Recording storage write failed"
    },
    "partial_paths": ["/recordings/cam1-scan.mp4"]
  }
}
```

`recording.get` 查询的任务不存在，属于调用失败：

```json
{
  "jsonrpc": "2.0",
  "id": "req-004",
  "error": {
    "code": 1001,
    "message": "Recording not found",
    "data": {
      "recording_id": "rec-missing"
    }
  }
}
```

## 时间与逐帧元数据

以下是候选记录内容，不代表已经确认保存全部 ANC 数据。

| 信息 | 含义与限制 |
| --- | --- |
| 原始帧身份 | 关联输入流实例和源帧；不能用时间码、缓存槽或文件帧序号替代。 |
| 文件呈现帧与 PTS | 将文件中的画面关联回输入；编码重排时不能用编码包序号替代呈现帧。 |
| 采集流时间 | 采集接口提供的帧时间与持续时间，带明确 time base。 |
| 硬件参考时间 | 设备时钟域内的采集时间，不天然等于 UTC 或主机绝对时间。 |
| SDI 嵌入 timecode | 保留可获得的时间码及必要的格式/标志信息；可能缺失、重复、跳变或回绕。 |
| 连续性与缺失状态 | 显式记录可检测的不连续和缺失；不能伪造未取得的时间信息或声称检测到了全部丢帧。 |

候选时间表示为整数 tick 加明确 time base/clock；具体整数编码未定。
逻辑时间线与采集时间、timecode 等来源信息分开记录，必要时保存明确映射；
不能把墙钟或硬件参考时间隐式作为可模拟的业务时钟。
MP4 的 PTS 可以有自己的起点，但必须保留其与源帧和采集时间的映射。
对文件帧与输入帧的一对一关系、丢帧、重复帧以及去隔行等变换应另行规定，不能隐式假设。

“保留解析后的 timecode”和“归档全部可获取的 ANC 原始包”是不同需求。
设备、信号及采集路径决定能取得什么；缺失是有效状态，不用生成的值冒充采集值。
MOV/MP4 可承载时间码轨，但不能仅据此承诺所有逐帧 ANC、跳变和原始身份都被无损保存。

## 下一步待决

- 录制时间线、逻辑起止边界及模拟重放记录，内容时长与运行保护的分离。
- 产物如何组织，输出目标参数如何表达，怎样判断整体完成或部分损坏。
- 开始/停止边界、最大时长、同源并发录制、暂停与回录是否支持。
- 高清晰录制规格、音频是否纳入，以及录制与直播的资源关系。
- 输入无信号、格式改变、写入失败、磁盘不足时的行为。
- 时间信息的实际用途、必要保存范围、缺失时是否仍允许完成标定录制。

## 技术依据

以下以 DeckLink 为接口例子，不代表已经选定采集卡或 SDK：

- [帧流时间与硬件参考时间](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinkvideoinputframe.html)。
- [时间码及其标志](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinktimecode.html)。
- [逐帧 ANC packet 接口](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinkvideoframeancillarypackets.html)。
- [FFmpeg MOV/MP4 muxer](https://ffmpeg.org/ffmpeg-formats.html#MOV_002fMPEG_002d4_002fISOMBFF-muxers)，包括 `write_tmcd`。
