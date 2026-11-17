# vchao 的 atlas、V、Ti 与 T_ref

**状态：** `draft` — 面向 chr 读者解释 vchao 的几何关系。以 vchao 自己的文档为准；
本页依据当前固定的子模块 `external/vchao`（`f209fc9`）。

**范围：** vchao 中「atlas」指什么、这个词被用于哪三种产物，以及联系 $K$、$R$、$C$、
$H$、$V$、$q$、$T_{\mathrm{ref}}$ 和 $T_i$（vchao 的 `Ti`）的几何关系的形式化表述，以及离线（建图）和在线（逐帧）算法。
本页不定义 chr 协议。英文版：[vchao-atlas.md](vchao-atlas.md)。

`external/vchao` 中的出处：`README.md`（`Atlas`）、`docs/camera-geometry.md`
（目录前的开篇部分，以及第 3、6、8、9 节）、`projective-atlas/README.md` 和
`atlas-vq-estimate/README.md`。

## 被叫做「atlas」的三样东西

| | vchao 的叫法 | 参考坐标系 | 内容 | 有没有像素 | 状态 |
|---|---|---|---|---|---|
| ① | **`Atlas`**（正式定义，canonical） | 机位的**物理方向**（已标定） | 每个机位一套方形图块：12 个水平方向 × 3 个俯仰，K = I，视场 90°；存图块的方向、静态背景图和覆盖范围 | 有 | 只有设计，还没实现 |
| ② | uncalibrated projective panorama（`projective-atlas` 的输出） | **任意选的一帧**，按 K = I 显示 | 同样的 36 个图块布局，内容是已配准各帧 RGB 的逐通道时间中值 | 有 | 已实现，用于显示和评估 |
| ③ | anchored **V/Q map**（`atlas-vq-estimate` 读的 manifest） | 建图时选定的参考平面 | 特征点 q 和描述子、每个视图的 V、T_ref | **没有** | 已实现，**实时求 Ti 用的就是它** |

- 严格意义上，vchao 的 `Atlas` 只指 ①。vchao 明确说明全景图 ② 有别于 canonical Atlas。
  V/Q map ③ 是另一种射影、未标定的产物，同样不是 canonical Atlas。② 和 ③ 都是下文
  定义 3 意义下的配准，但不一定是同一个：V/Q map 保存自己拟合出的 $V$ 和 $q$，两者必须配套使用。
- 「要有 atlas 才能实时估计 Ti」指的是 V/Q map **③**。
- chr 的业务文档（[calibration records](calibration-control.md)、
  [system architecture](system-architecture.md)）中的「atlas」是宽泛用法，指
  「每张都带有球场 Ti 的参考图像，加上光心 C」。它最接近 V/Q map 的 `views` 表
  （带 $V$ 和可选 $T_i$ 的帧），不是 vchao 的 canonical `Atlas`。

## 命名

- **V/Q map** 是 vchao（`atlas-vq-estimate`）对一组存储内容的命名：**V** 指各视图的单应
  $V_i : \Sigma \to \mathcal{I}_i$（`views` 表的 `v` 列），**Q** 指特征点（`landmarks` 表的 `q` 列），
  另外还有连接两者的观测，以及记录锚点和 $T_{\mathrm{ref}}$ 的 manifest。
- 本页把单个特征点记作 $q_k$，即 vchao 的 Q 中的一行。它是从光心出发的一条射线（定义 4），
  不是三维点，也不是球场坐标点。
- 定义 5 中的 $Q_a$ 是图块旋转（沿用 vchao `camera-geometry.md` 的记号），与特征点 Q 无关。
  规范变换矩阵记作 $M$。

## 形式化表述

### 0. 记号

- $\mathbb{P}^2 = (\mathbb{R}^3 \setminus \lbrace 0 \rbrace)/\mathbb{R}^\times$
  为实射影平面。对 $x, y \in \mathbb{R}^3 \setminus \lbrace 0 \rbrace$，
  $x \sim y$ 表示存在 $\lambda \neq 0$ 使 $x = \lambda y$；$[x]$ 表示 $x$ 所在的等价类。
