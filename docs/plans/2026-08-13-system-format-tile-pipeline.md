# 系统格式（AVIF/TIFF/HEIC/RAW）接入分块管线 — 实现记录

> 2026-08-13。触发场景：105MB / 14386×10481 的 AVIF 打开约 10s、拖动缩放糊且卡顿。

**Goal:** 让系统解码格式与 PNG/BMP 走同一条大图管线（预览 → 分块 → 金字塔 → 预取），使打开耗时与常驻内存和源图像素数解耦。

## 根因

| # | 问题 | 位置（改前） |
|---|---|---|
| 1 | 判定"能否交给 WebView `<img>` 直显"只看文件大小（默认阈值 300MB），而 AVIF/HEIC 压缩比极高：105MB 即 1.5 亿像素，喂给 `<img>` 会让 WebView 主线程整图解码、卡顿秒级并在 WebKit 内驻留数百 MB | `policy.rs` `render_via_webview` |
| 2 | 走大图路径时，系统格式一律 `tileable=false` —— 只有一张 ≤2048 边的预览被拉伸，既没有分块也没有金字塔，放大必糊，且前端 `LargeImageCanvas` 的粗层→细层渐进逻辑（`if (!session.tileable) return`）完全不生效 | `session.rs` `open_large_image` |
| 3 | 解码中转用 PNG：1.5 亿像素图输出 PNG 6.3s / 179MB，再 `image::open` 整图读回本进程（≈350MB RGBA） | `extended_formats::decode_system_image_in` |
| 4 | 邻图预热 `prefetch_system_decode` 对大图也做整图 PNG 解码，而大图路径根本不消费该缓存 —— 纯浪费 CPU 与磁盘，并与打开操作抢资源 | `extended_formats.rs` |

## 方案

**核心：让外部解码器直接输出未压缩 BMP 栅格，本进程只做随机分块读取。**

实测（14386×10481 AVIF，M 系列 Mac）：

| 中转格式 | 耗时 | 产物 |
|---|---|---|
| PNG（改前） | 6.3s | 179MB，需再整图解码进内存 |
| BMP（改后） | 1.0s | 431MB，`BmpReader` 可随机读，零整图驻留 |

sips 输出的 BMP 为 40 字节 BITMAPINFOHEADER / 24-bit / BI_RGB / top-down，正是现有 `BmpReader` 支持的子集，因此整条 BMP 管线（分块、持久金字塔、瓦片 LRU、邻图预建）自动复用。

## 改动

| 文件 | 改动 |
|---|---|
| `system_decode_backend.rs` | 新增 `raster_bmp_command`：macOS 用 `sips -s format bmp [-Z n] -m sRGB`；非 macOS 返回 `None`（libvips 不写 BMP），调用方回退 |
| `extended_formats.rs` | 新增 `decode_system_image_to_bmp`（栅格直转，失败清理半成品）；`prefetch_system_decode` 跳过会走大图路径的文件（按文件大小 + 像素阈值） |
| `large_image/policy.rs` | `render_via_webview` 增加像素阈值判据（header 探测，不解码）；探测结果复用给后续分支，不重复起子进程 |
| `large_image/session.rs` | 新增 `prepare_system_bmp_raster`：栅格 → `BmpInfo` → `generate_bmp_preview` → `tileable=true`；`raster_max_side` 按 `SYSTEM_RASTER_MAX_BYTES`（2GB）等比收敛；失败回退旧预览路径 |
| `settings.rs` | 并发默认值：`tile_concurrency` 4→8、`thumbnail_concurrency` 4→6、`cpu_threads` 8→12 |
| `LargeImageCanvas.vue` | canvas context 加 `desynchronized: true`，降低缩放/拖动的绘制延迟 |

## 效果（同一张 105MB AVIF）

| 指标 | 改前 | 改后 |
|---|---|---|
| 打开（栅格 + 预览） | ≈10s | 1.5s |
| 本进程整图内存 | ≈350MB RGBA | 0（只有预览 + 有界瓦片 LRU） |
| 首个瓦片 | 不支持分块 | 39ms |
| 放大清晰度 | ≤2048 预览拉伸 | 原始分辨率瓦片 + 粗层渐进 |

## JPEG XL

macOS 14+ 的 ImageIO 只读支持 `public.jpeg-xl`（`sips --formats` 无 Writable 标记），
因此 JXL 与 AVIF 走完全相同的链路，无需额外解码器。

实测 274MB / 14392×10484 / `effort=1`（modular 快速无损）：BMP 栅格 + 预览 **1726ms**、首个瓦片 **50ms**，
sips 解码吃满多核（≈250% CPU）。

与其他系统格式的唯一差别：JXL **不走 `<img>` 直显**（`extended_formats::is_jxl`）。
WebKit 对 JXL 的支持在各版本间反复，与其"先让 `<img>` 失败再 onerror 回退"造成闪烁，
不如统一走系统解码；代价是小 JXL 打开多一次 sips 调用（百毫秒级）。

## 资源边界

- **内存**：整图不再进本进程；瓦片 LRU 上限 = `memoryCacheLimitMB × 40%`；前端 tile 缓存同样按字节淘汰。
- **磁盘**：临时栅格 ≤2GB/会话，最多 2 个活跃会话；会话关闭/逐出时随 `level0.is_temp` 删除，启动时 `remove_dir_all(large-raster)` 兜底。持久金字塔独立配额 `pyramidDiskCacheMB`（默认 1024MB）。

## 已知边界

- Linux（libvips）无 BMP 直转，仍走"整图解码 → 预览"路径，行为不变；若后续验证 `vips` 可写 BMP，只需让 `raster_bmp_command` 返回命令即可接入。
- 超过 2GB 栅格预算的图会等比降采样，100% 缩放下略低于源分辨率。
- 系统格式的 EXIF orientation 依赖解码器烘焙（与改前的 PNG 中转路径一致）。

## 验证

- `cargo test`：147 项通过，含新增 `system_format_prepares_tileable_bmp_raster`（AVIF → 可分块栅格 → 瓦片可读）、`raster_max_side_only_downscales_beyond_disk_budget`、`test_probe_high_pixel_system_format_skips_webview_path`。
- 基准：`PICSEE_BENCH_IMAGE=<path> cargo test --release benchmark_system_open_path -- --ignored --nocapture`。
- `vue-tsc` + `vitest`：28 项通过。
- **未做**：GUI 实机验收（拖动/缩放主观流畅度）、Linux 回归。
