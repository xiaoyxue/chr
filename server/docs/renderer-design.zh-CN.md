# 精简渲染器功能范围与设计

状态：**路线图；静态 GLB 和直接光 PBR 已实现**。本文对照本地 `trd` 的 `9c7bccc` 与 `chr`
的 `dcebb33`，提出更小的渲染器，而不是移植参考项目的整套应用。
英文配对文档为 [renderer-design.md](renderer-design.md)。

[coordinates.md](coordinates.md) 仍然是坐标契约。
除明确标注为现有能力的内容外，下文均为目标。

`chr_renderer::scene` 已提供共享索引 CPU 网格、按材质分组绑定、model matrix/显隐，
以及经过校验的 PBR 系数和直接光/环境光配置。
`Scene::snapshot` 返回拥有所有权的不可变 `SceneView`；后续修改保留旧资源，
并拒绝过期或跨 scene 的句柄。仍被当前实例引用的资源不能删除；
拒绝奇异 model matrix，隐藏使用显隐状态。
几何与材质值共享保存，不随每个实例更新而复制。
这是库级基础，不是直播控制协议：EXR/IBL、面光/大气以及 runtime 适配仍属后续工作。现有三角形路径不变。

`glb::import_glb` 已支持不访问文件系统的静态 BIN 内嵌 GLB 几何及 PNG/JPEG 贴图解码。
默认节点变换与 glTF 到 chr 的坐标轴旋转只烘焙一次；保留材质分组、法线、UV0、
tangent、顶点颜色及 sampler 元数据。缺少法线时采用三角形平面法线，
解码后的 RGBA8 图像在贴图/材质间共享。
骨骼、Morph、alpha blending、外部资源和未支持扩展会明确报错；
动画片段忽略并给出诊断。贴图数据由下述共享 PBR 路径使用。

`chr_renderer::camera` 已实现经过校验的 K/pose、FOV/look-at 和 Ti+C 针孔相机，
提供原生像素投影、像素到世界射线，以及包含度量 near/far 深度的列主序 WGSL clip 矩阵。
Ti+C 派生正确的光学坐标基，同时保留原始 Ti 系数。
这不包含镜头畸变，也不改变直播输入协议。

`MeshRenderer` 已提供共享的索引、不透明 unlit/深度基础路径。
网格在当前准备视图引用期间驻留 GPU，复用实例/材质分组 uniform 和深度目标。
M1 样例将合成球场和贴地、按面着色的方盒渲染到
`output/renderer/milestone-m1.png`。
在仓库根开发环境执行 `cargo run --locked -p chr-app --example mesh_milestone`。
该图用于展示相机投影、网格放置和遮挡，不是 PBR 或阴影效果，
也不等于 #40 的 Socket/WebSocket 交互验收。

`MeshRenderer::prepare_pbr` 已采用 GGX、height-correlated Smith 可见性及 Burley
漫反射进行金属度-粗糙度 PBR，绑定基础色、金属度/粗糙度、法线和自发光贴图，
支持导入或导数切线基、顶点颜色、alpha mask 和双面/镜像表面。
颜色纹理及 mipmap 在线性光照域过滤，数据贴图保持线性。
遮蔽贴图为间接照明保留，不用于压暗点状直接光；不添加隐藏环境补光或固定灯光组。

scene 的方向光/点光/聚光已参与无阴影的直接照明。
理想点源在精确奇异中心不产生贡献，其他位置采用平方反比衰减。
光栅粗糙度下限为 0.045，辐亮度限制在 RGBA16F 范围内，
再统一进行一次输出曝光及 Reinhard/ACES 变换。
IBL 尚未实现时显式拒绝环境照明；M2 不宣称阴影控制、面光或大气已完成。

M3 通过 `PbrSettings::shadows` 增加方向光/聚光的 `Hard` 与 `Pcf` 阴影图。
方向光按持久场景包围盒取景，不依赖观察相机；聚光图匹配光锥。
阴影与主渲染共享 alpha mask、模型变换和单双面规则。
接收平面深度修正、明确的深度/法线偏移及可配置 PCF 纹素半径处理采样伪影，
分配阴影图前检查纹理限制和可配置内存预算。

场景不变时，相机移动和 Hard/PCF 切换复用已有深度图。
M3 俯视对比保持普通 pinhole 取景、模型和太阳加四聚光照明相同，只改变 Off/Hard/PCF。
这些仍是仅供拥有者本地查看的诊断，不是完整交互验收。
本增量不包含级联、点光 cubemap、PCSS 或天空遮挡。

M4 在同一深度图上增加方向光/聚光的 `Pcss`。每盏方向光具有以弧度计的光源角半径，
每盏局部光具有以米计的阴影光源半径。shader 先把各遮挡物深度重建为线性光空间深度，
再求平均；太阳半影采用角尺寸与间距，聚光半影采用相似三角形，
最后按投影后的半径过滤比较结果。零尺寸严格等于 Hard，无阻挡时完全照亮；
点状 BRDF 求值和发射单位不变。

共享 `PcssSettings` 暴露每阶段 1..=64 次采样（默认 32+32）、
独立的搜索/过滤半径上限（各 64 个阴影纹素）。共享阴影预算限制每片元最坏工作量
（所有灯合计 1024 次逻辑采样）。无效质量或预算超限明确报错。
固定圆盘采样无需屏幕随机噪声或时间累积即可确定性复现，两个阶段都应用接收平面修正。
仅修改质量或移动相机复用深度图。大半径受上限限制，细小遮挡物可能漏采；
算法仍是单一平均遮挡深度近似。
`shadow-pcss.png` 保留 M3 照明，`shadow-pcss-wide.png` 将光源半径放大四倍而不改变能量。
级联、天空可见性及有限面光 BRDF 积分尚未实现。
PCSS 为高优先级，面光/LTC 现为低优先级。

