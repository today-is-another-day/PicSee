# PicSee 图片查看器需求修订 v0.3

> 本文档是对《PicSee 图片查看器需求初稿 v0.2》的**增量修订**，仅记录新增章节与对原章节的修订条目，未提及的内容以 v0.2 为准。
>
> 修订来源：v0.2 需求评审（2026-06-12），含 4 项已确认的范围决策与若干技术风险修订。

---

## A. 新增章节

### A1. 导航窗（Navigator / Minimap）需求 【纳入 MVP】

v0.2 缺失。导航窗是「放大哪块看哪块」核心体验的一部分（对标 WPS 看图放大时右下角的导航小窗）。

#### A1.1 功能要求

- 主画布缩放超过 fit-window 级别时，自动显示导航窗；
- 导航窗显示全图缩略内容，**直接复用当前图片的 preview**，不做额外解码；
- 导航窗内绘制视口框（viewport rect），表示主画布当前可见区域；
- 双向联动：
  - 拖动 / 点击导航窗中的视口框 → 主画布跳转到对应区域；
  - 主画布拖拽 / 缩放 → 视口框实时同步；
- 缩放回 fit-window 或更小时自动隐藏；
- 支持设置：常显 / 自动 / 隐藏（归入「查看设置」）。

#### A1.2 实现约束

- 页面结构（v0.2 §23.2）新增组件：

```
AppLayout
 ├── TopToolbar
 ├── ThumbnailSidebar
 ├── ImageCanvasViewer
 │     └── NavigatorOverlay   ← 新增
 ├── StatusBar
 └── SettingsModal / SettingsDrawer
```

- 视口状态复用 `useViewerStore`（缩放比例、offset、viewport），不引入新 store；
- 导航窗渲染用低分辨率 preview（如最长边 ≤ 512 的降采样），避免高 DPI 下额外内存开销；
- 普通图片与大图（tile 模式）统一支持导航窗，行为一致。

#### A1.3 默认规格

- 位置：主画布右下角，距边缘 16px；
- 尺寸：长边 200px（可配置 160 / 200 / 240）；
- 视口框：1px 边框 + 半透明遮罩区分可见 / 不可见区域。

---

### A2. 打开入口与文件关联 【纳入 MVP】

v0.2 仅有 Cmd+O 打开文件与打开目录。补充以下入口，这是「能否被当作默认看图工具」的关键。

#### A2.1 功能要求

- **文件关联**：macOS 注册 `CFBundleDocumentTypes`，声明全部第一阶段支持格式，使 Finder 中可「打开方式 → PicSee」并可设为默认；
- **双击打开单张图片**：
  - 立即显示该图片（最高优先级）；
  - 后台自动扫描该图片所在目录，构建图片列表，支持左右切换；
  - 当前图片在列表中的 index 正确定位；
- **拖放打开**：拖图片文件 / 目录到窗口或 Dock 图标即打开；
- **二次打开**：应用已运行时再次 open 文件，复用现有窗口切换图片（单实例行为）；
- 命令行 `open -a PicSee xxx.jpg` 等价于双击打开。

#### A2.2 实现约束

- Tauri 2.0 使用 deep-link / `RunEvent::Opened`（macOS Apple Event）接收打开请求；
- 启动参数（argv）与 Apple Event 两条路径都要处理；
- 单图打开复用目录扫描策略（v0.2 §11.2），当前图片最高优先级，目录扫描不阻塞首图显示。

---

### A3. 基础文件操作 【纳入 MVP】

v0.2 MVP 不含任何文件操作。确认纳入以下浏览流程必需的操作（仍不含图片编辑）：

| 操作 | 行为 | 快捷键 |
|------|------|--------|
| 视图旋转 | 仅旋转显示，**不写回文件** | R（顺时针 90°）/ Shift+R（逆时针 90°） |
| 删除 | 移到废纸篓（`trash`，非永久删除），删除后自动跳到下一张并更新列表 | Delete / Cmd+Backspace |
| 在 Finder 中显示 | Reveal in Finder | Cmd+Shift+R 或右键菜单 |
| 复制文件 | 复制文件到剪贴板（可粘贴到 Finder） | Cmd+C |
| 复制路径 | 复制完整路径文本 | 右键菜单 |

要求：

- 删除必须走系统废纸篓 API，可恢复；删除前不强制确认弹窗（可在设置中开启确认）；
- 视图旋转状态在切换图片后重置；旋转与缩放 / 拖拽 / 导航窗联动正确；
- 所有操作提供右键上下文菜单入口。

---

### A4. IPC 与二进制传输设计约束 【架构级，必须遵守】

