# vchao 的 atlas、V、Ti 与 T_ref

**状态：** `draft` — 面向 chr 读者解释 vchao 的几何关系。以 vchao 自己的文档为准；
本页依据当前固定的子模块 `external/vchao`（`f209fc9`）。

**范围：** vchao 中「atlas」指什么、这个词被用于哪三种产物，以及 K、R、C、H、V、q、
T_ref、Ti 之间的公式关系。本页不定义 chr 协议。英文版：[vchao-atlas.md](vchao-atlas.md)。

`external/vchao` 中的出处：`README.md`（`Atlas`）、`docs/camera-geometry.md`
（目录前的开篇部分，以及第 3、6、8、9 节）、`projective-atlas/README.md` 和 `atlas-vq-estimate/README.md`。

## 被叫做「atlas」的三样东西

| | vchao 的叫法 | 参考坐标系 | 内容 | 有没有像素 | 状态 |
|---|---|---|---|---|---|
| ① | **`Atlas`**（正式定义，canonical） | 机位的**物理方向**（已标定） | 每个机位一套方形图块：12 个水平方向 × 3 个俯仰，`K=I`，视场 90°；存图块的方向、静态背景图和覆盖范围 | 有 | 只有设计，还没实现 |
| ② | uncalibrated projective panorama（`projective-atlas` 的输出） | **任意选的一帧**，按 `K=I` 显示 | 同样的 36 个图块布局，内容是已配准各帧 RGB 的逐通道时间中值 | 有 | 已实现，用于显示和评估 |
| ③ | anchored **V/Q map**（`atlas-vq-estimate` 读的 manifest） | 建图时选定的参考平面 | 特征点 q 和描述子、每个视图的 V、T_ref | **没有** | 已实现，**实时求 Ti 用的就是它** |

- 严格意义上，vchao 的 `Atlas` 只指 ①。vchao 明确说明全景图 ② 有别于 canonical Atlas。
  V/Q map ③ 是另一种射影、未标定的产物，同样不是 canonical Atlas。② 和 ③ 使用同一类
  参考平面配准（见下文第 3 节），但不一定是同一组 V：V/Q map 保存自己拟合出的 V 和 q，
  两者必须配套使用。
- 「要有 atlas 才能实时估计 Ti」指的是 V/Q map **③**。
- chr 的业务文档（[calibration records](calibration-control.md)、
  [system architecture](system-architecture.md)）中的「atlas」是宽泛用法，指
  「每张都带有球场 Ti 的参考图像，加上光心 C」。它最接近 V/Q map 的 `views` 表
  （带 V 和可选 Ti 的帧），不是 vchao 的 canonical `Atlas`。

## 公式

矩阵作用于齐次列向量，图像坐标为 SNC（[坐标契约](../server/docs/coordinates.md)）。

### 1. 单帧相机

K 为内参，R 为世界到相机的旋转，C 为以球场米为单位的光心。

```text
D_i  = R_iᵀ K_i⁻¹                         像素 → 世界方向
P_i  = K_i R_i [I | −C_i]                  世界 3D → 像素
Ti_i = P_i E_0 = K_i R_i [e1 e2 −C_i]      球场地面 (X, Y) → 画面
A_i,h = P_i E_h = D_i⁻¹ [e1 e2 (h·e3 − C_i)]   高度为 h 的水平面 → 画面
```

`E_h` 把平面坐标 `(X, Y, 1)` 提升为 `(X, Y, h, 1)`。离开地面的点需要 C；
Ti 只覆盖 `h = 0`。

### 2. 光心不动时，两帧之间是纯单应 H

```text
H[j←i] = D_j⁻¹ D_i = K_j R_j R_iᵀ K_i⁻¹
```

C 被消掉。只做摇、俯仰和变焦的机位，任意两帧之间都只差一个 H。

### 3. 参考平面和 V

选一个参考 r。参考平面（「全景平面」）是一个抽象的射影平面（RP²），
不是俯视图，也不是 3D 坐标。当 r 是一帧真实画面时：

```text
V_i = X_i = H[i←r] = K_i R_i R_rᵀ K_r⁻¹    参考平面 → 第 i 帧
U_i = V_i⁻¹                                  第 i 帧 → 参考平面
H[j←i] = V_j V_i⁻¹
```

vchao 的图（graph）代码把这个节点叫 `X_i`；全景图使用的映射（第 i 帧 → 参考平面）叫 `U_i`（即 `V_i⁻¹`）；V/Q map 把节点叫 V。
不标定时只能知道 `V_i ~ D_i⁻¹ A`，其中 A 未知（gauge）；把所有 `V_i`
右乘同一个矩阵 M，帧间的 H 都不变。

### 4. 特征点 q（属于 V/Q map ③）

```text
x_ik ~ V_i q_k      第 k 个特征点在第 i 帧的位置
```

map 中保存的 V 和 q 由 vchao 之外的流程拟合产生，vchao 没有记录建图方法。概念上，
建图就是拟合所有 V_i 和 q_k，使每个观测都满足上式。q 是参考平面上的齐次点，不是度量 3D 点，第三个分量可以为零。

### 5. 锚定到球场

用一帧已知 Ti 的锚点帧 a：

```text
T_ref = V_a⁻¹ Ti_a = K_r R_r [e1 e2 −C]    即参考平面自己的 Ti，是常量
Ti_i  = V_i T_ref
```

验证：`V_i T_ref = K_i R_i R_rᵀ K_r⁻¹ · K_r R_r [e1 e2 −C] = K_i R_i [e1 e2 −C] = Ti_i`。
一般情况下（`V_i ~ D_i⁻¹ A`），`T_ref ~ A⁻¹ [e1 e2 −C]`，A 在相乘时消掉，验证同样成立。
锚点确定 T_ref；只要已知 C（`C_z ≠ 0`），就有 `A ~ [e1 e2 −C] T_ref⁻¹`；不知道 C 时，仅凭锚点无法确定 A。

### 6. 实时估计（`atlas-vq-estimate`）

```text
新帧 → ALIKED 特征 → 与 V/Q map 里的特征点 q 匹配 → (x, q) 对
     → RANSAC，再用 Huber 损失的阻尼 Gauss-Newton 只求 V_query（8 个自由度），
       其他 V 和 q 全部固定
     → Ti_query = V_query · T_ref
```

它不输出 C、K、R。立体物体还需要 C，C 来自另外的标定（例如标定包中的
`C_world_metres`）。

### 7. 正式 Atlas ①：把未知的 A 换成物理方向

```text
S_k     = 机位 k 的参考旋转：z 轴指向球场中心，重力向上
Q_a     = 图块 a 相对机位的旋转
D_a     = S_k Q_a                       （K_a = I）
H[a←i]  = D_a⁻¹ D_i = Q_aᵀ S_kᵀ D_i     帧 → 图块
P_a E_0 = 球场在图块上的投影；固定不变，和当前帧的摇、俯仰、变焦无关
```

从全景图 ② 或 V/Q map ③ 到 Atlas ①：求出未知的 A（即标定），就有 `D_i = canon(A · V_i⁻¹)`。
射影产物由此升级为带物理角度、对应真实光心的 canonical Atlas。

## 总结

- V、q、T_ref 定义在同一个参考平面上，这种参考平面配准是三者共同的基础。
- 只保存 q、描述子、V、T_ref，就是 V/Q map ③，用来实时求 Ti。
- 按 V 把各帧像素贴回去，就是全景图 ②，用来查看和评估配准。
- 把参考平面标定成机位的物理方向，就是 ①，即正式的 `Atlas`，目前还没实现。
