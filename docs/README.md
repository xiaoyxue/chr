# Documentation guide

**Status:** `draft` — 产品与协议讨论持续完善；不代表所有设计已经实现。

**Scope:** 文档导航、内容归属和编辑格式。开发环境及渲染实现契约保留各自的权威入口。

## Document map

| Layer | Document | Owns |
| --- | --- | --- |
| Development | [Development environment](development.md) | 已有目录职责、工具链和开发命令。 |
| Product | [Business workflow](business-workflow.md) | 四阶段业务流程、参与方、交接和阶段出口。 |
| Architecture | [System architecture](system-architecture.md) | 产品边界、逻辑模块、数据流及现有实现与目标的区别。 |
| Responsibilities | [Component responsibilities](component-responsibilities.md) | 各功能的操作、配置、执行和反馈归属。 |
| Shared protocol | [Protocol conventions](protocol-conventions.md) | 所有方法共用的消息封装、标识、错误、schema 记法及完整消息示例。 |
| Execution | [Execution model](execution-model.md) | Effect、生效位置、逻辑时间、批处理和模拟重放。 |
| Business methods | [Cam1 input recording](recording-control.md) | 录制 params、result、state、errors、产物及业务验证场景。 |
| Business methods | [Calibration records](calibration-control.md) | atlas/Ti/C 记录及 create/get/load 的核心职责。 |
| Frame pipeline | [Frame ingestion and rendering](render-frame-input.md) | 首期 pinhole 范围、scene 上下文、帧发布/渲染提交与资源生命周期。 |
| Discussion | [Open decisions](open-decisions.md) | 尚未解决的问题索引，不复制各文档已确认的决定。 |
| Renderer contract | [Coordinate contract](../server/docs/coordinates.md) | 已实现的坐标与投影契约。 |

业务讨论按 Product → Architecture → Responsibilities 展开；方法设计使用
Shared protocol 和 Execution 的共同基础，再在 Business methods 中逐项细化。
开发和渲染文档独立维护，不为统一讨论文档外观而重复或改写其约束。

## Ownership rules

每项约定有一个主要维护位置，其他文档通过链接使用：

- 通用封装只在 `protocol-conventions.md` 定义。具体方法默认遵守，不重复讲解版本、
  request ID、notification 或标准错误；方法示例只展示业务 payload。
- 跨方法的时间和 effect 规则放在 `execution-model.md`。业务页只补充自己的边界与场景。
- 产品目标不等于代码现状。目录和已实现能力引用开发及模块文档，不把提案写成已有功能。
- 已确认决定保留在负责该主题的文档，`open-decisions.md` 只跟踪仍待解决的问题。
- 私有 `reference/` 不是可发布文档素材，不引入其资产或实验数据。

## Document format

本组设计文档使用英文标题与章节名，解释性正文可用中文；
协议标识的语言和命名规则以通用协议文档为准。
文档开头依次给出 `Status`、`Scope`，有前置文档时列出 `Depends on`。

| Label | Meaning |
| --- | --- |
| `draft` | 文档仍在演进，不代表业务 schema 已冻结。 |
| `confirmed` | 已明确确认的决定，写在对应主题的 `Confirmed decisions` 或明确标注的段落中。 |
| `proposed` | 供评审的具体候选，不能作为已获批准或已实现的依据。 |
| `open` | 尚未决定；不能以默认值或实现便利隐式替代。 |
| `implemented` | 必须有当前代码或实现契约依据，与产品目标分别表达。 |

业务方法文档依次组织为 `Scope`（在文首）、`Confirmed decisions`、`Methods`、
`Effects and state`、`Payload schemas`、`Errors`、`Payload examples`、
业务数据/时间约束、`Simulation acceptance`、`Open decisions`、`References`。
没有对应内容的章节可以省略，不添加占位框架。
每个方法必须列出 params、result 和成功 effect；错误与异步失败分开说明。
补全核心指令集合时，可以先声明逻辑输入、结果和 effect，但必须显式标注字段级 schema
尚未完成；不能将该阶段的清单当作可实现的完整接口。之后再逐个专题细化。

## Design references

历史业务输入是赛前内容准备、场地扫描与校准、叠加测试、赛中直播四阶段流程。
Fusion 的现阶段定位见产品架构，不再复制历史远端部署假设。

[Hong-Xiang/vchao#150](https://github.com/Hong-Xiang/vchao/pull/150)
是历史讨论参考，不是 chr 的最终协议，也不覆盖本仓库的现有实现契约。
[PR #4](https://github.com/xiaoyxue/chr/pull/4) 单独讨论渲染服务部署与输出设计；
其未合并提案不在这里自动升级为已确认决定。已经落地的目录、IPC 和服务端能力
以当前实现及其契约为准，不因该设计 PR 尚未合并而视为待定。

本组设计由 [tracking issue #1](https://github.com/xiaoyxue/chr/issues/1) 跟踪，
[draft PR #2](https://github.com/xiaoyxue/chr/pull/2) 持续记录讨论结果。
