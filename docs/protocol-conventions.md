# Protocol conventions

**Status:** `draft` — JSON-RPC 2.0 和英文协议标识为 `confirmed`；业务 schema 和部分细节仍待确认。

**Scope:** 所有 RPC 方法共用的消息、ID、错误、命名和 schema 规则，不限定来源或实现模块。

## Scope and naming

- RPC 消息使用 [JSON-RPC 2.0](https://www.jsonrpc.org/specification)，不另造
  `command`/`command_id` 外层。协议版本与业务 schema 版本分开。
- 协议字段名、方法名、状态值、枚举值、错误标识和 schema 类型名使用英文；
  协议中的 `message` 使用英文说明，客户端可根据稳定标识本地化展示。
  这不限制用户提供的名称、文件路径等业务字符串的语言。
- 业务方法使用 `<namespace>.<command_name>`；`rpc.` 是标准保留前缀。
  新字段、状态与枚举沿用 `snake_case`，文档类型名使用 `PascalCase`。
- 中文可以用于解释语义，但不是 wire value。例如录制中编码为 `recording`，
  不编码为中文描述，也不允许客户端依赖显示文案分支。

这个决定不要求视频、逐帧几何或批量媒体数据使用 JSON，也不选择 HTTP、WebSocket、
WebRTC、本地 socket 或实现库。传输上的消息分帧、连接与订阅机制后续确定。

## Schema notation

下列 TypeScript 风格类型是文档中的结构 schema，不要求实现使用 TypeScript。
成员默认必填，`?` 表示可省略，`never` 表示禁止出现，`|` 表示互斥变体。
类型旁的约束同样属于 schema；实现阶段仍需边界验证，不能只依赖静态类型。
业务页使用同样的记法，并列出 params、result、errors 和成功 effect。
除显式声明的扩展点外，额外业务字段不属于当前草案；扩展必须同步更新 schema。
字符串是否允许为空、路径所属机器等约束由业务页明确。

## Request, response and notification

```typescript
type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

type RpcId = string | number | null;
type RpcParams = JsonValue[] | { [key: string]: JsonValue };

type RpcRequest<P extends RpcParams> = {
  jsonrpc: "2.0";
  id: RpcId;
  method: string;
  params?: P;
};

type RpcNotification<P extends RpcParams> = {
  jsonrpc: "2.0";
  method: string;
  params?: P;
  id?: never;
};

type RpcSuccess<R extends JsonValue> = {
  jsonrpc: "2.0";
  id: RpcId;
  result: R;
  error?: never;
};

type RpcFailure<D extends JsonValue> = {
  jsonrpc: "2.0";
  id: RpcId;
  error: {
    code: number;
    message: string;
    data?: D;
  };
  result?: never;
};

type RpcResponse<R extends JsonValue, D extends JsonValue> =
  | RpcSuccess<R>
  | RpcFailure<D>;
```

| Member | Constraint |
| --- | --- |
| `jsonrpc` | 必须为 `"2.0"`。 |
| `id` | 响应原样匹配请求 ID；无法识别请求 ID 的错误使用 `null`。 |
| `params` | 标准允许对象或数组，也允许省略；具体方法进一步规定形状和是否必填。 |
| `result` | 成功响应必填，严格使用该方法声明的 result schema。 |
| `error` | 失败响应必填，不能与 `result` 同时出现。 |
| `error.code` | 必须为整数，不能违反 JSON-RPC 保留码范围。 |
| `error.message` | 简短英文说明；程序不得以文案替代稳定错误码/标识。 |
| `error.data` | 可选的结构化细节；具体方法必须说明其支持的 schema。 |

`RpcId` 反映标准允许的类型，不承诺每个客户端支持任意精度数值。
标准不推荐 `null` 或小数 ID；本仓库示例使用字符串，是否进一步限制请求 ID 类型仍待定。
JSON 不接受 `NaN` 或无穷值；大整数与时间 tick 的精确编码需在业务 schema 中明确。

notification 没有响应，包括参数错误；需要确认的操作应使用 request。
RPC `id` 仅关联请求与响应，不提供去重、任务身份、执行一次或重启恢复保证。
需要安全重试时由业务定义稳定操作身份及重复调用行为。

标准错误包括 `-32700` (`Parse error`)、`-32600` (`Invalid Request`)、
`-32601` (`Method not found`)、`-32602` (`Invalid params`) 和 `-32603` (`Internal error`)。
其余业务错误在具体方法中定义，不能覆盖标准保留用途。
业务错误码分配需要跨 namespace 检查冲突；当前候选码只在对应业务页维护，
最终分配和版本规则仍待确定。

## Result payloads and asynchronous effects

每个方法必须分别声明 params、result 和 error data，不能只写“返回状态”或给一个
无约束的 payload。参数无效或请求被拒绝使用 `RpcFailure`，不额外发明通用
`success`/`message` 外层，也不把失败伪装成正常 result。

长任务可以在建立任务后返回，但方法必须明确成功确认的是哪个 effect。
任务运行失败与 RPC 调用失败分开：查询一个已失败任务可以成功返回带
`state: "failed"` 的业务快照，这不是失败伪装成功，而是查询本身成功。
一个普通 request 只有一次 response，后续状态通过查询或业务事件获取。
等待 response 超时不自动取消已经发起的业务任务。

状态枚举由具体业务定义，不要求所有模块共用同一个 state machine。
错误、状态与事件不能只靠自然语言文字区分；缺失、空集合与 `null` 也应明确语义。
具体 result schema 的候选示例见 [Cam1 input recording](recording-control.md)。

## Message examples

完整消息示例仅放在本页；方法文档只展示 params、result 或业务 error payload。
这里引用 `recording.get` 说明封装，不在此重新定义录制状态。

Request:

```json
{
  "jsonrpc": "2.0",
  "id": "req-001",
  "method": "recording.get",
  "params": {
    "recording_id": "rec-001"
  }
}
```

Success response:

```json
{
  "jsonrpc": "2.0",
  "id": "req-001",
  "result": {
    "recording_id": "rec-001",
    "state": "preparing"
  }
}
```

Invalid-params response（与上面的成功 response 是互斥示例）:

```json
{
  "jsonrpc": "2.0",
  "id": "req-001",
  "error": {
    "code": -32602,
    "message": "Invalid params"
  }
}
```

## Batch and transport boundaries

JSON-RPC batch 不保证执行顺序、原子性或统一生效时刻；response 顺序也可能不同。
它不等于帧批处理协议。需要原子修改或逻辑时刻生效时，应通过业务方法明确约定。

JSON-RPC 本身不提供事件持久化、订阅或重放。重连后的状态恢复、认证与传输细节
需要各自明确，不能从使用标准 envelope 推导出不存在的保证。
控制 effect、逻辑时间与模拟约束见 [Execution model](execution-model.md)。

## Open decisions

- 是否将请求 ID 限定为字符串，以及业务 schema 的版本和演进方式。
- 跨 namespace 的业务错误码分配。
- 各传输适配的连接、消息分帧、订阅、认证和重连机制。
- 业务操作身份、去重范围和重启持久性；不从标准 ID 推导。
