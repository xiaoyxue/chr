# Cam1 input recording

**Status:** `draft` — Cam1 术语和产物边界为 `confirmed`；方法、payload 和状态为 `proposed`。

**Scope:** 为静态场景建图/标定录制主相机输入，不包含延迟后的渲染输出录制。

**Depends on:** [Protocol conventions](protocol-conventions.md)、[Execution model](execution-model.md)。

## Confirmed decisions

- 球场主相机的输入 SDI 流称为来自 `Cam1` 的 SDI 流。
- 录制产物是 **MP4 + 配套逐帧元数据，作为一个整体交付**。
  元数据编码、文件命名、目录组织及是否打包成归档尚未确定。

## Methods

| Method | Params | Result | Successful effect |
| --- | --- | --- | --- |
| `recording.start` | `RecordingStartParams` | `RecordingReference` | 已创建并受理录制任务，不承诺取得首帧或完成文件。 |
| `recording.stop` | `RecordingTargetParams` | `RecordingReference` | 已受理停止意图，不承诺达到停止边界或完成收尾。 |
| `recording.get` | `RecordingTargetParams` | `RecordingSnapshot` | 返回读取时的任务快照。 |

`Cam1` 是逻辑来源，不是采集卡端口或某次采集的唯一身份。任务应绑定实际输入流实例。
停止和查询针对 `recording_id`，不能笼统停止 Cam1 的全部活动。
同源并发录制、重试后重复建立任务、对已终止任务再次 stop 的行为仍待确认。

## Effects and state

| State | Meaning |
| --- | --- |
| `preparing` | 已建立任务身份，正在准备资源；尚未承诺录到画面。 |
| `recording` | 已接收用于该录制的帧，保留其源身份。 |
| `finalizing` | 已确定停止边界，编码器、容器和元数据仍在完成写入。 |
| `completed` | 视频与逐帧元数据均已完成并具备一致关联，可整体交付。 |
| `failed` | 报告原因和已知未完成产物，不能以文件存在代替成功。 |

候选约定：人工停止与达到最大时长使用同一收尾流程，并报告不同停止原因；
停止意图被受理不代表产物已完成。失败前取得的部分数据是否可用，应与任务成功分开表达。

第一版可以从“准备完成后的下一个可采集帧开始”讨论，暂不预设回录缓存。
必须记录实际解析到的源帧和逻辑边界，才能重放同一录制范围；
只记录操作到达时间不足以复现准备完成的位置。
精确触发、准备中收到 stop 的处理、状态转换和断线行为仍待进一步确定。

## Payload schemas

### Params

```typescript
type RecordingStartParams = {
  source: string;
  output_path: string;
};

type RecordingTargetParams = {
  recording_id: string;
};
```

`source` 是已配置的输入名称，本轮使用 `Cam1`。
`output_path` 暂以执行机器上的绝对 MP4 路径表达，禁止隐式覆盖是候选约束。
最终是否改为产物目录或其他目标描述，随文件组织方式一起确认。
所有 ID、输入名称和路径字符串均非空。

### Results

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

`recording_id` 引用同一任务时保持不变。
`video_path` 与 `frame_metadata_path` 均为执行机器上的非空绝对路径；
前者指向 MP4，后者指向其逐帧元数据。这里只定义两个角色，不固定文件后缀或编码。
`completed` 必须同时提供两者，不能用单个 MP4 路径声称整体已完成。

`partial_paths` 是已知残留文件的绝对路径列表，允许为空，不是可交付产物列表。
`failed` 不携带 `artifacts`，不承诺残留文件完整或可用。
失败类型集合为候选，明确输入中断、超时等策略后需同步修订。

首末帧、生效逻辑时刻、停止原因和统计信息在相关语义明确后加入；
当前 schema 不宣称这些已成为 response 字段。

## Errors

以下业务拒绝码及 data 是候选定义，不用于替代运行中的 `RecordingFailure`：

| Code | Message | Data schema | Methods |
| --- | --- | --- | --- |
| `1001` | `Recording not found` | `{ "recording_id": string }` | `recording.stop`, `recording.get` |
| `1002` | `Source unavailable` | `{ "source": string }` | `recording.start` |
| `1003` | `Output unavailable` | `{ "path": string }` | `recording.start` |
| `1004` | `Recording resources unavailable` | `{ "source": string }` | `recording.start` |

上述四种错误的 data 必填，其字符串字段非空。`1003` 不等于允许覆盖或自动修复路径。
资源在受理后才失败时，报告任务 `failed`。查询一个已失败任务得到的是
`RecordingSnapshot`，查询不存在的任务则是 `1001`。