- $\mathrm{PGL}(3) = \mathrm{GL}(3)/\mathbb{R}^\times$ 以 $[x] \mapsto [Mx]$
  作用于 $\mathbb{P}^2$（列向量）。由于 $3$ 是奇数，$\mathrm{SL}(3) \to \mathrm{PGL}(3)$
  是双射；$N(M) = M / \sqrt[3]{\det M}$（实立方根）就是 vchao 存储的行列式为 1 的
  代表元（vchao 的 `N(H)`；`atlas-vq-estimate` 称之为 `canon`，它不是
  `camera-geometry.md` 中使 $K_{33} = 1$ 的相机规范化 `canon`）。
- 世界为 $W = \mathbb{R}^3$，单位为球场米；地面为 $\Pi_0 = \lbrace Z = 0 \rbrace$，
  标准基为 $e_1, e_2, e_3$（向上为 $-e_3$，见[坐标契约](../server/docs/coordinates.md)）。
- 每帧 $i$ 的像平面 $\mathcal{I}_i \cong \mathbb{P}^2$ 使用已去畸变的 SNC 坐标。
  镜头畸变 $\psi_i$ 作用在投影之后，下文省略。
- $\varphi(x) = (x_1/x_3,\ x_2/x_3)$ 为去齐次化。
- 对 $W_f \times H_f$ 原生像素的帧，记 $s_f = \sqrt{W_f H_f}$，$\Lambda_f$ 把原生像素坐标
  $(u, v, 1)^\top$ 映为 SNC：

$$
\Lambda_f = \frac{1}{s_f} \begin{bmatrix} 2 & 0 & -W_f \\ 0 & 2 & -H_f \\ 0 & 0 & s_f \end{bmatrix}.
$$

- 对 $h \in \mathbb{R}$ 和 $C \in \mathbb{R}^3$，记

$$
G_h(C) = \begin{bmatrix} e_1 & e_2 & h\,e_3 - C \end{bmatrix} \in \mathbb{R}^{3\times 3},
\qquad \det G_h(C) = h - C_z .
$$

### 1. 相机

**定义 1（针孔相机）。** 第 $i$ 帧有内参 $K_i$（上三角，$f_x, f_y > 0$，
$(K_i)_{33} = 1$）、世界到相机的旋转 $R_i \in \mathrm{SO}(3)$ 和光心 $C_i \in W$。
其投影矩阵与方向矩阵为

$$
P_i = K_i R_i \begin{bmatrix} I & -C_i \end{bmatrix} \in \mathbb{R}^{3\times 4},
\qquad
D_i = R_i^\top K_i^{-1} \in \mathrm{GL}(3).
$$

像点 $[p] \in \mathcal{I}_i$ 反投影为射线 $\lbrace C_i + t\,D_i p : t \in \mathbb{R} \rbrace$。

**命题 1（平面单应）。** 令 $E_h : (X, Y, 1)^\top \mapsto (X, Y, h, 1)^\top$
为高度 $h$ 的水平面的嵌入，则

$$
A_{i,h} := P_i E_h = D_i^{-1} G_h(C_i),
$$

它属于 $\mathrm{PGL}(3)$ 当且仅当 $h \neq C_{i,z}$。球场单应为

$$
T_i := A_{i,0} = D_i^{-1} G_0(C_i) = K_i R_i \begin{bmatrix} e_1 & e_2 & -C_i \end{bmatrix}.
$$

*证明。* $\begin{bmatrix} I & -C \end{bmatrix} E_h = G_h(C)$，且 $K_i R_i = D_i^{-1}$。$\square$

**命题 2（由 $T_i$ 与 $C$ 投影三维点）。** 若 $C_{i,z} \neq 0$，则任意
$X \in W \setminus \lbrace C_i \rbrace$ 的投影为

$$
p \sim T_i\, G_0(C_i)^{-1} (X - C_i).
$$