v0.2 §26 的 command 草案返回 `ImageBytes`，未约定通道。**Tauri 2.0 默认 IPC 走 JSON 序列化，传输 MB 级像素数据会成为性能瓶颈**（单个 512×512 RGBA tile ≈ 1MB，4096 preview ≈ 64MB），必须遵守：

#### A4.1 通道要求

- `get_preview` / `get_tile` / `get_thumbnail` 返回二进制时，必须使用以下方案之一，**禁止默认 JSON IPC**：
  1. Tauri 2.0 raw payload：`tauri::ipc::Response`（二进制直传）；
  2. 自定义 URI 协议：`register_uri_scheme_protocol`（如 `picsee://tile/{image_id}/{generation}/{z}/{x}/{y}`），前端用 `fetch` / `<img>` 拉取，天然获得浏览器并发与缓存。
- 建议：preview / thumbnail 走自定义协议（可被 `<img>` 直接消费）；tile 走 raw payload 或自定义协议，开发期做一次 benchmark 决定。

#### A4.2 传输格式

- tile 传输格式需要权衡（开发期 benchmark 后定）：
  - raw RGBA：零编解码开销，但带宽大（512² ≈ 1MB）；
  - WebP（无损或 q≈90）/ PNG：体积小，但 Rust 侧编码 + WebView 侧解码各有成本；
- preview 建议直接编码为 WebP/JPEG 传输（一次性传输，体积敏感）。

---

## B. 对 v0.2 原章节的修订条目

### B1. 性能指标补充（对应 §2、§29）

v0.2 仅对大 BMP 有「2 秒内显示 preview」指标。补充普通图片与启动链路指标：

- 普通 JPG（≤ 20MP），应用已运行：从触发打开到内容可见 **≤ 300ms**；
- 冷启动双击打开一张普通 JPG：从双击到内容可见 **≤ 1s**（M 系列芯片基准）；
- 启动策略：窗口先显示（骨架 / 上次背景色），图片异步填充，不得白屏等待解码；
- 目录内左右切换（已预加载命中）：**≤ 100ms** 内容可见。

### B2. EXIF orientation（对应 §5、§12、§13）【必做】

v0.2 未提及，属必踩坑：

- JPG / TIFF / HEIC / RAW 的 EXIF orientation 必须在**缩略图、preview、tile 三条链路统一处理**，保证方向一致；
- tile 坐标系按「应用 orientation 之后」的图像坐标定义，避免前端再做坐标变换；
- 验收：含 orientation=3/6/8 的测试图在缩略图、主图、导航窗中方向一致且正确。

### B3. ICC 色彩处理（对应 §5、§28.2）【纳入 MVP】

v0.2 将色彩管理完全排除。修订为分两级：

- **第一阶段（MVP）**：自研解码路径（image-rs / libvips / 自研 BMP reader / TIFF）检测嵌入 ICC profile，若存在则转换到 sRGB 再输出（lcms2 / libvips 内置能力）；无 profile 的按 sRGB 假定；
- **后续**：完整色彩管理（匹配显示器 profile、P3 输出）；
- 走系统解码的格式（HEIC via ImageIO）由系统处理色彩，无需额外转换；
- 理由：Mac 普遍为 P3 广色域屏，完全不处理 ICC 的摄影图偏色明显，对看图软件是观感硬伤。

### B4. TIFF 第一阶段范围修正（对应 §7）

v0.2 把 LZW / Deflate 放到后续增强，但现实中绝大多数 TIFF 都是 LZW / Deflate 压缩，原范围实际为空。修正：

- **第一阶段**：未压缩 + LZW + Deflate（Rust `tiff` crate 原生支持，成本低）；
- **后续**：JPEG-in-TIFF、Pyramidal TIFF、BigTIFF、多页切换、GeoTIFF、OME-TIFF。

### B5. RAW 放大上限 UX（对应 §9）

第一阶段不做 demosaic，embedded preview 分辨率常低于原图（部分机型仅半尺寸），「放大看局部高清」对 RAW 不可行。明确：

- RAW 放大上限 = embedded preview 原生分辨率（即 100% 指 preview 的 100%）；
- UI 在状态栏 / 角标提示「RAW 预览模式」，并显示 preview 实际分辨率，避免用户误判画质；
- 后续接入 libraw 完整解码后解除该限制。

### B6. HEIC 解码决策（对应 §8）