## Payload examples

### recording.start params

```json
{
  "source": "Cam1",
  "output_path": "/recordings/cam1-scan.mp4"
}
```

### recording.start / recording.stop result

```json
{
  "recording_id": "rec-001"
}
```

### recording.get params

```json
{
  "recording_id": "rec-001"
}
```

### recording.get completed result

`.metadata` 只是示例名称，不指定编码：

```json
{
  "recording_id": "rec-001",
  "state": "completed",
  "artifacts": {
    "video_path": "/recordings/cam1-scan.mp4",
    "frame_metadata_path": "/recordings/cam1-scan.metadata"
  }
}
```

### recording.get failed-task result

```json
{
  "recording_id": "rec-001",
  "state": "failed",
  "failure": {
    "kind": "storage_failed",
    "message": "Recording storage write failed"
  },
  "partial_paths": ["/recordings/cam1-scan.mp4"]
}
```

### recording.get not-found error

```json
{
  "code": 1001,
  "message": "Recording not found",
  "data": {
    "recording_id": "rec-missing"
  }
}
```

## Recording time and metadata

内容最大时长按选定的逻辑时间线表达；无输入、写入等运行保护时限单独定义。
示例暂不包含时长参数，等待时间编码、起止点和时间线推进语义明确后补充。
MP4 只确定容器；高清晰录制还需明确编码、位深、色度采样、分辨率、帧率和扫描模式。

以下是逐帧元数据的候选内容，列名是语义标签，不是冻结的字段名：

| Information | Meaning and constraints |
| --- | --- |
| Source frame identity | 关联输入流实例和源帧，不以时间码、缓存槽或文件帧序号替代。 |
| File presentation frame / PTS | 将文件画面关联回输入，编码重排时不能以编码包序号替代呈现帧。 |
| Capture stream time | 采集接口提供的帧时间与持续时间，带明确 time base。 |
| Hardware reference time | 设备时钟域内的采集时间，不天然等于 UTC 或主机绝对时间。 |
| SDI timecode | 保留可获得的时间码及必要标志，允许显式缺失、重复、跳变或回绕。 |
| Continuity and availability | 记录可检测的不连续和缺失，不伪造未取得的时间信息或声称检测到了全部丢帧。 |

逻辑时间线与采集时间、timecode 等来源信息分开记录，必要时保存明确映射。
MP4 的 PTS 可以有自己的起点，但必须保留其与源帧和采集时间的映射。
丢帧、重复帧以及去隔行等变换需明确，不能隐式假设文件帧与输入帧一一对应。

保留解析后的 timecode 与归档全部可获取的 ANC 原始包是不同需求。
设备、信号及采集路径决定能取得什么；缺失是有效状态，不用生成值冒充采集值。
MOV/MP4 可承载时间码轨，但不能仅据此承诺所有逐帧 ANC、跳变和原始身份都被无损保存。

## Simulation acceptance

以下为候选业务验证场景，不代表已实现测试：

- 固定模拟输入及逻辑起止点，改变推进速度后产物仍对应相同源帧范围。
- 模拟 stop 被受理但仍在收尾，确认 `finalizing` 不被当成 `completed`。
- 模拟视频或元数据写入失败，确认不返回完整 `artifacts`，并保留明确失败状态。
- 成功录制同时交付两类产物；带不连续或缺失时间信息的输入不被静默补造为连续记录。

## Open decisions

- 时间线、逻辑起止边界、最大时长及模拟重放记录。
- 产物组织、输出目标参数、完成或部分损坏的判定。
- 同源并发录制、重复操作、暂停、回录与输入重连的行为。
- 高清晰规格、音频及录制与直播之间的资源关系。
- 输入无信号、格式改变、写入失败、磁盘不足与超时的具体策略。
- 元数据保存范围、缺失时间信息时是否仍允许完成标定录制。

## References

以下以 DeckLink 为接口例子，不代表已经选定采集卡或 SDK：

- [Capture stream and hardware time](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinkvideoinputframe.html)。
- [Timecode and flags](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinktimecode.html)。
- [Frame ANC packets](https://sdk-doc.blackmagicdesign.com/decklink-sdk/DeckLinkAPI/interfaces/idecklinkvideoframeancillarypackets.html)。
- [FFmpeg MOV/MP4 muxer](https://ffmpeg.org/ffmpeg-formats.html#MOV_002fMPEG_002d4_002fISOMBFF-muxers)。