*证明。* $P_i (X, 1)^\top = D_i^{-1}(X - C_i)$；由 $T_i = D_i^{-1} G_0(C_i)$ 且
$G_0(C_i)$ 可逆，得 $D_i^{-1} = T_i\, G_0(C_i)^{-1}$。$\square$

因此，渲染离开地面的物体恰好需要 $(T_i, C_i)$。

### 2. 机位

**定义 2（机位）。** 机位是一组光心相同的帧 $F$：对所有 $i \in F$，$C_i = C$。

**命题 3。** 对 $i, j \in F$，对应像点满足

$$
H_{j \leftarrow i} = \left[ D_j^{-1} D_i \right] \in \mathrm{PGL}(3),
$$

与场景深度无关。

*证明。* 对 $X = C + t\,D_i p$（$t \neq 0$），
$P_j (X, 1)^\top = D_j^{-1}(X - C) = t\, D_j^{-1} D_i\, p$。$\square$

### 3. 配准与参考平面

**定义 3（配准）。** 机位 $F$ 的一个配准是一族 $(V_i)_{i \in F} \subset \mathrm{PGL}(3)$，满足

$$
H_{j \leftarrow i} = V_j V_i^{-1} \qquad \text{对所有 } i, j \in F .
$$

映射 $V_i : \Sigma \to \mathcal{I}_i$ 的公共定义域 $\Sigma \cong \mathbb{P}^2$
称为**参考平面**（全景平面），并记 $U_i := V_i^{-1}$。

**命题 4（存在性与规范自由度）。**

1. 对任意 $A \in \mathrm{GL}(3)$，$V_i = [D_i^{-1} A]$ 是一个配准。
2. 同一机位的两个配准 $V, V'$ 满足：存在一个 $M \in \mathrm{PGL}(3)$，使所有
   $V'_i = V_i M$。全体配准构成一个右 $\mathrm{PGL}(3)$ 主齐性空间。
3. 任意配准都形如 $V_i = [D_i^{-1} A]$，其中 $A = D_r V_r$，$r \in F$ 任取。

*证明。* (1) $V_j V_i^{-1} = D_j^{-1} A A^{-1} D_i = H_{j \leftarrow i}$。
(2) 由 $V'_j V'^{-1}_i = V_j V_i^{-1}$ 得 $V_j^{-1} V'_j = V_i^{-1} V'_i$，为常量 $M$。
(3) $D_i^{-1} D_r V_r = H_{i \leftarrow r} V_r = V_i V_r^{-1} V_r = V_i$。$\square$

**几何意义。** 点 $[s] \in \Sigma$ 是过 $C$、方向为 $[A s]$ 的射线，因为
$p = V_i s$ 时 $D_i p = A s$。所以 $\Sigma$ 是光心处的射线空间，以 $A$ 坐标化；
它既不是俯视图，也不是三维空间。取根 $V_r = I$ 时 $A = D_r$，$\Sigma$ 即第 $r$ 帧的像平面。

**可辨识性。** 帧间单应 $\lbrace H_{j \leftarrow i} \rbrace$ 不能确定 $A$。
$s, s' \in \Sigma$ 两条射线的夹角为

$$
\cos \angle(s, s') = \frac{s^\top \omega_\Sigma\, s'}{\sqrt{s^\top \omega_\Sigma\, s}\ \sqrt{s'^\top \omega_\Sigma\, s'}},
\qquad \omega_\Sigma := A^\top A ,
$$

在未标定的配准中 $\omega_\Sigma$ 未知。

### 4. 特征点（V/Q map ③）

**定义 4（特征点）。** 特征点是点 $q_k \in \Sigma$，附带描述子
$d_k \in \mathbb{R}^{128}$。它在第 $i$ 帧中的观测满足

$$
x_{ik} \sim V_i\, q_k \qquad \text{（相差测量噪声）}
$$

$q_k$ 是一条射线，不是度量三维点；允许 $q_{k,3} = 0$，即 $\Sigma$ 仿射坐标下的无穷远点。

建图是估计问题

$$
\min_{\lbrace V_i \rbrace,\ \lbrace q_k \rbrace}\ \sum_{i,k} \rho\left( \left\lVert \varphi(V_i q_k) - x_{ik} \right\rVert \right),
$$

