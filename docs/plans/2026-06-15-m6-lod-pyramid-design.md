# M6 大图 LOD 金字塔设计

> 设计文档（spec）。实现计划由后续 writing-plans 按 Phase 拆分产出，交 codex 逐步实现、fable 评审。
> 状态：设计待评审 / 方向已定（方案 A）。

## 背景与目标

基准图：19200×16384、~900MB、24bpp BMP（江西造币厂钞票质检样本），走大图 tile/canvas 路径。

当前两个画质/性能问题（已用埋点定位，证据充分）：

- **Q1 低倍率摩尔纹**：低于 ~19% 缩放时前端只画 preview、不画 tile，而 preview 是**最近邻**降采样（`BmpReader.read_region(_parallel)` 用 `nearest_source_index`；`fast_preview_rgba` 同），9.4× 缩小细密纹路 → 严重摩尔纹。19% 阈值来自 `LargeImageCanvas.vue` `needTiles` 的 `visibleTileCount <= floor(256*0.7)=179` 低倍率保护（代码注释已标 `TODO M6`）。
- **Q2 拖动闪影 + 卡顿**：≥19% 拖动时 ① 每帧 `clearRect` 后重画 preview + 视口内**上百张** tile（每张 `save/setTransform/drawImage/restore`）→ 卡顿；② 新进入视口的 tile 未加载时露出模糊 preview，tile 异步到达再变清晰 → 模糊鬼影一闪。

**目标**：任意缩放都干净（无摩尔纹）、拖动顺滑、切换不闪；深度放大仍能看到 1:1 精确像素（QA 查缺陷）。

## 关键决策（已定）

- **方案 A**：预生成 LOD（mip）金字塔 + 粗层优先 + 邻居预建。否决"按 tile 实时降采样"（低倍率单 tile 要读极大源区域，又慢又难抗锯齿）。
- **分期**：Phase 1A（MVP）→ Phase 1B（完整）→ Phase 2（邻居预建）。每期独立可交付。
- **旋转**：本期纳入 rotation=90/180/270 的 LOD tile 渲染。
- **典型用法**：用户固定 ~19–20% 缩放、逐张切换查看（"固定倍率翻图"是首要场景）。该倍率 @dpr2 选中 **level 1**。

> codex 只读审核结论：三条核心决策方向认可，但均"需修正"。本设计已并入其修正与"落地风险 Top3"。

---

## §1 层级模型与坐标

- **z 语义**：复用现有协议 `picsee://tile/{session}/{z}/{x}/{y}`（现 z 被忽略）。`z=0` = 原图最细，`z=L` = 缩小 2^L。tileSize 维持 512（**层内像素**）。
- **层尺寸（必须逐层向上取整）**：`levelW(L)=ceil(width/2^L)`，`levelH(L)=ceil(height/2^L)`。**禁止**用 floor 或 `tileSize*2^L` 反推，否则逐层累积偏移、右/下边越界。
- **maxLevel**：最小的 L 使 `max(levelW,levelH) <= tileSize`（整层 ≤ 单 tile）。19200×16384 → L=6，共 7 层（0–6）。
- **per-level tile 网格**：`tilesX(L)=ceil(levelW(L)/512)`，`tilesY(L)=ceil(levelH(L)/512)`；边缘 tile 的实际宽/高裁剪到该层剩余像素（`min(512, levelW-512*tx)`）。
- **前端选层 + 迟滞**：`effScale = zoom*dpr`（device px / 原图 px）。基础选层 `L0 = clamp(floor(log2(1/effScale)), 0, maxLevel)`；为防平滑缩放/触控板在临界值抖动，引入**迟滞**：仅当当前层物理采样率 `< 0.45` 才切更粗层、`> 1.1` 才切更细层（保留上一帧 L，按阈值迁移）。
  - 19–20%@dpr2（effScale≈0.4）→ L=1；effScale≥1 → L=0（原图 1:1）。

## §2 后端建塔与存储

- **level0 不预生成**：z=0 tile 仍由 `BmpReader.read_region` 从**原文件** 1:1 读取（无缩放、无锯齿）。非 BMP 沿用既有临时栅格（`ensure_raster`）作 level0；**系统解码格式（HEIC/TIFF/RAW）当前标记不可 tile，本期保持不可 tile（preview-only），不纳入金字塔**。
- **level1..N 盒式 mip 链**：`levelL+1` 由 `levelL` **2×2 盒式平均**逐行流式降采样，写临时 BMP 栅格（复用 `write_temp_bmp_raster`，top-down BI_RGB）。要点：
  - **盒式平均**（非最近邻）是消摩尔纹的根本；
  - **奇数边**：右/下边只有 1 或 2 个子像素时，对**实际存在**的 1/2/4 个子像素求平均；
  - **32-bit alpha**：用**预乘 alpha**平均后再还原，避免边缘发暗/发亮（24-bit 无 alpha 直接平均）；
  - **流式低内存**：每次只持 2 行源 + 1 行目标。
