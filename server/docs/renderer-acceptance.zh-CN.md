# 渲染器路线图验收（Linux）

状态：**#34 的 Linux 渲染器验收，2026-11-12**，基于合并 #87 之后的 `main`。
本报告覆盖通过类型化 API 使用的渲染器库。协议传输、服务端 SDI/监看策略与 Windows
另行处理。英文配对文档为 [renderer-acceptance.md](renderer-acceptance.md)；
设计与路线图见 [renderer-design.zh-CN.md](renderer-design.zh-CN.md)。

硬件：**NVIDIA GeForce RTX 3090，驱动 580.159.03，Vulkan，wgpu 30**，通过 SSH 运行。
下列数字只适用于该主机和文中的合成场景，不代表帧率承诺。

## 能力表

| 能力 | 状态 | 说明 |
|---|---|---|
| 保留场景、逐实例模型矩阵、带材质分组的静态 GLB | 首期合同 | 非均匀缩放、镜像与节点变换只应用一次；不支持动画或蒙皮 |
| 由 FOV、K + 位姿或 Ti + C（`S^-1 Ti A(C)`）构建的针孔相机 | 首期合同 | 首期线协议中唯一的相机 |
| PBR（GGX/Burley）、无光照、自发光、alpha 镂空、双面 | 首期合同 | 所有光源共用一条材质路径 |
| 平行光、点光源、聚光灯 | 首期合同 | 使用辐射度量单位；没有隐藏的主/辅/轮廓光组 |
| 硬阴影 / PCF / PCSS、平行光级联、点光源立方体贴图 | 首期合同 | 逐光源模式；共享采样与内存预算，超出时明确报错 |
| OpenEXR 环境光照与可见天空 | 首期合同 | IBL 不考虑遮挡（无天空可见性） |
| 单一曝光/色调映射、MSAA 1x/4x、诊断视图、图像合成 | 首期合同 | 合成时拍摄素材保持显示参考空间 |
| 正交相机 | 扩展（渲染器 API） | 不在线协议中 |
| `radial-poly-native-px-v1` 畸变 + 逐帧单应 | 扩展（渲染器 API） | `CameraModel::{Pinhole, Distorted}`；线协议与标定存储由 #12/#36/#59 把关 |
| 矩形/圆盘/球面光源 | 扩展，带标签的近似 | `ltc-plausible-v1`：逐光源误差 <= 0.02 Le + 30%，面光漫反射为 Lambert x (1 - F0) |
| 面光源阴影 | 扩展，带标签的近似 | `area-shadow-pcss-v1`：中心立方体阴影图 + PCSS；半影不随光源轮廓变化，也不按光滑 BRDF 加权 |
| 阴影接收面（shadow catcher） | 扩展 | 按亮度计算有/无阴影的光照比例，叠加到素材上；无线协议字段 |
| 地面贴花（ground decal，#113） | 扩展 | 不受光照，按预乘 alpha 叠加在不透明 CG 上，按实例顺序绘制，并先于阴影接收面绘制，因此 CG 阴影会压暗贴花；不写深度、不投影；在场景 tone map 之前的线性光照中精确；无线协议字段 |
| 点光源/聚光灯的 PCSS 光源半径 | 预览近似 | 只是软阴影的光源尺寸，不是有限面积着色 |
| 物理大气（#28）、天空可见性（#29） | 可选（所有者决定） | 本次验收不要求 |
| Brown-Conrady 切向畸变 | 延后 | 已改用原生像素径向模型 |
| 面光源、畸变、shadow catcher 的线协议映射 | 延后 | 需要先批准协议与标定契约；可选的[跟踪相机模拟](tracking-simulation.md)只逐帧传 `frame_warp`，镜头在启动时注册 |
| GLB 动画/蒙皮、物理 SDI、硬件编码器 | 延后 | 不在本渲染器路线图内 |

## 验收证据