其解只在规范变换 $(V_i, q_k) \mapsto (V_i M, M^{-1} q_k)$ 下唯一。
vchao 只消费建好的 map，不记录建图方法。

### 5. 锚定球场

**命题 5。** 设 $C_z \neq 0$，$V$ 是一个配准。则存在唯一的
$T_{\mathrm{ref}} \in \mathrm{PGL}(3)$，使

$$
T_i = V_i\, T_{\mathrm{ref}} \qquad \text{对所有 } i \in F,
$$

且

$$
T_{\mathrm{ref}} = \left[ A^{-1} G_0(C) \right] = V_a^{-1} T_a \qquad \text{锚点 } a \in F \text{ 任取}.
$$

*证明。* 存在：$V_i A^{-1} G_0(C) = D_i^{-1} G_0(C) = T_i$。唯一：$V_i$ 可逆。
锚点形式由 $T_a = V_a T_{\mathrm{ref}}$ 得出。$\square$

$T_{\mathrm{ref}} : \Pi_0 \to \Sigma$ 把地面点映为过该点的射线。在规范变换
$V_i \mapsto V_i M$ 下，$T_{\mathrm{ref}} \mapsto M^{-1} T_{\mathrm{ref}}$，而
$V_i T_{\mathrm{ref}}$ 不变。

**命题 6（确定 $A$）。** 若 $C$ 已知且 $C_z \neq 0$，则

$$
A \sim G_0(C)\, T_{\mathrm{ref}}^{-1}, \qquad D_i^{-1} \sim V_i A^{-1},
$$

对 $V_i A^{-1}$ 做 RQ 分解，以 $\det R_i = 1$ 定符号、$(K_i)_{33} = 1$ 定尺度，
可唯一得到 $K_i$ 和 $R_i$。不知道 $C$ 时，仅凭锚点无法确定 $A$；需要对 $K$
加约束（零 skew、方形像素）才能同时解出 $C$ 和 $A$。

### 6. 实时估计（`atlas-vq-estimate`）

给定 V/Q map $\lbrace (q_k, d_k) \rbrace$、$T_{\mathrm{ref}}$ 和新的一帧 $f$：

**第 1 步（匹配）。** 将 $f$ 的 ALIKED 特征与描述子 $d_k$ 匹配（互最近邻加比值检验），
得到对应关系 $\lbrace (x_m, q_{k(m)}) \rbrace$，其中 $x_m$ 为原生像素坐标。

**第 2 步（后方交会）。** RANSAC 给出初值，再用阻尼 Gauss-Newton 求解单应后方交会
$\Sigma \to \mathcal{I}_f$（8 个自由度，至少 4 个对应点）。残差以原生像素计，
$\rho_3$ 为阈值 3 像素的 Huber 损失：

$$
\hat V_f = \operatorname*{arg\,min}_{V \in \mathrm{PGL}(3)}\ \sum_m \rho_3\left( \left\lVert \varphi\left(\Lambda_f^{-1} V q_{k(m)}\right) - x_m \right\rVert \right).
$$

**第 3 步（输出）。**

$$
T_f = N\left( \hat V_f\, T_{\mathrm{ref}} \right).
$$

map 中的 $V_i$、$q_k$ 和 $T_{\mathrm{ref}}$ 保持不变；不估计 $C$、$K$、$R$。

### 7. 正式 Atlas ① 与全景图 ②

**定义 5（canonical Atlas）。** 对光心为 $C$ 的机位，令 $u = (0, 0, -1)^\top$
为世界的向上方向，$O$ 为球场中心，定义

$$
b_3 = \frac{O - C}{\lVert O - C \rVert}, \qquad
b_1 = \frac{b_3 \times u}{\lVert b_3 \times u \rVert}, \qquad
b_2 = b_3 \times b_1, \qquad
S = \begin{bmatrix} b_1 & b_2 & b_3 \end{bmatrix} \in \mathrm{SO}(3).
$$