算法参考 NVIDIA
[Soft Shadows sample](https://docs.nvidia.com/gameworks/content/gameworkslibrary/graphicssamples/opengl_samples/softshadowssample.htm)
及其 PCSS 集成资料。保留有界的两阶段策略，采用确定性圆盘采样和明确的世界/角度单位，
不虚构太阳距离；不把本地 `trd` 的接地 blob shader 复用为可见性。
GPU 回归将受控接触/间距/光源尺寸场景与独立的圆盘光源射线/平面可见性积分比较，
并覆盖零尺寸、无遮挡、近裁剪面、缓存和预算。这些夹具不是通用场景质量基准，
也不构成实时性能保证。

M5 将 Hard/PCF/PCSS 扩展到点光，每盏灯使用六个深度数组层，
顺序为 `+X,-X,+Y,-Y,+Z,-Z`。深度是径向距离除以按可见场景包围盒拟合的公共远半径。
投影物和可见性查询均排除 0.01 米近球内部；各面近裁剪平面在 cube 角部也包含该球面。
这不改变灯光的平方反比衰减。

点光核在方向/切空间采样，以 `2/resolution` 作为面中心纹素的角度尺度。
双线性邻居逐个跨面重映射，不截断在当前面；接收平面比较为实际读取纹素重建径向距离，
空的远深度采样保持照亮。PCSS 对径向遮挡距离求平均，
按最近场景包围盒半径保守收紧阻挡搜索，并沿用共享的采样数/半径限制。
标量半影仍是有限光源近似，不是形状相关 BRDF 积分；不使用本地参考项目的接地 blob。

合成 GPU 夹具使投影物经过六个面中心、十二条棱及八个角，
覆盖部分遮挡的过滤边缘、无遮挡/远边界、近球裁剪、alpha mask、镜像以及混合平面/cube 层。
点光接触硬化也与独立发光体可见性参考比较。
显存预算按每盏点光六层计算；M5 `point-*` 对比明确采用太阳加四点光、1024 分辨率
（25 层、100 MiB），M3/M4 `shadow-*` 保留太阳加四聚光和 2048 分辨率。
Hard/PCF/PCSS 切换及相机移动复用不变的深度图。
级联、广泛性能验收、天空/IBL 和面光/LTC 仍属后续工作。

M6 为三类灯光增加 `Light::shadow_mode: Option<ShadowMode>`。
`None` 继承帧默认模式；显式 Off 仅关闭阴影可见性，不关闭直接照明。
帧默认 Off 时，显式启用的模式仍生效。枚举放在 GPU 无关的 scene 状态中，
并保留 crate 根重导出。

分配和缓存标识包含实际启用阴影的灯集合；只有实际 PCSS 灯需要
PCSS 质量校验。默认过滤模式变化且启用集合不变时复用深度图，
编辑灯光覆盖模式则改变 scene snapshot，保守重建阴影图。
unlit 路径忽略覆盖模式。GPU 测试通过不同颜色的灯分离贡献，
自动验证继承/显式模式、关闭阴影后的资源节省、启用集合变化、预算和 unlit 行为，
不依赖人工看图验收。
`point-mixed.png` 保持太阳/四点光能量及 1024 分辨率不变，
采用太阳 PCSS，四角点光按 X/Y 顺序使用 Hard/PCF/PCSS/Off。
所需 19 层（76 MiB），关闭阴影的点光仍参与照明。
本增量不增加实时场景控制协议。

M7 支持明确的 PCF 质量配置：方形核可选 1/4/9/16/25/36/49/64 次采样。
保留默认 3x3 覆盖范围，单采样模式取中心；其他数量仅改变同一纹素半径范围内的采样密度，
点光仍逐次跨面重映射。修改采样数量不重画深度图。
共享的 `ShadowSettings::max_samples_per_fragment` 替代原 PCSS 专用预算：
实际 Off/Hard/PCF/PCSS 分别计为 0/1/PCF 数量/阻挡加过滤数量次逻辑采样。
GPU 工作前验证启用模式的质量及合计预算，不要求校验未使用模式的质量参数。
私有样例明确区分关闭一盏阴影的混合模式 `point-mixed.png`
与所有阴影开启的混合模式 `point-mixed-all.png`，并增加 64 次采样 PCF 对比。
关闭阴影不关闭照明。自动化测试覆盖三类灯光的所有支持核、缓存复用、
无效核及混合模式预算计算。

离线 `pbr_milestone` 样例读取本地准备的 camera fixture 及用户 GLB，
渲染虚拟地面和贴地模型，并输出太阳/点光/聚光对比 PNG。
相机输入仍为条件性 Ti+C 射影估计，不是物理标定验收；
私有输入和输出仅留在本地。该样例不定义新的公共 wire schema，
也不代替要求的 NativeSocket/WebSocket 交互路径。

为便于检查，样例额外输出 `standard.png` 和逐模型的 `model-N.png`，
复用普通 FOV/look-at 针孔相机，采用水平光轴、零 skew 和基于包围盒的取景。
模型及照明不变，原始 Ti/C 保留用于对比视角，
从而区分相机投影造成的视觉倾斜和模型本身的朝向。

当前 Cargo 工作区位于仓库根，包括 `server/renderer`（`chr-renderer`）、
`server/runtime`（`chr-server`）、`server/viewer`（`chr-app`）和
`common`（`chr-common`）。当前服务已将旋转三角形送入模拟 SDI 端口，
并通过 WebRTC 监看端口实际发出的画面。

[帧输入契约](../../docs/render-frame-input.md) 已确认持久 scene 和
**首期仅 pinhole、无畸变**的输入范围。本路线图保留正交与畸变镜头作为后续扩展，
不向首期 payload 填入可选占位字段。
[通用协议](../../docs/protocol-conventions.md) 已确定 JSON-RPC 控制；
业务方法 schema、帧资源和 scene 绑定精确边界仍待决定。

## 1. 精简功能清单

精简的是职责归属、配置和交付入口，**不是**已要求的相机与灯光能力。
保留一个 Rust/wgpu 核心及现有原生/无窗口外壳，不复刻参考项目的编辑器和协议栈。

| 领域 | 参考项目 / 当前状态 | chr 建议范围 |
|---|---|---|
| 渲染核心 | trd 多前端共享渲染；chr 共享三角形管线 | 一个渲染器同时支持原生 surface 与离屏纹理 |
| 场景 | trd 有放置后的图元、资源仓库、辅助线和场景组装 | 扁平网格实例、共享资源、相机、灯光及背景；不引入 ECS 或场景树 |
| 几何 | trd 加载 OBJ、GLB，并支持材质分组 | 静态索引网格与实例，每个实例一个 model matrix；GLB 导入保留材质分组；OBJ 转换留在核心之外 |
| 材质 | trd 暴露完整 Disney 参数集 | PBR 是主要/默认材质路径，参考 trd 着色；支持金属度-粗糙度、基础色、法线、自发光、遮蔽、opaque/mask alpha 及双面表面；unlit 为辅助 |
| 渲染模式 | 填充、线框、纹理、PBR 和诊断视图 | PBR 用于正式渲染；unlit 用于初期管线、明确的无光照资源和诊断；线框、法线、深度、坐标轴和包围盒不另建场景模型 |
| 相机 | trd 保存 view/projection/viewport 并提供 CG/CV 构造；chr 只有 Ti 投影数学 | 透视/FOV 与标定针孔是等价输入；正交；跟踪 Ti + C；显式镜头畸变 |
| 局部/直接光 | trd 打包固定的三盏方向光；点光 shader 槽位存在但默认为空 | 场景直接定义方向光、点光、聚光和面光；没有隐藏的主光/补光/轮廓光 |
| 天空 | trd 支持 HDR 环境照明和可选可见背景 | 一个天空源：OpenEXR `.exr` 环境或物理大气；照明与背景共享参数 |
| 面光形状 | 所检查的 trd 灯光接口没有场景面光路径 | 矩形、圆盘、球形；显式大小、方向、辐射和单双面属性 |
| 阴影 | trd 只有 blob，明确拒绝 `shadow_map`；chr 没有阴影路径 | 显式支持 PCF/PCSS 的几何阴影、有限面光/太阳阴影及天空可见性；blob 不能替代 |
| 输出 | trd 逐物体 tone mapping；chr 输出 sRGB 三角形 | 场景线性 HDR 累积，统一输出曝光/tone map，再做 sRGB 展示或读回 |
| 输入 | chr-common 已实现 JSON-RPC 与本地 IPC；帧输入业务 schema 仍为草案 | 适配 scene 修改、固定渲染配置和 FrameKey/image_ref/逻辑时间/Ti/C；不逐帧重传完整 scene |
| 平台 | trd 有原生、浏览器、CLI、GUI 和编辑器 | 仅 Linux 原生与无窗口路径；Windows 不在范围内；暂缓浏览器交付，但不在架构上封死 |
| 编辑/媒体 | trd 包含拾取、变换编辑、视频解码适配、稀疏轨迹、导出与重放 | 不纳入本轮渲染器增量；不做时间线、媒体解码器或旧 trd 文档迁移 |

暂缓各向异性、次表面、透射、clearcoat、sheen 及通用 alpha 混合。
导入时应报告不支持的 GLB 特性，不能静默压平成貌似等价的材质。
若上游文档需要无损往返，在渲染热路径之外保留原始资源字节。

优先完成正确且共享的 PBR 实现，以 trd 的 PBR/Disney shader 为算法参考。
精简公共参数和职责划分，不牺牲照明/材质响应的正确性。
其他显示模式是辅助工具，不作为同等权重的独立材质系统。

HDR 图像资源使用 **OpenEXR `.exr`**，不使用 Radiance `.hdr`。
环境加载保留场景线性浮点值，明确支持的通道、精度和颜色元数据。
定义支持的 flat RGB(A) 范围，对不支持的 deep/multipart/通道布局显式报错，
不能静默转成 LDR。现有 sRGB PNG 截图保持不变；未来如增加 HDR 图像导出，同样使用 `.exr`。

完整目标包含**五类灯光、物理大气、多种面光形状，以及包含 PCF 和 PCSS 的阴影**。
分阶段仅代表实施顺序，不是缩减为无阴影 MVP。
鱼眼、全景相机、景深、运动模糊、云和完整全局光照不在本范围。
GLB 动画播放、骨骼蒙皮、Morph 形变和动画混合也暂不支持；
仅支持模型整体的矩阵更新。

本设计仅以 Linux 为交付和验收平台，不要求 Windows 支持或 Windows 专项验证。
本次仅更新设计文档，不删除当前基础实现中已有的 Windows 相关代码和脚本。

## 2. 最小架构

```text
JSON-RPC scene/config commands -> chr-common -> runtime adapter -> retained Scene
                                                                      |
FrameKey + image_ref + logical_time + Ti/C -> frame assembly <-----------+
                                                   |
                         validated frame + Camera + bound scene view
                                                   |
                                  one Renderer in chr-renderer
                                      |
                                      +--> runtime -> SDI output -> monitor -> WebRTC
                                      +--> viewer/test target -> presentation/readback
```

| 所有者 | 职责 |
|---|---|
| 协议适配器 | runtime 侧映射 chr-common 业务消息和原生帧输入；字段/坐标校验、资源解析及 scene/帧一致性 |
| `Scene` | 持久的扁平网格实例、灯光、单一天空与背景可见性；render worker 在明确边界修改状态 |
| `Camera` | 唯一的源投影表达、原生图像尺寸、镜头标定及裁剪参数 |
| `RenderSettings` | 运行前确定的输出/颜色设置、抗锯齿、调试视图及质量/资源预算；不逐帧重复或隐式修改 |
| `Renderer` | GPU 资源、派生相机数据、管线、阴影/天空缓存、pass 编码及读回 |
| 原生/无窗口外壳 | Linux 窗口生命周期、输入事件、surface 获取与恢复、展示、文件 I/O 和图像编码 |

在 `chr-renderer` 内使用普通模块，先区分相机、场景、材质、灯光和渲染职责。
不为每个概念创建 crate，不引入通用 render graph、插件框架或重复的类型化数学库。
仅在能避免真实错误的单位和坐标约定上使用强类型。

概念入口为 `render(scene_view, camera, settings, target)`；
scene view 来自持久状态，而不是每帧完整的线协议 scene。资源上传一次后通过稳定句柄引用，
不逐帧解码或上传。删除资源后必须拒绝失效句柄，不能让它静默指向复用槽位中的其他资源。
target 是输出目的地，不是另一个渲染器。GPU 数据按变化频率更新：资源/材质按变更、
相机/灯光按帧、变换按实例。

灯光属于场景，曝光属于输出，材质参数属于表面。不保留逐物体天空旋转、
重复相机姿态或会泄漏到下一场景的隐式灯光 setter。
GPU 打包是派生数据，不能让固定 uniform 数组大小反过来决定公共数据模型。

### 协议适配器与持久 scene

`common/src/{jsonrpc,methods,framing,server}.rs` **已经实现**控制封装和本地 framing：
JSON-RPC 2.0 经 NativeSocket 或 loopback WebSocket 进入同一 handler。
为已确认业务方法扩展 `chr-common` 及 TypeScript 镜像，不另造封装或重选传输。
帧/媒体数据不必使用 JSON。

薄映射/组装适配器放在 `server/runtime`，依赖 `chr-common` 和 `chr-renderer`；
renderer 不依赖网络、wire 方法名、硬件设备或业务协议。
原生帧生产者可通过原生数据边界写入帧存储。

外部渲染指令必须同时支持 **NativeSocket 和 WebSocket**：scene/model 修改、
相机控制、灯光/阴影控制及已确认的渲染提交，均进入同一个 JSON-RPC handler
和 runtime 命令适配器。NativeSocket 保留 Unix Domain Socket 加 4 字节大端长度 framing；
WebSocket 保留每个 text message 一条 JSON 消息。
只有一份权威 scene 和回执/错误语义，不按传输复制状态。
不能用 WebRTC DataChannel 或直接调用 renderer 作为 demo 的外部控制通道。
原始图像/GLB 数据另行定义资源传递，不自动塞进大块 JSON。
核心单元/离屏测试及并行开发的 mock 仍可直接调用类型化 API。

| 输入层 | 映射职责 |
|---|---|
| 配置 | 运行前确定输出规格及颜色处理；显式重配置另行约定 |
| 持久 scene | 状态修改指令更新对象、model matrix、材质、灯光、天空和显隐；不逐帧传完整对象清单 |
| 逐帧输入 | 关联 `FrameKey`、已发布 `image_ref`、逻辑时间、Ti 和相容且已解析的 C；明确原生图像/世界域 |

首期帧范围仅 pinhole：没有镜头系数或畸变占位字段，不接收真实场景深度/分割，
核心不要求 atlas 或 calibration 记录查询。
正交和畸变相机如需暴露到直播输入边界，须另行确认扩展。
上层 scene 文档中的通用动画/播放术语不意味着本渲染路线图增加 GLB 动画。

源帧身份独立于 slot 序号、到达顺序、RPC ID 和输出位置。
C 从明确相容的帧或上下文解析，不能静默复用过期 Ti/C。
矩阵方向/布局与单位只转换一次，保留 Ti 系数，外部资源 ID 与 GPU 句柄分离。
输出尺寸和颜色策略不能通过逐帧默认值发生漂移。

发布可读帧和提交渲染是两个逻辑操作：publish 结果不代表渲染完成，
submit 回执不代表已经输出。候选的提交时 scene 快照记录 `scene_revision`，
防止后续指令改变在途任务；但精确绑定点、方法名、生效顺序、结果、错误和批次语义
须在协议决策 issue 中确认后再实现。

使用有界帧存储，检测过期引用，明确就绪依赖，并覆盖所有消费者及 GPU 操作的生命周期。
不能在提交时就复用 slot 或覆盖在用画面。
池满、缺失/迟到几何、取消与 deadline 策略仍是决策前置条件，不设方便实现的隐式默认。
输入与输出存储保持分离。

遵守[执行语义](../../docs/execution-model.md)：内容采用逻辑时间，
保护和节奏分别使用相应运行时时钟。JSON-RPC batch 不代表有序或原子 scene 修改。
重放/步进和批次大小变化必须保留已约定的帧关联与 effect 边界，
不承诺跨 GPU 像素逐位一致。

本任务不实现录制/标定服务、tracking 算法、codec、新 SDI 硬件或旁路的
render-direct WebRTC 流。保留 `render -> SDI -> monitor encoder -> WebRTC`；
基础设施演进仍由已有 tracker 负责。

### 静态模型与 model matrix

每个模型实例具有一个有限的 4x4 仿射 `model` 矩阵，将模型局部坐标映射到
chr 世界坐标：对齐次列向量，`world_position = model * local_position`。
它控制整个模型的平移、旋转和缩放，覆盖所有材质分组。
不同实例可以共享同一资源，但使用独立的 model matrix。

调用方可以逐帧替换 model matrix；渲染器无需提供动画播放器、关键帧插值
或逐节点变换控制。主渲染、阴影 pass 和包围盒使用同一变换；
非均匀缩放时，照明法线使用线性变换的逆转置。

静态导入时组合 GLB 自带的默认节点变换，并一次性烘焙到模型局部几何，
不能在每次 model matrix 更新时重复应用。
包含动画片段的静态资源可以按默认节点姿态加载，并明确提示不播放动画，
而不是任意采样某个动画帧。需要骨骼蒙皮或 Morph 形变的资源应报告不支持，
不能静默显示为貌似等价的静态模型。

### Mesh storage 的参考与适配

同时参考 `xiaoyxue/Gyunity` 的 `7361064`，具体为
`Gyunity/src/mesh/Mesh.h` 和 `Gyunity/src/container/README.md`。
检查到 Mesh 持有顶点/法线/UV/面向量及独立三角形缓存；
容器文档明确连续存储所有权、尺寸检查，以及存储变化会使借用视图失效。
这些语义可作参考，但不是已有 GPU 资源仓库可直接移植。

chr 应将不可变/可共享 CPU 网格、材质分组和包围盒，与每实例 model matrix、
renderer 持有的 GPU 驻留资源分开。几何上传一次，变换修改不重建顶点数组。
先采用带边界检查的普通 Rust 集合及防过期类型化句柄，不提前引入通用存储框架。
外部资源 ID 不等于驻留 slot。CPU 数据只为明确用途保留；
不照搬 Gyunity 的光追三角形缓存、shared-pointer 对象图，
也不在 `Vec` 足够时搬入 N-D 容器。
变更在线状态前检查尺寸/索引溢出，删除或替换时保留在途 GPU 使用所需的资源所有权。

## 3. 相机、单应矩阵和镜头契约

### 3.1 保留现有坐标

世界单位仍为米：X 沿场地长度，Y 沿宽度，地面 Z=0，**负 Z 向上**。
齐次向量使用列向量。Ti 是前向、行主序的地面到 SNC 单应矩阵，**不是**内参或旋转矩阵。

对原生尺寸 W,H，令 s=sqrt(W*H)，定义像素到 SNC 的 S：

```text
S = [ 2/s   0   -W/s ]
    [  0   2/s  -H/s ]
    [  0    0     1  ]

A(C) = [ 1  0  -Cx/Cz  0 ]
       [ 0  1  -Cy/Cz  0 ]
       [ 0  0   -1/Cz  1 ]

p_ideal = [u, v, 1]^T
p_ideal ~ S^-1 Ti A(C) [X, Y, Z, 1]^T
```

C 是世界坐标中的光心，Cz 必须非零。Z=0 时退化为
`p_ideal ~ S^-1 Ti [X,Y,1]^T`，仅地面投影不需要 C。
所有齐次因子乘完之后再做除法：中间无穷远点仍可能得到有限的最终图像投影。

保留导入 Ti 的全部系数，包括尺度与符号，不静默转置、求逆或归一化。
保留原生图像尺寸和整数像素中心，不添加半像素偏移。
仅在 GPU 边界将 Y 向下的 SNC/像素转换为 wgpu 的 Y 向上 NDC。

该等式定义的是**理想投影图像**。一般的畸变图像无法由一个 3x3 单应矩阵精确表达。
若上游直接用畸变观测拟合 Ti，它只是近似，不能据此认定畸变已经包含在矩阵中；
输入必须声明其图像域。

### 3.2 分开投影模型、参数化与畸变

| 相机输入 | 参数 | 含义 |
|---|---|---|
| 透视/FOV | camera-to-world 姿态、垂直 FOV、宽高比、near/far | 居中、零 skew 针孔相机的便捷构造 |
| 标定针孔 | camera-to-world 姿态、K、原生尺寸、near/far | 同一透视模型，支持偏心主点和 skew |
| 正交 | 姿态、水平/垂直跨度、near/far | 平行光线，不能转换成有限针孔 K |
| 跟踪单应 | Ti、非地面几何所需的 C、原生尺寸 | 保留上述投影关系；只有 Ti 的地面投影不是通用三维相机 |

K 使用像素单位，矩阵为 `[[fx, skew, cx], [0, fy, cy], [0, 0, 1]]`。
标定光学坐标为 X 向右、Y 向下、Z 向前。明确 pose 是 **camera-to-world**，
其逆为 world-to-camera。在一个边界完成光学轴到现有世界/GPU 约定的转换，
不能照搬 trd 的 +Y-up 世界假设。

只保存一种源表达，不允许 K、FOV、view、projection、Ti、C 各自独立编辑。
可以缓存派生矩阵。跟踪输入必须先具备有效的相机方向/前向深度约定，
或经过显式验证的投影相机分解，才能启用三维裁剪和照明；Ti 单独不足以提供这些信息。
不能把世界 Z 或未归一化的 `h.z` 当作 wgpu 深度；即便派生其他表达，也保留源系数。
同时提供标定和跟踪表达时，应检查像素空间的一致性，而不是静默任选其一。

相机的点投影和像素反投影光线必须一致。透视光线共享起点；
正交光线起点不同、方向恒定。正交镜面着色必须使用该方向，
不能套用 `normalize(camera_position - surface_position)`。

### 3.3 带畸变的镜头

首期直播帧范围不包含畸变。对后续单独确认的扩展，
建议支持 `None` 和 **Brown-Conrady 径向/切向畸变**。
显式提供模型标识、标定尺寸、归一化 K、有效图像域和系数。
不能按系数个数猜模型，也不能将 rational/fisheye 标定静默当成 Brown-Conrady。

由 `K^-1 p_ideal` 得到归一化理想坐标 `(x,y)`：

```text
r2 = x*x + y*y
radial = 1 + k1*r2 + k2*r2*r2 + k3*r2*r2*r2
xd = x*radial + 2*p1*x*y + p2*(r2 + 2*x*x)
yd = y*radial + p1*(r2 + 2*y*y) + 2*p2*x*y
p_distorted ~ K [xd, yd, 1]^T
```

这里是 K 归一化的光学坐标，**不是 SNC**。Ti 输入先计算 `S^-1 Ti A(C)`，
再在声明的域内应用标定镜头映射。Ti 不能补出缺失的镜头标定。

先将理想场景连同正确深度渲染到线性 HDR，再采用**逆向图像映射**：
每个畸变目标像素求解对应的理想源位置。前向散射顶点/像素会产生空洞，
也不能正确弯曲三角形边缘。必须限制反解次数，显式标记无效像素覆盖，
并验证标定域可逆。理想渲染范围应覆盖所需源采样，不能将缺失边缘样本
clamp 成被拉伸的边界像素。

零系数必须产生恒等映射。该后续扩展仅对透视与跟踪投影相机支持畸变；
在正交标定语义明确前，拒绝正交相机的非零畸变。
输出缩放/适配在原生域投影和镜头映射之后进行，窗口 resize 不得静默缩放
导入 Ti 或标定参数。

若未来调用方合成已畸变的真实图像，只对虚拟图层及其预乘 alpha 做畸变，
然后在相同像素域合成，不能再次扭曲真实图像。
拾取必须反解同一映射，不能另用近似相机。

## 4. 灯光与阴影

### 4.1 一个场景定义的灯光模型

使用扁平的局部/直接光集合，加一个可选天空。
`Directional`、`Point`、`Spot`、`Area` 是直接光变体；
`Sky` 是第五类，属于场景环境而非局部发光体。
面光具有形状，不为每种形状创建独立渲染器。

| 类别 | 必需数据 | 着色 / 可见性 |
|---|---|---|
| 方向光 | 传播方向、颜色、辐照度；太阳可具有角半径 | 远处照明；方向光阴影图 |
| 点光 | 位置、颜色、辐射强度；若限定范围则显式提供 | 平方反比衰减；cubemap 阴影 |
| 聚光 | 点光参数、传播方向、内外锥角 | 平方反比及平滑锥形衰减；透视阴影图 |
| 面光 | 矩形尺寸、圆盘半径或球半径；姿态、辐亮度、单双面属性 | 对有限发光体及可见性积分；大小影响高光和半影 |
| 天空 | OpenEXR `.exr` 图及旋转/增益，或物理大气参数 | 漫反射/镜面环境照明、可选可见天空及环境可见性 |

使用线性 RGB 和明确的辐射度学量：点光/聚光为 W/sr，方向光为垂直平面上的
W/m^2，面光/天空为 W/(m^2 sr)，并说明 RGB 近似。
不能用一个未解释的 `intensity` 标量混用所有单位。
若增加流明/坎德拉 UI，转换必须明确；曝光不改变发射功率。

拒绝非有限或负发射值、零方向、错误锥角顺序和非正面光尺寸。
平面发光体默认单面，双面需显式声明；球体向外发光。零发射表示无贡献。
显式影响范围截断是声明过的近似，不是悄悄替换平方反比衰减。
理想点源的近场奇异性需要明确处理策略，不能直接沿用 trd 的任意
`0.01 + distance^2`。

### 4.2 物理大气，不是天空颜色渐变

以 Hillaire 的可扩展生产级大气方法（EGSR 2020，来源见下文）作为主要光栅/计算参考，
采用 LUT 大气模型，包含 Rayleigh/Mie 散射、吸收（含臭氧）、
地面反照率、观察者海拔及近似多次散射。
定义行星/大气半径和系数单位，验证参数范围，不硬编码无单位的地球常数。
局部海拔使用 `ground_altitude - Z`，遵循负 Z 向上。

大气引用一盏太阳方向光。唯一的太阳方向、角半径和源发射量同时驱动大气计算、
可见日盘及直接太阳照明/阴影。地面直接太阳辐照度必须乘大气透射率，
不能使用未经衰减的大气顶数值。

从与背景相同的天空辐亮度生成漫反射辐照度和按粗糙度预过滤的镜面输入。
启用解析太阳照明时，环境照明积分应排除直接日盘，避免计算两次；
可见背景仍可显示日盘。太阳的镜面/直接贡献必须保留有限角范围。

本提案中 HDR 与大气是互斥天空源，不是两个独立旋转的天空。
隐藏背景不等于关闭环境照明。太阳/大气参数变化会使相关 LUT 与环境照明缓存失效；
仅修改输出曝光不会。空气透视、云和通用参与介质暂缓。

### 4.3 阴影方法与明确的近似

| 灯光 / 效果 | 建议基线 | 限制 |
|---|---|---|
| 方向光 / 太阳 | 拟合正交深度图，视野需要时加入级联，可选 PCF/PCSS | PCSS 根据光源角尺寸近似接触硬化；PCF 单独不具备该效果 |
| 点光 | 六面深度 cubemap，统一径向深度比较，支持 PCF/PCSS | 理想零尺寸光源产生硬阴影；有限阴影光源半径属于显式近似 |
| 聚光 | 匹配外锥角和范围的透视深度图，支持 PCF/PCSS | 锥形衰减与阴影投影必须一致；有限阴影光源半径须显式声明 |
| 面光 | 按 4.5 节选择高效光栅可见性方法；确定性表面积分提供验证参考 | 单个点光替代无法证明形状正确的照明或阴影 |
| 天空 | 按 4.5 节选择考虑可见性的环境着色；采样方向光可见性提供参考 | 高成本参考积分不是强制生产路径；无遮挡 IBL 仍须标为近似 |

对单面面光，直接照明参考估计形式如下，用作正确性基准，而非规定生产算法：

```text
Lo ~= (1/sample_count) sum[
    BRDF * Le * visibility * max(n dot L, 0) * max(n_light dot -L, 0)
    / (distance^2 * pdf_area)
]
```

均匀表面采样时 `pdf_area = 1 / emitting_area`。
应采用对应形状的分布和单双面规则，必要时可见性应包含发光体自身遮挡。
固定辐亮度并扩大面积会增加总功率；若要固定总功率，必须显式转换。
不能把所有面光当作点光后声称已实现随尺寸变化的照明。

有限角范围的太阳采样可复用方向光可见性；天空积分必须一致地考虑入射辐亮度、
BRDF、立体角和可见性。预览可以使用未考虑场景遮挡的预过滤 IBL，但必须标注质量模式。
只有遮挡同时影响漫反射和镜面环境贡献后，才能算天空阴影完成。
SSAO 或 blob 不能替代该能力。

所有阴影 pass 与主 pass 共享几何变换、alpha-mask 规则和明确定义的单双面策略。
说明深度/法线偏移的单位，检查阴影痤疮、悬浮阴影、cubemap 接缝和级联过渡。

暴露采样数、分辨率、阴影 pass 总数及内存预算。
不支持的质量或超预算应明确报告，不能静默丢灯。
即使 API 很小，面光和天空采样也可能很贵：简化架构不等于算法免费。
该可移植 wgpu 光栅设计不以光线追踪或完整路径追踪为前提。

### 4.4 必须支持的 PCF 与 PCSS 模式

方向光、点光和聚光显式支持 `Off`、`Hard`、`PCF`、`PCSS` 阴影模式，
共享阴影图生成路径，不创建独立渲染器。逐灯选择模式，共享质量/资源预算。
PCF 和 PCSS 是必需能力，不是未确定的未来备选项。

**PCF（percentage-closer filtering）**对多个深度比较结果求平均，
不是先平均深度再做一次比较。可配置的滤波核用于柔化阴影图边缘、减少锯齿。
固定滤波半径本身不能模拟随遮挡物与接收面距离增大而变宽的半影。

**PCSS（percentage-closer soft shadows）**明确分为三步：

1. 搜索原始阴影深度中位于接收面之前的遮挡物，在一致的线性光空间深度约定下估计其平均距离。
2. 根据光源尺寸和遮挡物/接收面间距估算半影大小。
3. 使用所得可变滤波半径执行 PCF。

对有限局部光源，通常的相似三角形近似为
`penumbra_radius ~= source_radius * (receiver_distance - blocker_distance) /
blocker_distance`。对角半径为 `theta` 的方向光，沿光照方向使用
`penumbra_radius ~= tan(theta) * blocker_receiver_separation`。
再将结果转换为阴影图中的投影范围，不能给太阳虚构有限距离后套用局部光源公式。

不能把非线性透视深度缓冲值的平均值当作物理距离。
二维图使用线性光空间深度，cubemap 使用声明的径向深度度量；
遮挡判断和重建必须与存储编码一致。有效搜索未发现遮挡物时，结果为完全受光。
限定搜索/滤波半径，拒绝无效光源尺寸，并明确处理近平面或零距离情况，避免除零。
零尺寸光源退化为硬可见性，仅保留声明的抗锯齿过滤，不能凭空产生物理半影。

太阳使用大气模型中唯一的角半径。点光/聚光以米表示的非零阴影光源半径
属于明确标注的软阴影近似，不能把平方反比点光着色变成有限发光体 BRDF 积分。
需要有限形状照明时使用 `Area`。PCSS 不能替代按形状采样的面光阴影或天空可见性。
对面光/太阳/天空的采样积分，每个样本按需使用 Hard/PCF 过滤可见性；
不能再给每个样本叠加一次整个发光体的 PCSS 模糊，重复计算光源尺寸。

暴露以阴影纹素为单位的 PCF 滤波半径、滤波采样数、PCSS 遮挡物采样数，
以及最大搜索/滤波半径，并与物理光源尺寸分开。
质量预算同时计入 PCSS 的两个采样阶段。采用确定性采样保证输出可复现，
不支持的设置不能静默从 PCSS 回退到 PCF。

方向光级联将同一个世界空间半影转换到各级纹素尺度，并混合过渡；
稳定阴影图拟合，避免相机移动时闪烁。点光滤波核在方向/切空间范围内跨越
cubemap 面边界，不能使用被夹紧在单个面上的 UV 核。
PCF 和 PCSS 都必须遵循偏移、阴影图覆盖范围及 alpha-mask 投影物规则。

### 4.5 现代光栅方法，而非默认暴力采样

trd 已实现所需行为时，先审查并简化再复用；未实现的灯光/阴影能力，
应评估有明确来源的现代生产级光栅方法，而不是临时编写朴素算法。
“SOTA”是质量/性能选型标准，不表示某一算法普遍最优，也不把早期论文称作最新论文。

| 领域 | 优先参考 / 候选 | 必须明确的边界 |
|---|---|---|
| 有限面光 PBR | Heitz 等人的 LTC（SIGGRAPH 2016），用于多边形/矩形积分 | LTC 近似无遮挡 BRDF 积分，不解决可见性；圆盘/球形须单独确认并验证，不能直接假设矩形查表适用 |
| PCF/PCSS | 稳定级联/cubemap 阴影图、优化比较核和有界遮挡物搜索 | 保留明确要求的 PCF/PCSS 模式、深度正确性、确定性采样和预算控制 |
| 可扩展软阴影 | 评估 Epic 文档中的 Virtual Shadow Maps 与 SMRT | Virtual Shadow Maps 不是 Variance Shadow Maps；SMRT 采样光栅深度，不做硬件三角形求交。引入分页/缓存/几何集成成本前须证明收益，并记录单层深度局限 |
| 物理大气 | Hillaire 的可扩展生产级大气（EGSR 2020） | 以可移植光栅/计算实现所需天空/太阳子集，明确单位、LUT 精度和缓存失效；不自动扩展到云/体积 |
| 环境遮挡 | 评估考虑可见性的 probe/map 或有界光栅可见性采样 | 仅漫反射接触 AO 不是完整天空可见性或镜面遮挡；说明屏外、薄几何和缺失数据的局限 |

高采样面光/天空积分作为合成离线或测试基准。
不能仅因实现直接，就默认实时路径为每个发光体/环境生成几十张阴影视图。
生产面光着色与可见性必须一致组合；将 LTC 乘以无关的可见性平均值属于近似，
需要量化误差。

单独的算法选型 issue 应记录主要来源、所需 wgpu 能力、实现复杂度、
GPU 耗时、内存、时间稳定性及相对参考场景的误差。
覆盖多遮挡物、掠射视角、接触、大半影、屏外遮挡物和缓存失效。
选择满足约定质量/性能预算的最简单方法；没有证据时不强制整套虚拟阴影基础设施。
任何时间复用都须明确历史失效与重放行为。
不以硬件光追、Nanite 或特定引擎 API 为前提。

#### 有界合成阴影基准

从 workspace 根目录运行：

```sh
cargo run --locked --release -p chr-app --example shadow_benchmark
cargo test --locked -p chr-app --example shadow_benchmark
```

指定的非 NixOS NVIDIA 主机使用
`nix develop --command bash -c 'NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- cargo run --locked --release -p chr-app --example shadow_benchmark'`。
测试仅创建合成四边形并写入 `output/renderer/shadow-benchmark.json`，
不读取所有者资产。默认 180 行覆盖方向光、聚光、点光；Hard、全部八种 PCF
方形核、PCSS 8/8、32/32、64/64 遮挡搜索/过滤样本；接触、分离、
大半影、多遮挡物和掠射视角场景。图像为 384×384，阴影图默认 512²；
`--resolution 256|512|1024` 和 `--repetitions 1..20` 限定工作量，
每行每种缓存状态默认重复三次。

仅该 example 请求可选 wgpu `TIMESTAMP_QUERY` 和
`TIMESTAMP_QUERY_INSIDE_ENCODERS`。两个实际 GPU 时间戳包围现有
`MeshRenderer::encode`（深度、HDR 网格及 tone mapping），不包含 CPU prepare、
上传、分配、像素回读和时间戳 resolve。CPU prepare 墙钟时间单独报告，
绝不标为 GPU 耗时。保留原始 GPU 样本及中位数/最小/最大值；
不支持时间戳时 GPU 指标为 null，明确记录状态，硬件验收不成功。
软件或未知类型 adapter 同样不能悄悄通过硬件验收。

每行先运行一帧不计时预热。**Cold** 表示在新 scene revision 中重新提交相同
遮挡物模型，强制重新生成深度，但保留常驻资源和 pipeline；
不是首次编译或分配延迟。**Warm** 重复同一快照。测试检查平面 1 / 点光 6
个深度 pass，而 warm 必须为零，并单独比较 cold/warm RGBA 字节。
内存明确为 `resolution² × layers × 4` 的 Depth32Float texel 数据
（512 时平面 1 MiB / 点光 6 MiB），不是总显存或分配对齐开销。
每片元逻辑预算计 Hard=1、PCF=核样本数、PCSS=搜索+过滤样本数；
双线性比较不等于一次物理纹理读取。

质量评估使用不重复的接收面条带像素中心，排除遮挡物自身覆盖，
将逆 sRGB/Reinhard 后的阴影辐亮度除以无阴影基线。独立 CPU 光线/四边形
可见性积分采样均匀发光圆盘，记录 48²/96² 网格裁圆积分的收敛差异。
报告有限发光体和点发光体 MAE、部分可见像素数（可见性 0.1–0.9）及参考计数。
自动门禁拒绝无效基线、非确定性复用、可见性 >1.05、参考收敛 MAE >0.01、
Hard 点参考 MAE >0.15、PCSS 32/64 有限发光体参考 MAE >0.15。
这些仅是有界夹具回归阈值，不是通用质量预算，也不保证 PCSS 总优于 PCF。

RTX 3090（NVIDIA 580.159.03）release Vulkan 运行使用三次重复及 512²
阴影图，180 行全部通过：平面 cold/warm 行中位数范围 0.018–0.121 /
0.014–0.116 ms；点光为 0.030–0.630 / 0.015–0.615 ms。
PCSS 32/64 有限发光体参考最大 MAE 为 0.071，Hard 点参考 MAE 0.046，
积分收敛 MAE 0.006。计时包含查询开销，可能受到并发 GPU 使用、时钟和调度影响；
微型合成场景不代表通用实时性能保证。本切片不评估级联、专门屏外遮挡物、
cube 接缝、时间相机轨迹、LTC、大气或天空。仅可见性圆盘积分不是 BRDF 加权
传输，点光参考是朝向场景的圆盘而非球面辐射积分。
更广泛的方法选型仍由未关闭的 issue #35 跟踪。

主要参考：

- [Heitz 等，Real-Time Polygonal-Light Shading with Linearly Transformed Cosines（2016）](https://eheitzresearch.wordpress.com/415-2/)。
- [NVIDIA PCF/PCSS 样例及原始 2005/2008 年参考](https://archive.docs.nvidia.com/gameworks/content/gameworkslibrary/graphicssamples/opengl_samples/softshadowssample.htm)。
- [wgpu 时间戳能力](https://docs.rs/wgpu/30.0.0/wgpu/struct.Features.html#associatedconstant.TIMESTAMP_QUERY)及[时间戳周期](https://docs.rs/wgpu/30.0.0/wgpu/struct.Queue.html#method.get_timestamp_period)。
- [Epic，Virtual Shadow Maps 与 SMRT 文档](https://dev.epicgames.com/documentation/en-us/unreal-engine/virtual-shadow-maps-in-unreal-engine)。
- [Hillaire，A Scalable and Production Ready Sky and Atmosphere Rendering Technique（2020）](https://sebh.github.io/publications/)。

## 5. 渲染顺序与实施阶段

建议的 pass 顺序：

```text
validate inputs and resource budgets
  -> update changed assets / atmosphere / environment caches
  -> render required light visibility maps
  -> render ideal-camera geometry and sky into linear HDR + depth
  -> resolve antialiasing and apply native-domain lens warp
  -> composite any caller-supplied scene-linear image layer in the same pixel domain
  -> one exposure / tone-map / sRGB output transform
  -> display-space debug overlays, presentation or readback
```

图像层输入只是渲染边界，不代表加入视频解码器或时间线。
背景可见性与天空照明独立。不能在照明/合成前对每个网格分别 tone mapping。
已转换为显示域的照片/视频不是场景线性辐亮度；若要保持其外观，需要独立且明确的
输出合成策略，不能自动再次对它应用虚拟场景的曝光/tone map。

| 阶段 | 可独立演示的结果 |
|---|---|
| 0. 设计前置决策 | 在现有 JSON-RPC 上确认首期 scene/config/frame 合同；独立选择并基准验证 trd 尚缺的现代光栅灯光/阴影方法 |
| 1. 场景和 pinhole 基础 | 持久 scene 与 model matrix；共享网格/unlit/深度路径；FOV/K/pose/Ti+C pinhole 输入；帧适配、生命周期及版本绑定；保留三角形 |
| 2. PBR 和直接光 | 静态 GLB 材质分组、单一 PBR 路径、点光/聚光/方向光、HDR 天空、hard/PCF 阴影及合成后接入现有 SDI 输出 |
| 3. 接触硬化与有限发光体 | PCSS；矩形/圆盘/球形面光、高效的形状着色和软阴影，以采样参考验证，并明确质量/成本控制 |
| 4. 大气和天空可见性 | 物理大气、一致的太阳/天空照明、有限太阳阴影及考虑可见性的环境照明 |
| 5. 相机扩展 | 正交相机及 Brown-Conrady 畸变；如需暴露到直播协议，必须单独确认 schema 扩展 |

阶段 0-5 覆盖完整渲染路线图，不能把阶段 2 宣称为所有灯光和阴影均已支持。
每个增量通过明确输入/配置选择启用，不静默改变现有三角形和坐标契约。
每实现一项功能，同步扩展适配器映射与契约测试；
具体协议解码器的实现以新输入 schema 已达成约定为前提。

### 并行工作流与优先级

renderer 为**高优先级**。分离所有权，让不同 agent 并行推进，
不把所有核心任务都变成等待协议的工作：

| 工作流 | 负责范围 | 交接 |
|---|---|---|
| [Renderer #11](https://github.com/xiaoyxue/chr/issues/11) | `server/renderer`、`server/viewer` 中的原生/核心验证、PBR/相机/灯光/阴影和核心合成 | 稳定的类型化 scene/camera/target API、合成测试资源和球场场景配置 |
| [Protocol #36](https://github.com/xiaoyxue/chr/issues/36) | `common`、TypeScript 协议镜像、对应业务方法文档 | 已确认 DTO、错误、effect，以及两种现有 IPC 的一致性样例 |
| [Server #37](https://github.com/xiaoyxue/chr/issues/37) | `server/runtime`、运行时集成和交互 harness 行为 | 命令映射、帧生命周期/版本绑定、相机/灯光控制及 SDI/monitor 集成 |

尽早确认最小核心接口，在其他实现到位前使用 mock。
根 Cargo manifest/lockfile 和共享文档的修改需要协调，不能并行互相覆盖。
GitHub blocked-by 表示完整集成/验收的前置条件，不禁止先做设计或原型。
renderer CPU/GPU 开发不等待整个 server，但最终 socket 控制 demo 需要集成完成。

### 高优先级球场演示场景

进度预案 [#45](https://github.com/xiaoyxue/chr/issues/45) 在集成时间影响交付时，
评估 bgfx 和待确认的 tinyrender 等现成方案；不自动替换 wgpu，也不阻塞当前实现，
切换后端需要明确决定。

单独由 [#40](https://github.com/xiaoyxue/chr/issues/40) 跟踪。
独立算法选型前置任务为 [#35](https://github.com/xiaoyxue/chr/issues/35)，
线协议 schema 决策为 [#12](https://github.com/xiaoyxue/chr/issues/12)。

以用户最新提供的私有 scene/camera 数据包和本地 GLB 目录作为可选、私有验收输入。
从包内 metadata 索引解析相对运行路径，不使用历史来源中的绝对路径；
该输入替代早期 SC1464 选择。
已检查到原始 Ti、明确的帧映射/来源、估计的世界光心及地面元数据；
这些本身不证明完整 K/pose 标定在物理上精确。
选择明确源帧，验证相容 pinhole 视角并保留原始值用于 reset。
不能将标定数值、源视频、GLB 字节、本机绝对路径或参考数据派生截图提交到 Git。

先用 `z=0` 的虚拟 PBR 球场接收平面，尺寸来自明确的本地输入/元数据，
并保留其“假设值”属性。模型完成缩放/旋转后，对所有材质分组计算世界 AABB，
再应用 `translation_z = -max_z`。负 Z 向上，因此**贴地底部是 max_z，而不是 min_z**。
模型几何不需要动画。

四角各放一个假设灯塔：`(+-L/2, +-W/2, -h)`，h 可在 30–40 米调整；
35 米只是建议的合成默认值，不是拟合测量。
提供点光/定向聚光模式，调节发射量、锥角和光源尺寸，
加入太阳方向光及 Hard/PCF/PCSS 控制。
演示仅太阳、四点光、四聚光、灯塔加太阳四种配置。
同一灯塔同时叠加点光和聚光时必须明确分配能量，不能误当重复照明；
完整物理大气可后续加入。

实时 zoom/dolly、orbit/旋转和 reset 操作作用于可编辑预览相机，
不改写导入标定。浏览器 harness 通过 WebSocket 控制 server，
显示现有 SDI 输出派生的 WebRTC 画面；NativeSocket/CLI 控制必须作用于同一场景/相机，
两条控制路径的 effect 一致。renderer 独立样例可以先开发，
但只有直接调用的 viewer 不算完成交互验收。
首期虚拟平面 demo 不代表真实视频畸变、外部遮挡或物理 SDI 硬件集成已经解决。

## 6. 验收标准与待定事项

| 领域 | 实现后需要的证据 |
|---|---|
| 协议适配器 | 已确认 RPC/原生输入产生相同的持久状态变更与帧视图；保留现有 framing；拒绝未支持字段/资源；首期 pinhole 范围无畸变字段 |
| 控制传输 | scene/model/camera/light/shadow/render 指令同时经 NativeSocket 和 WebSocket 工作，状态/effect 语义一致；UI/CLI 看到同一 scene；媒体不作为控制通道 |
| 帧生命周期与绑定 | 图像/Ti 乱序、过期引用、就绪、消费者持有、池耗尽、提交后 scene 修改、逻辑重放及按确认策略保证批次大小不改变语义 |
| 坐标保持 | 地面/非地面投影符合 `S^-1 Ti A(C)`；Ti 带符号尺度不变性；没有新增半像素偏移 |
| 模型变换 | 实例矩阵互相独立、模型及材质分组整体移动、非均匀缩放法线、包围盒/阴影一致、静态 GLB 节点变换仅应用一次；明确提示未支持的动画/形变 |
| 相机等价性 | FOV 与等价 K/pose 输出相同像素；支持偏心/skew 标定及横竖图尺寸 |
| 正交相机 | 平行射线、尺寸不随深度变化、正确镜面观察方向、正确 near/far 裁剪 |
| 畸变 | 合成径向/切向样例、零系数、CPU/GPU 一致、逆映射边界、反解失败及无效标定域拒绝 |
| 深度与合成 | 非地面几何遮挡、相机背后裁剪、不重复扭曲已经畸变的背景 |
| 直接光 | 方向符号、远离奇异点的平方反比、聚光锥、零发射及场景间状态隔离 |
| 面光 | 各形状/单双面、采样收敛、随尺寸变化的高光/半影、明确功率/辐亮度行为 |
| 天空 | OpenEXR HDR 数值/通道/颜色处理、环境旋转及天空/反射一致、太阳高度角/海拔响应、无重复太阳、遮挡物减少天空照明 |
| 阴影 | 移动遮挡物、alpha-mask 投影物、非均匀变换、偏移、接缝、过渡及确定性采样 |
| PCF | 对比较结果过滤、可配置半径/采样数、运动时边缘稳定、二维图/cubemap 行为一致 |
| PCSS | 接触处硬阴影，间距/光源尺寸增大时半影变宽；无遮挡物及零尺寸情况；正确线性深度重建；稳定级联/cubemap 边界；与发光体采样参考结果对比 |
| 光栅算法选型 | 审查主要来源及可移植性；将生产方法与参考积分对比，记录质量、GPU 耗时、内存、稳定性和近似边界 |
| 球场 demo | 私有相机/GLB 不外传；变换后世界 AABB max_z=0；四角 30–40 米灯塔、点光/聚光/太阳及软阴影；两种控制传输上的实时 zoom/orbit/reset |
| 输出 / 资源 | 单一输出变换、稳定资源生命周期、显式预算错误、resize 及原生/离屏核心结果一致 |

只使用合成测试资源。未来代码变更须满足仓库的 CPU/lint 和 Linux 真实硬件像素检查。
交互 UI 验收应在交互设备上进行，不以 SSH smoke test 替代。
这些是未来验收要求，**并非已完成验证的声明**。

实现协议适配器前，需解决 `docs/render-frame-input.md` 中待定的业务 schema、
effect 顺序、帧资源/就绪/生命周期、scene 绑定和失败策略，
不重选已确定的 JSON-RPC 封装，也不将持久 scene 替换为逐帧完整快照。
实现相机/灯光功能前，需确认实际标定模型及有效图像域、Ti 属于理想/畸变域的输入元数据、
是否需要矩形/圆盘/球形以外的面光形状，以及目标图像尺寸、帧耗时和内存预算。
性能约束决定采样和级联上限；本文不武断固定灯光数量上限或承诺帧率。

## 7. 来源索引

以 `trd` 为实现参考，不整体移植。每个实现增量记录复用、简化或修正了什么及原因，
对修正行为补充合成回归样例，并区分有意限制和已证实的缺陷。
尤其不能把 +Y-up 约定、固定灯光组、任意点光衰减 epsilon、逐物体 tone mapping
或仅 blob 阴影原样作为本路线图的最终行为；只保留 chr 实际调用方所需的辅助与抽象。

以下参考路径相对于本地 `trd` 仓库；本提案未复制私有参考资源或数据集。

| 来源 | 采用的依据 |
|---|---|
| `trd: README.md`、`docs/architecture.md` | 功能清单、共享核心、交付外壳、资源、协议及媒体边界 |
| `trd: crates/trd-core/src/camera.rs` | view/projection/viewport 表达、CG/CV 输入及 +Y-up 约定 |
| `trd: crates/trd-core/src/light.rs`、`render/pbr.rs` | 固定方向光组、默认空点光槽位、场景级环境 |
| `trd: crates/trd-core/src/shader/pbr.wgsl` | 实际方向光/点光/IBL 着色与逐物体 tone mapping |
| `trd: crates/trd-core/src/render/scene.rs`、`render/options.rs` | 场景/背景分离、灯光归属、模式与辅助线 |
| `trd: crates/trd-core/src/render/render_config.rs` | 仅有 blob 的阴影类型及对 shadow map 的显式拒绝 |
| `trd: docs/pbr.md` | 材质/GLB/环境范围及尚未支持的附加材质特性 |
| `xiaoyxue/Gyunity: Gyunity/src/mesh/Mesh.h`、`Gyunity/src/container/README.md`，`7361064` | CPU 网格数组所有权、连续存储及借用/resize 校验语义；适配 chr 的索引 GPU 网格，不移植光追结构 |
| `chr: server/renderer/src/{lib,coordinates,triangle,offscreen,sdi}.rs` | 当前三角形渲染、投影数学、渲染目标与 SDI 颜色转换 |
| `chr: common/src/methods.rs`、`server/runtime/src/{scene,pipeline}.rs` | 已实现控制方法、逻辑帧三角形状态与 SDI/monitor 输出边界 |
| `chr: docs/{protocol-conventions,render-frame-input,execution-model,calibration-control}.md` | 已确认的控制/pinhole/持久 scene 边界及明确标注为候选的资源/effect 细节 |
| `chr: server/docs/coordinates.md`、`server/AGENTS.md` | 坐标不变量、外壳/核心边界、平台及验证要求 |