- **第一阶段仅走 macOS 系统解码（ImageIO / CoreGraphics）**，不引入 libheif；
- Windows / Linux 的 HEIC 方案（libheif 编译、HEVC 专利、包体积）在第二阶段单独立项评估，不阻塞架构；
- 系统无法解码的 HEIC 变体给出明确错误提示（i18n 文案）。

### B7. GIF 直通路径（对应 §5、§6.1）

v0.2 写「静态预览优先，动图播放后续增强」。注意 WebView 的 `<img>` 原生就能播放 GIF——走 Canvas 管线反而会把动图退化为静态。修正：

- GIF（及后续动画 WebP）保留 `<img>` 直通渲染路径，MVP 即支持动图播放；
- 缩略图取首帧即可；
- GIF 不进 tile 管线（GIF 尺寸上限本身很小，无大图场景）。

### B8. 内存预算统一管理（对应 §20、§21、§29.2）

- preview 内存不可忽略：4096 边长 RGBA ≈ 64MB/张，叠加「前后各 1 张大图 preview 预加载」即可到 200MB 量级；
- **preview、缩略图、tile 纳入同一全局内存预算**（`memoryCacheLimitMB` 统一约束），而非各自独立 LRU；
- 磁盘缓存补充淘汰策略：LRU + 总量水位（达到 `diskCacheLimitMB` 的 90% 触发清理至 70%）；
- 验收「内存长期稳定」据此可测：浏览 100 张混合图片后 RSS 不持续增长，回落到预算附近。

### B9. HiDPI / Retina（对应 §13、§18）

- 所有 Canvas 渲染按物理像素计算：canvas 尺寸 × `devicePixelRatio`；
- 「适应窗口」时选取 preview / tile 层级以**物理像素**为基准，否则 Retina 屏上 fit-window 显示发糊；
- 导航窗、缩略图同样适用。

### B10. 大图判定阈值调整（对应 §13.1、§19.1）

- 1 亿像素解码后即 ≈ 400MB RGBA，建议默认像素阈值由 100,000,000 下调为 **50,000,000**；
- 或改为按「解码后内存估算 = 宽 × 高 × 4」判定（阈值如 200MB），语义更直接；
- 其余规则（文件 > 300MB、单边 > 12000、BMP 激进规则）维持 v0.2。

### B11. 状态栏内容明确（对应 §17.3、§23.2）

状态栏显示：文件名、序号（x / y）、原始分辨率、当前缩放百分比、文件大小；RAW 预览模式时附加模式角标（见 B5）。

### B12. 快捷键补充（对应 §22.1）

在 v0.2 默认快捷键基础上新增：

```
R              : 顺时针旋转 90°（视图）
Shift + R      : 逆时针旋转 90°（视图）
Delete / Cmd+⌫ : 删除到废纸篓
Cmd + C        : 复制文件
Cmd + Shift + R: 在 Finder 中显示
```

---

## C. MVP 必含清单更新（对应 §28.1）

在 v0.2 §28.1 基础上**新增**：

- 导航窗（minimap）及视口联动；
- 文件关联（CFBundleDocumentTypes）+ Finder 双击打开 + 单图自动加载所在目录；
- 拖放打开文件 / 目录；
- 视图旋转（不写文件）；
- 删除到废纸篓；
- 在 Finder 中显示、复制文件 / 路径；
- EXIF orientation 全链路处理；
- ICC → sRGB 转换（自研解码路径）；
- GIF 动图播放（`<img>` 直通）；
- TIFF：未压缩 + LZW + Deflate；
- IPC 二进制通道（raw payload / 自定义协议）；
- 全局统一内存预算。

§28.2「暂不包含」维持不变，并明确补充：完整色彩管理（显示器 profile）、RAW demosaic、HEIC 跨平台解码 仍属暂不包含。

## D. 验收标准增量（对应 §29）

- 双击 Finder 中图片可直接打开，左右键可切换同目录图片；
- 普通 JPG 热启动打开 ≤ 300ms、冷启动 ≤ 1s、切换 ≤ 100ms（预加载命中）；
- 放大后导航窗出现，拖动视口框与主画布双向联动正确；
- orientation=3/6/8 测试图方向正确，缩略图 / 主图 / 导航窗一致；
- 带 ICC profile（如 Display P3、Adobe RGB）的 JPG 显示不明显偏色（对照系统预览）；
- LZW / Deflate 压缩 TIFF 可正常打开；
- GIF 动图可播放；
- R / Shift+R 旋转生效且不修改文件；Delete 移入废纸篓且列表正确更新；
- Retina 屏下 fit-window 显示清晰（无半分辨率发糊）；
- 浏览 100 张混合图片（含大 BMP）后内存回落到预算附近，不持续增长。