- **粗层优先（措辞修正）**：mip 链依赖决定**必须先建 level1**（要读全图一次）。建塔顺序 = `1 → 2 → … → 当前选中层 → 其余更粗层`；即"先把当前视野所在层及其依赖链建出来"。19–20% 选 L1，恰好一步到位；fit-window 需要更粗层时，L2…在 L1 之上派生很快。
- **出图不变**：仍走现有秒开（先显示现有 nearest 预览作占位）；对应层建好后该层 tile 接管，低倍率变干净。
- **建塔调度器（新增）**：全局**单一 IO 并发（默认 1）**、按 `(图, 目标层)` 去重、**支持取消**（session 关闭/被逐出即取消其在途建塔）、当前图优先于邻居。
- **层就绪 manifest**：每个塔维护 `{level: {width,height,path,ready}}`；建塔输出先写临时文件、完成后**原子 rename**，再置 ready，避免读到半成品。
- **磁盘缓存键**：`canonical_path + file_size + mtime + tileSize + 算法版本 + 色彩处理版本`（不能只用 `(path,mtime)`）。
- **磁盘预算（修正）**：现 `diskCacheLimitMB(2048)` **仅用于 thumbnails/**，并非共享预算。金字塔需**独立配额**（新增设置项，或明确从 2048 划出专用额度）。下采样各层合计 ≈ level0 的 1/3 ≈ ~300MB/图。淘汰**必须保护活跃 session 与正在构建的塔**；缩略图那套单目录扫描淘汰不能直接复用到"多文件塔目录"，需新淘汰器。
- **生命周期分离（修正）**：
  - **session 临时栅格**（非 BMP level0）：随 session 关闭/逐出删除（现状）。
  - **持久金字塔塔**（Phase 2 邻居预建产物）：**无 session**，按磁盘缓存键管理、独立淘汰，不随某个 session 删除。两者分开管理，互不误删。

## §3 前端渲染（LargeImageCanvas.vue）

- **删 `visibleTileCount<=179` 保护**：但**仅在目标层可用且失败可重试之后**删除（否则层没建好时退化为白屏/兜底）。
- **tile 数量（措辞修正）**：选层后视口内 tile 数大幅下降，但**并非恒定 9–16**——取决于视口物理尺寸/tileSize/层 clamp；Retina 大窗口仍可能数十张，但远少于现在上百张。
- **per-level 渲染**：请求 `tile/{session}/{L}/{x}/{y}`；可见范围、层网格、边缘尺寸**全部按 level 重算**；tile 在原图坐标系按 `2^L` 放置（复用 `applySourceTransform`，tile 内再 ×2^L）。**边缘按 1px gutter/apron 处理接缝**，而非简单放大目的矩形。
- **批量 transform**：每层只 `save + setTransform` **一次**，循环内仅 `drawImage`（消除每 tile save/restore 开销）。
- **消闪影**：画目标层 L 前，**先铺已缓存的更粗层**（L+1 或当前已缓存最粗层，仅覆盖其已缓存区域）作背景，再叠 L；preview 仅作最底兜底。目标层到达后覆盖粗层。→ tile 未到时露出的是"更粗但清晰"的层，不再是模糊鬼影。
- **可重试失败（修正硬伤）**：现 `failedTiles` 是**永久负缓存**——后台层未建好时请求失败会导致该 tile 整 session 不再重试。改为**带退避的可重试**（区分"层未就绪/网络"短期失败 与 "真越界"永久失败）。
- **请求优先级**：可见粗层 → 可见目标层 → 目标层预取（外扩 prefetchRadius）。
- **旋转支持（本期纳入）**：删除 `needTiles` 的 `rotation===0` 限制；`computeVisibleRect`/tile-range 计算改为**旋转感知**（按 rotation 反算可见原图矩形）；`applySourceTransform` 已含各旋转分支，tile 在原图坐标放置即随之旋转。
- **前端 tile 缓存**：key 含 z（`{session}:{z}:{x}:{y}`，cache/loading/failed 三个集合同步改）；容量改为**按估算解码字节**淘汰（512×512×4≈1MB/张，256 张≈256MB），与后端内存预算共同核算。

## §4 协议 / 接口契约变更

- **tile 协议启用 z**：`handle_tile_request` 不再忽略 z；按 level 选源（L=0 原文件，L≥1 对应层栅格）、按 **per-level** `tiles_x/tiles_y` 做越界校验；`TileKey=(session_id, z, x, y)`（类型已含 z，落实使用）。
- **层未就绪**：请求尚未建好的层 → 返回**可重试**状态（如 HTTP 425/503 语义），前端退避重试，**不写永久负缓存**。
- **`open_large_image` 返回**新增 `maxLevel`（前端据 width/height/tileSize 也可自算，二者一致）；可选返回初始 ready levels。
- **层就绪通知**：后端通过事件或轻量查询暴露 ready levels，前端据此触发重绘/重试（避免轮询风暴）。
- **新增命令（Phase 2）**：`prefetch_large_pyramid(paths)` —— 子线程为邻居预建持久塔（至少 level1），受建塔调度器限流与去重。

## §5 错误处理与兜底

- 层未就绪：粗层兜底 + 退避重试，绝不永久失败。
- 真越界 tile：永久跳过（保留）。
- 建塔失败（IO/磁盘满）：降级为"该图仅 preview + level0 按需"，记录日志，不崩。
- session 关闭/逐出：取消其在途建塔；持久塔不随之删除。
- 磁盘配额超限：先淘汰非活跃持久塔；仍不足则跳过邻居预建（仅保当前图）。

## §6 测试与验收

- **单元**：层尺寸 `ceilDiv`、边缘 tile 裁剪、盒式平均（含奇数边/32bit 预乘 alpha）逐字节正确性、选层公式+迟滞、tile 越界 per-level 校验、缓存键含 z。
- **集成（`#[ignore]` 大文件）**：生成中等 BMP 走 open→建塔→各层 tile，断言层间像素关系、接缝无缝/无重叠。
- **性能验收目标（目标，非设计事实）**：
  - level1 建塔（19200×16384，release，M 系列）作为**实测基准**记录（设计中"~0.7s"仅为预估，以实测为准）；
  - 19–20% 拖动稳定 ≥ 50fps（埋点帧间隔）；
  - 切换到已预建邻居（Phase 2）→ 干净视图 < 100ms 出现。
- **手测**：基准目录 `白光-Back`，低倍率无摩尔纹、拖动无闪无卡、深度放大像素精确、旋转后同样清晰。

## §7 分期范围

> 实现状态（2026-06-16）：Phase 1A / 1B / 2 **代码均已完成、测试通过并合入 main**；**GUI 视觉验收待用户在 `白光-Back` 实机确认**（摩尔纹/拖动/旋转/切换秒净/缓存复用，只能人眼验）。

- **Phase 1A（MVP，先修 Q1 + 拖动）**：
  - 后端：启用 z 协议 + per-level 校验；建 **level1**（盒式，奇数边/预乘 alpha 基础正确）；建塔后台 + 取消 + 就绪 manifest。
  - 前端：按层渲染（仅 0/1 两级也可）、批量 transform、粗层兜底、**可重试失败**、tile key 含 z。
  - 产出：低倍率摩尔纹消除，~19–50% 拖动显著改善。
- **Phase 1B（当前图完整）**：完整 2..maxLevel；选层迟滞；接缝/gutter；旋转 LOD；前端按字节淘汰；性能验收。
- **Phase 2（切换秒净）**：持久磁盘塔 + 独立配额淘汰器 + `prefetch_large_pyramid` 邻居预建（±N，默认 ±1，限流）。
- **后续 follow-up（评审记录，未做）**：按需只建到被请求层而非全塔（S1）、evict LRU 改显式访问时间（S2）、symlink/TOCTOU 加固（S3）。

## §8 落地风险 Top3（codex）与缓解

1. **层就绪竞态 / 生命周期**：永久负缓存、关闭后后台仍写、淘汰在用塔。
   → 可重试失败 + 原子 rename + 就绪 manifest + 关闭即取消建塔 + 淘汰保护活跃/在建。
2. **奇数尺寸 / 边缘坐标**：丢像素、右下越界、接缝/重叠。
   → 统一 `ceilDiv` + 边缘裁剪 + 盒式按实际子像素 + 1px gutter；逐字节单测覆盖。
3. **IO / 缓存预算失控**：当前图 + 邻居 + 缩略图 + level0 临时栅格分别计费，实际远超 2048MB/512MB。
   → 金字塔独立磁盘配额 + 单一建塔 IO 并发 + 前端按字节淘汰 + 邻居预建超额即跳过。

## 复用的既有基建

`write_temp_bmp_raster` / `BmpReader.read_region(_parallel)` / tile LRU（`LargeImageState`）/ session 生命周期与临时栅格清理 / 缩略图磁盘淘汰范式（需改造为多文件塔目录）。