| 领域（renderer-design 第 6 节） | 证据 |
|---|---|
| 跨功能矩阵、确定性、状态隔离、预算 | `acceptance_gpu`：五类光源配合硬阴影/PCF/PCSS，变换与镜像实例，多种材质，针孔/正交/畸变三种相机；重复渲染逐字节一致，渲染其他场景后结果不变，预算与非法输入明确报错并可恢复 |
| 坐标保持、相机等价 | `camera`、`mesh_gpu`、`orthographic_gpu` |
| 模型变换、GLB | `scene`、`glb`、`mesh_gpu` |
| 畸变 | `distortion_gpu`（恒等、两种模型下标记点误差 < 0.3 px、素材不被扭曲、无原像像素、guard band 预算、输出缩放），`distortion` 单元测试，CPU 参考 `distortion_reference` |
| 深度与合成 | `composition_gpu`、`distortion_gpu`、`shadow_catcher_gpu`、`decal_gpu` |
| 直接光源、PBR | `pbr_gpu` |
| 面光源 | `area_light_gpu`（固定候选值、参考对比在标准内、单双面、尺寸/功率、轨迹连续性）；评估见 `area-light-evaluation.md` |
| 阴影、PCF、PCSS、级联、立方体贴图 | `shadow_gpu`、`area_shadow_gpu`、`shadow_catcher_gpu`（spot 近平面回归） |
| 天空（无遮挡 IBL） | `environment_gpu`、`exr_import` |
| 输出与资源 | `diagnostics_gpu`、`composition_gpu`、`sdi_gpu`、`triangle_gpu` |
| 有界队列、耗尽、生命周期（运行时） | `program_output::explicit_profile_keeps_input_and_submission_bounded`、`retained_control::declared_capacity_and_queue_rejections_do_not_apply_effects`、`frame_resources` 字节预算/耗尽测试、`sdi::output_repeats_on_underrun_and_drops_when_full` |

## 实测预算（1920 x 1080，RTX 3090）

跨功能矩阵（`acceptance_gpu::cross_feature_matrix_frame_costs`）：整帧 GPU 时间，
取七次中位数，阴影图 1024，PCSS 16 + 16 采样。

| 配置 | 冷帧（重建阴影图） | 热帧 |
|---|---:|---:|
| 针孔 | 5.8 ms | 5.7 ms |
| 正交 | 7.9 ms | 7.8 ms |
| 叠加到背景，针孔，4x MSAA | - | 5.8 ms |
| 叠加到背景，畸变模型，4x MSAA | - | 6.4 ms |

每个冷帧 20 个阴影 pass（平行光 1、点光源 6、聚光灯 1、两个面光源各 6）；阴影内存
80 MiB；HDR 目标 15.8 MiB。合成场景很小，真实几何下阴影 pass 的成本会更高。

面光源（`area_shadow_gpu::area_light_frame_cost`）：1 个矩形 0.31 ms，开 PCSS 为 1.5 ms；
8 个混合形状 13.0 ms，开 PCSS 为 23.4 ms。曲面光源不开阴影时每个约 2.5–3 ms。
一个本地转播演示（私有素材，未提交）在 1080p 下合成一帧（畸变、shadow catcher、
4x MSAA）含回读用时 11.6 ms。

## trd 复用与修正

设计文档第 2 节和第 7 节记录了逐项对比。概括来说：trd 的 PBR/Disney 着色为共享的
GGX/Burley 路径提供了参考，但 chr 修正了缺失的接收面余弦和随意设定的
`1/(0.01 + d^2)` 点光源衰减；用几何的硬阴影/PCF/PCSS 阴影图替代了 blob 阴影；
用单一输出变换替代了逐物体色调映射；并保留 chr 的 -Z 向上世界坐标，而不是 trd
的 +Y 向上相机假设。每项修正在上述测试中都有合成回归用例。

## 检查结果与未验证项目

本主机上通过：`cargo fmt --check`、禁止警告的 workspace Clippy、workspace CPU 测试、
完整的渲染器硬件测试（`cargo test -p chr-renderer -- --ignored`）以及 `nix flake check`。

未验证：Windows；本机交互 UI（窗口缩放、最小化/恢复、HiDPI、Escape/关闭），需要在
可交互设备上而不是通过 SSH 检查；物理 SDI 采集/输出与硬件编码；渲染器扩展的线协议控制。
未使用任何私有参考资料作为测试夹具。