相对机位旋转为 $Q_a \in \mathrm{SO}(3)$ 的图块 $a$，是满足下式的配准：

$$
A = D_a := S\, Q_a \quad (K_a = I), \qquad
H_{a \leftarrow i} = D_a^{-1} D_i = Q_a^\top S^\top D_i ,
$$

球场在其上的投影为 $T_{\mathrm{ref}}^{(a)} = D_a^{-1} G_0(C)$，与任何帧无关。
此时 $\omega_\Sigma = A^\top A = I$ 且朝向已知，所以图块上的欧氏角就是物理射线夹角。

**定义 6（射影全景图）。** 取根 $r$ 使 $V_r = I$，并**假设** $\omega_\Sigma = I$。
旋转为 $R_c$ 的显示图块 $c$ 把图块点 $u$ 映到第 $i$ 帧的 $V_i R_c u$，其颜色为各帧颜色
$\mathrm{RGB}_i$ 的逐通道中值

$$
B_c(u) = \operatorname{median} \left\lbrace \mathrm{RGB}_i\left( V_i R_c u \right) : V_i R_c u \text{ 在第 } i \text{ 帧内} \right\rbrace .
$$

真实值一般为 $\omega_\Sigma = D_r^\top D_r = K_r^{-\top} K_r^{-1} \neq I$，所以图块上的角度不具物理意义。

**关系。** ② 和 ③ 都是 $A$ 未知的配准（定义 3）；① 是 $A$ 取已知物理旋转的配准。
把 ② 或 ③ 升级为 ①，所需条件正是命题 6 的条件。

## 算法

两个算法都沿用形式化表述中的记号。每一步注明 vchao（`f209fc9`）中对应的工具及其状态：
**已实现**（有 CLI）、**规划**（`docs/cli-contracts.md` 中的候选，还没有 CLI）、
**研究**（目前由 vchao CLI 之外的研究代码完成）。

### 离线算法：建图

**输入。** 同一机位的一段扫场视频：光心固定，通过摇、俯仰、变焦覆盖全场；标准球场模型
$\mathcal{M}(L, W)$（以球场米为单位的俯视图：场线、圆和球门；$L$、$W$ 可调，默认
$L = 105$，$W = 68$，场线局部和球门保持固定的标准尺寸）；可选的已声明镜头模型 $\psi$。

**输出。** V/Q map ③ $\big(\lbrace (q_k, d_k) \rbrace, \lbrace V_i \rbrace, T_{\mathrm{ref}}\big)$、
以及光心 $C$；可选输出 $L$、$W$、$K_i$、$R_i$ 和 $\psi$。

**步骤 O1（同一机位的帧）。** 把视频切成镜头，保留同一机位的帧 $F$（定义 2），并遮掉屏幕叠加图文。
`tracklet-shot`、`ui-mask` — 已实现。光心相同是调用方声明的假设，不做验证。

**步骤 O2（镜头）。** 命题 3 只在去畸变的 SNC 中严格成立。若声明了 $\psi$，先把原生像素 $\tilde u$
映为 $p = \psi^{-1}\left(\varphi(\Lambda_i \tilde u)\right)$。vchao 目前的 map 和估计器跳过这一步，
直接在原生（带畸变）像素上工作，视畸变为可忽略。没有 vchao 工具 — 研究。

**步骤 O3（纹理对应）。** 匹配静止的纹理（看台、广告牌、草皮、场线）；球员、球和叠加图文是外点。
相邻帧：ALIKED N16ROT 加 LightGlue 和 SEA-RAFT 光流（`frame-track`，已实现）。
相隔较远的帧（用于闭环）：EfficientLoFTR（`eloftr-match`，已实现）。

**步骤 O4（帧对单应）。** 对每个匹配上的帧对做鲁棒拟合（先 RANSAC，再用鲁棒损失 $\rho$）：

$$
\hat H_{j \leftarrow i} = \operatorname*{arg\,min}_{H \in \mathrm{PGL}(3)} \sum_m \rho\left( \left\lVert \varphi(H x_{i,m}) - x_{j,m} \right\rVert \right).
$$

`sparse-fit-h-*` — 规划。

**步骤 O5（配准）。** 求节点，可以由 H 图求解

$$
\lbrace \hat V_i \rbrace = \operatorname*{arg\,min}_{\lbrace V_i \rbrace} \sum_{(i,j)} d\left( \hat H_{j \leftarrow i},\ V_j V_i^{-1} \right)
$$

（`solve-g-*`，规划），也可以按第 4 节的目标函数与特征点联合求解（研究）。
取中间帧 $r$ 令 $V_r = I$ 固定规范（`projective-rebase`，已实现）。

**步骤 O6（供在线前端使用的特征库）。** 用**在线**前端（`aliked-n16`，不是 N16ROT）重新提取特征，
归到各特征点 $q_k$，$d_k$ 取成员描述子之和再归一化。只有支持数 $\geq 2$ 的特征点进入在线匹配库。
研究；`f209fc9` 中没有导出用的 CLI。

**步骤 O7（球场观测）。** 亮线点和线段（`ridge-lines`、`pitch-line-gaussian-hessian-points`，已实现）；
带已知模型编号的语义点、线、圆锥曲线（`pitch-detect-*`，规划）。

**步骤 O8（锚定球场）。** 用一帧 $a$ 对模型拟合（`fit-anchor-camera`，规划），令
$T_{\mathrm{ref}} = V_a^{-1} T_a$（命题 5）。再固定 $V_i$，用所有帧精化（`calibrate-shot-from-g`，规划）。
模型直线 $\ell$ 映为图像直线 $T_i^{-\top} \ell$，因此对第 $i$ 帧中观测到的线点
$p = (x, y, 1)^\top \in \mathcal{O}_{i,\ell}$：

$$
\left( \hat T_{\mathrm{ref}}, \hat L, \hat W \right) = \operatorname*{arg\,min}_{T_{\mathrm{ref}},\, L,\, W}\ \sum_i \sum_{\ell \in \mathcal{M}(L, W)} \sum_{p \in \mathcal{O}_{i,\ell}} \rho\left( \frac{\left| \left( (V_i T_{\mathrm{ref}})^{-\top} \ell \right)^\top p \right|}{\left\lVert \left( (V_i T_{\mathrm{ref}})^{-\top} \ell \right)_{1:2} \right\rVert} \right).
$$

**步骤 O9（光心）。** 地面特征无法确定 $C$（命题 2），离地特征可以。设球门点 $X_g$
（立柱和横梁，高 $2.44$ m）在第 $i$ 帧中观测到的位置为 $y_{i,g}$，并固定 $T_i = V_i T_{\mathrm{ref}}$：

$$
\hat C = \operatorname*{arg\,min}_{C}\ \sum_{i,g} \rho\left( \left\lVert \varphi\left( T_i\, G_0(C)^{-1} (X_g - C) \right) - y_{i,g} \right\rVert \right).
$$

`pitch-centre-fit` — 已实现（球门点和线，3 个自由参数）。

**步骤 O10（可选：标定）。** $A \sim G_0(\hat C)\, T_{\mathrm{ref}}^{-1}$，再由 RQ 分解得到
$K_i$ 和 $R_i$（命题 6），进而生成正式 Atlas ①（`make-canonical-atlas`，规划）。

**步骤 O11（质量检查）。** 渲染全景图 ②（`projective-atlas`）、计算 RGB 重投影误差
（`h-graph-eval-rgb-reproject`）和轨迹一致性（`eval-track-consistency`）— 已实现。

**步骤 O12（导出）。** 写出 V/Q map（`landmarks`、`observations`、`views` 三张表，以及记录锚点和
$T_{\mathrm{ref}}$ 的 manifest，见 `atlas-vq-estimate/README.md`），连同 $\hat C$。研究；`f209fc9` 中没有导出用的 CLI，
`atlas-vq-estimate` 只负责读取 map。

### 在线算法：逐帧

**输入。** V/Q map、$C$，以及带源时间 $t_f$ 的实时帧 $f$。
**输出。** 按顺序为每一帧给出 $T_f$ 和状态 `fresh`、`interpolated`、`reused` 或 `unavailable`。

**步骤 I1（调度）。** `pitch-analysis` 在 $N = 8$、$S = 4$ 的网格上对计划的目标帧做估计
（每 8 帧 2 次新鲜估计），只有一个执行槽，优先最新的目标。被保留超过 $500$ ms 的帧立即按当前选择输出。
已实现。

**步骤 I2（特征）。** 把帧缩放到长边 1024，运行 `aliked-n16`（阈值 $0.2$，NMS 半径 $2$，最多
$20000$ 个点）；关键点换算回原生像素 $x$，附带描述子 $\delta$。已实现（`atlas-vq-estimate`，I3 到 I7 同）。

**步骤 I3（匹配）。** 把 $\delta$ 与特征库 $\lbrace d_k \rbrace$ 匹配：互为最近邻，双向平方距离比
$< 0.64$，平方距离 $\leq 0.49$。得到 $\lbrace (x_m, q_{k(m)}) \rbrace$。

**步骤 I4（RANSAC）。** 最多 $2048$ 个四点假设（种子 $0$），内点阈值 $3$ 个原生像素，
一致率达到 $90\%$ 时提前停止。

**步骤 I5（精化）。** 在内点上做最多 8 次阻尼 Gauss-Newton 迭代，只有 $V$ 是自由的：

$$
\hat V_f = \operatorname*{arg\,min}_{V \in \mathrm{PGL}(3)}\ \sum_m \rho_3\left( \left\lVert \varphi\left(\Lambda_f^{-1} V q_{k(m)}\right) - x_m \right\rVert \right).
$$

**步骤 I6（接受条件）。** 只有满足以下条件才接受：$3$ px 内至少 $12$ 个内点，两个方向的原生跨度都至少
$10\%$，归一化协方差的最小特征值大于 $10^{-4}$；否则状态为 `no_features`、`no_matches` 或
`low_support`，不产生位姿。

**步骤 I7（新鲜输出）。**

$$
T_f = N\left( \hat V_f\, T_{\mathrm{ref}} \right).
$$

**步骤 I8（估计之间的帧）。** 在最近的两个有效端点 $A$、$B$ 之间，在矩阵对数空间插值（LogH）：

$$
\Delta = N\left( T_B T_A^{-1} \right), \qquad
\alpha = \frac{t_f - t_A}{t_B - t_A}, \qquad
T_f = N\left( \exp\left( \alpha \log \Delta \right) T_A \right),
$$

其中 $\log$ 为主实对数。没有前后端点时，沿用最近一次有效的 $T$（`reused`），或报告无（`unavailable`；
绝不用单位矩阵代替）。已实现（`pitch-analysis`，来自 `pitch-live`）。

**步骤 I9（chr 渲染）。** chr 从 VchaoFrame 元数据收到 $T_f$。由于 I2 到 I5 拟合的是真实（带畸变）
的原生像素，$T_f$ 已经直接对应真实画面，chr 按针孔把世界点 $X$ 渲染在

$$
p = \varphi\left( T_f\, G_0(C)^{-1} (X - C) \right)
$$

处（命题 2），并在节目截止时间前合成（`server/docs/tracking-worker.md`、`server/docs/gst-core.md`）。
只有当生产方的 $T$ 对应去畸变画面并提供了镜头时，才使用 $p = \psi\left(\varphi(\cdots)\right)$
（chr 的 `lens` 和 `frame_warp`，或 CLI 指定的镜头）。

## 总结

- $V$、$q$、$T_{\mathrm{ref}}$ 定义在同一个参考平面 $\Sigma$ 上，即机位光心处的射线空间；
  这种配准是三种产物共同的基础。
- 只保存 $q$、描述子、$V$、$T_{\mathrm{ref}}$，就是 V/Q map ③，用来实时求 $T_f$。
- 按 $V$ 把各帧像素贴回去，就是全景图 ②，用来查看和评估配准。
- 把 $A$ 定为机位的物理方向，就是 ①，即正式的 `Atlas`，目前还没实现。
