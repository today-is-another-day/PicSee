# M6 Phase 1A — LOD 金字塔 MVP 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:executing-plans 逐任务实现。步骤用 `- [ ]` 勾选跟踪。
> 设计依据：`docs/plans/2026-06-15-m6-lod-pyramid-design.md`（§1/§2/§3 + §7 Phase 1A 范围）。
> 角色：codex 研发，Opus 4.8 评审，PM 复核。TDD、frequent commits、story 与 fix 分开提交。

**Goal:** 为大图引擎引入两级 LOD（level0 原图 + level1 盒式半分辨率），让 ~19–20% 缩放无摩尔纹、~19–50% 拖动更顺滑、tile 失败可重试。

**Architecture:** 后端在 `ImageSession` 内维护按层的源（level0=原文件/临时栅格，level1=后台盒式降采样生成的临时 BMP 栅格），`handle_tile_request` 按 `z` 选层、按 per-level 网格校验、层未就绪返回可重试状态码；前端按 `zoom*dpr` 选层（0/1）、按层渲染、粗层兜底、失败退避重试。**不含**旋转 LOD、level≥2、邻居预建（留 Phase 1B/2）。

**Tech Stack:** Rust（Tauri 2、自研 BMP reader、lru）、Vue 3 `<script setup>` + Canvas 2D、vitest、vue-tsc。

**约束（来自 codex 审核）：**
- 层尺寸用 `ceilDiv`：`levelW=(srcW+1)/2`、`levelH=(srcH+1)/2`；边缘只对**实际存在**的 1/2/4 子像素平均。
- 32-bit 用**预乘 alpha**平均后还原；24-bit 直接平均。
- 建塔**流式低内存**（逐行，不持整图）；输出**先写临时文件、完成 rename**、再置 ready。
- session 关闭/逐出**取消在途建塔**并删除其临时栅格。
- 前端 `failedTiles` 改**可重试**（退避，非永久负缓存）；tile cache/loading/failed/inflight 键全部含 `z`。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `src-tauri/src/large_image/pyramid.rs` | 盒式降采样栅格生成（流式、奇数边、24/32bit 预乘 alpha） | 新建 |
| `src-tauri/src/large_image/mod.rs` | 导出 `pub mod pyramid;` | 改 |
| `src-tauri/src/large_image/session.rs` | `ImageSession` 增加按层源（`LevelSource`），后台建 level1 + 取消 + ready；`handle_tile_request` 按 z 选层/校验/可重试；`open_large_image` 返回 `maxLevel` | 改 |
| `src-tauri/src/lib.rs` | picsee tile handler：层未就绪状态码透传（425） | 改 |
| `src/types/image.ts` | `OpenLargeImageResult`/`LargeImageSession` 增 `maxLevel` | 改 |
| `src/utils/largeImageUrl.ts` | `tileUrl(sessionId, z, x, y)` 启用 z | 改 |
| `src/components/LargeImageCanvas.vue` | 选层 + 按层渲染 + 批量 transform + 粗层兜底 + 可重试 tile + key 含 z；删低倍率保护 | 改 |

---

## Task 1：后端——盒式降采样栅格生成 `pyramid.rs`

**Files:**
- Create: `src-tauri/src/large_image/pyramid.rs`
- Modify: `src-tauri/src/large_image/mod.rs`（加 `pub mod pyramid;`）

**接口：**
```rust
/// 把 src（任意 24/32-bit BI_RGB BMP，via BmpReader）2×2 盒式平均降采样为
/// (ceil(w/2) × ceil(h/2)) 的 top-down BI_RGB BMP，写到 dst。
/// 24-bit 直接平均；32-bit 预乘 alpha 平均后还原。逐行流式，峰值内存 = 几行缓冲。
/// 奇数右/下边只对实际存在的 1/2/4 子像素平均。
pub fn generate_downscaled_raster(src: &Path, dst: &Path) -> Result<(u32, u32), LargeImageError>;
```

**实现要点（精确算法）：**
1. `BmpReader::open(src)` 得 `info{width:sw, height:sh, pixel_format}`。`dw=(sw+1)/2`、`dh=(sh+1)/2`。`has_alpha = matches!(pixel_format, Bgra32)`。
2. 输出栅格通道数：has_alpha→4 else 3。`dst_row=(dw*ch+3)&!3`。写 54 字节 top-down BMP 头（同 `write_temp_bmp_raster` 的头：宽 dw、高 `-(dh)`、bitcount ch*8、compression 0）。
3. 逐输出行 `oy in 0..dh`：源行 `sy0=2*oy`、`rows = min(2, sh-sy0)`（1 或 2）。`let src_rows = reader.read_region(Rect{0, sy0, sw, rows}, sw, rows)?;`（1:1，返回 RGBA `rows*sw*4`）。
4. 对 `ox in 0..dw`：源列 `sx0=2*ox`、`cols = min(2, sw-sx0)`。聚合 `cols*rows`（1/2/4）个 RGBA 子像素：
   - 24-bit：对 R/G/B 求整数均值（和 / n，四舍五入用 `(sum + n/2)/n`），A 固定 255。
   - 32-bit：先对每子像素 `pr=r*a/255`…（预乘），对 pr/pg/pb/a 求均值 `(am)`；还原 `r=if am>0 {clamp(pm*255/am)} else 0`，输出 BGRA。
   - 写入目标行缓冲（BGR 或 BGRA 顺序，含尾部 4 字节对齐填充置 0）。
5. `writer.write_all(&dst_row_buf)`。结束 flush，返回 `(dw, dh)`。

**Tests（`#[cfg(test)] mod tests`，用现有 `make_bmp_raw` 风格动态造 BMP）：**

- [ ] **Step 1：写失败测试（盒式均值 + 奇数边 + 24/32bit）**
```rust
// 4×4 24bit：构造已知像素，验证 2×2 块均值正确
// 像素值用 (x,y) 可预测：R=x*10, G=y*10, B=0
// 输出 (0,0) 应 = 源 (0,0),(1,0),(0,1),(1,1) 的 R/G 均值
#[test] fn test_box_2x2_24bit_mean() { /* 造 4×4 BMP → generate_downscaled_raster → BmpReader 读回 2×2 → 断言 R=(0+10+0+10)/4=5, G=(0+0+10+10)/4=5 */ }

// 5×3（奇数边）→ ceil → 3×2，右/下边只 1~2 子像素平均，验证不越界、边缘均值正确
#[test] fn test_box_odd_edges_3x2() { /* 断言输出尺寸 (3,2)，右下角块只平均存在子像素 */ }

// 32bit 预乘 alpha：a=0 的子像素不污染颜色
#[test] fn test_box_32bit_premultiplied_alpha() { /* 半透明块均值，验证预乘还原 */ }
```
- [ ] **Step 2：运行验证失败**：`cd src-tauri && cargo test large_image::pyramid 2>&1`，预期编译失败/未实现。
- [ ] **Step 3：实现 `generate_downscaled_raster`** 按上算法。
- [ ] **Step 4：运行验证通过**：`cargo test large_image::pyramid 2>&1`，预期 0 failures。
- [ ] **Step 5：提交**：`git add src-tauri/src/large_image/pyramid.rs src-tauri/src/large_image/mod.rs && git commit -m "feat(large-image): box-downscale raster generator for LOD"`（story）。

---

## Task 2：后端——`ImageSession` 按层源 + 后台建 level1

**Files:** Modify `src-tauri/src/large_image/session.rs`

**数据结构（新增）：**
```rust
/// 单层 tile 源。
pub struct LevelSource {
    pub width: u32,          // 该层像素宽
    pub height: u32,         // 该层像素高
    pub path: std::path::PathBuf, // 该层栅格/原文件路径
    pub is_temp: bool,       // 是否本引擎生成的临时栅格（关闭/逐出删）
    pub ready: std::sync::atomic::AtomicBool, // 建好可用
}
```
- `ImageSession` 增字段：
  - `pub max_level: u32`（= 让整层 ≤ tileSize 的 L；用 `width/height/tile_size` 算）
  - `pub levels: Mutex<Vec<Arc<LevelSource>>>`（index = z；初始放 level0：path=`tile_source_path`、ready=true、is_temp=`tile_source_is_temp`）
  - `pub build_cancelled: Arc<AtomicBool>`（关闭/逐出置 true）
- `OpenLargeImageResult` 增 `max_level: u32`（camelCase `maxLevel`）。
- `compute_max_level(w,h,tile)`：`let mut l=0; while ceil(w>>l)>tile || ceil(h>>l)>tile { l+=1 } l`（Phase 1A 仍可算真实 maxLevel，但只**建** level1）。

**后台建塔（粗层优先：先 level1）：**
- `open_large_image` 末尾，对 `tileable && max_level>=1` 的会话：`spawn_blocking` 调 `ensure_level(session, 1)`。
- `fn ensure_level(session:&ImageSession, z:u32)->Result<(),LargeImageError>`：
  - 若 `build_cancelled` 已置 → 直接返回 Ok（放弃）。
  - 取 `levels`：若 `levels[z]` 已 ready → 返回。否则确保 `levels[z-1]` 已 ready（递归/链式；Phase 1A 只到 z=1，src=level0）。
  - 目标临时路径：`app_cache_dir/large-raster/pyr-{session_id}-z{z}.bmp`（**先写 `.part` 再 rename**）。
  - `generate_downscaled_raster(level[z-1].path, tmp_part)` → rename → 构造 `LevelSource{ready=true,is_temp=true,...}` 写入 `levels[z]`。
  - 完成后再次检查 `build_cancelled`：若已取消则删除刚生成文件。
- session 关闭/逐出（`remove_session`/`add_session` 逐出分支）：置 `build_cancelled=true`，并删除 `levels` 中所有 `is_temp` 路径（含 level0 临时栅格与各层栅格）。

**Tests:**
- [ ] **Step 1：失败测试**——`compute_max_level(19200,16384,512)==6`；`compute_max_level(1000,1000,512)==1`；level0 初始 ready。
- [ ] **Step 2：cargo test 失败**。
- [ ] **Step 3：实现结构 + ensure_level（不含命令布线）**。
- [ ] **Step 4：cargo test large_image::session 通过**。
- [ ] **Step 5：提交** `feat(large-image): per-level sources and background level1 build`。

---

## Task 3：后端——`handle_tile_request` 按 z 选层 + 可重试

**Files:** Modify `src-tauri/src/large_image/session.rs`、`src-tauri/src/lib.rs`

**改 `handle_tile_request(state, session_id, z, tx, ty)`：**
1. tile_key `(session_id, z, tx, ty)`（已含 z）；先查 LRU。
2. 取 session；`levels` 中取 `level = levels.get(z as usize)`：
   - 不存在该层（z>max_level）→ `Err((400, tile_out_of_range))`。
   - 存在但 `!ready` → **`Err((425, LargeImageError::new("LEVEL_NOT_READY", ...)))`**（可重试）。
3. per-level 网格校验：`tiles_x=ceil(level.width/tile_size)`、`tiles_y=ceil(level.height/tile_size)`；越界 → `Err((400, tile_out_of_range))`。
4. `generate_bmp_tile(&level.path, tx, ty, tile_size, level.width, level.height)` → WebP；写 LRU；返回。
   - 注意：`generate_bmp_tile` 已按传入的 img_w/img_h 裁剪边缘 tile，传 **level 尺寸**即可正确。

**改 `lib.rs` picsee tile 路由：** 解析到的 z 透传给 `handle_tile_request`（当前传 `0`，改为解析值）；`Err((425,_))` 映射 HTTP 425（前端据此/据 onerror 退避重试）。

**Tests:**
- [ ] **Step 1：失败测试**——构造 session（level0 ready、level1 未 ready），`handle_tile_request(_,_,1,0,0)` 返回 425；level0 tile 正常；z>max_level 返回 400；per-level 越界返回 400。
- [ ] **Step 2/3/4：cargo test 红→实现→绿**。
- [ ] **Step 5：提交** `feat(large-image): serve per-level tiles via z with retryable not-ready`。

---

## Task 4：后端——`open_large_image` 返回 maxLevel + 前端类型

**Files:** `src-tauri/src/large_image/session.rs`、`src/types/image.ts`

- `OpenLargeImageResult` 增 `max_level`（赋 `compute_max_level(width,height,tile_size)`）。
- 前端 `OpenLargeImageResult`/`LargeImageSession` 增 `maxLevel: number`。
- [ ] **Step 1-2：** 后端 serde 字段 + `cargo build`；前端类型 + `npx vue-tsc -b`。
- [ ] **Step 3：提交** `feat(large-image): expose maxLevel in open result`。

---

## Task 5：前端——`tileUrl` 启用 z

**Files:** `src/utils/largeImageUrl.ts`

```ts
/** {base}/tile/{sessionId}/{z}/{x}/{y} */
export function tileUrl(sessionId: number, z: number, x: number, y: number): string {
  return `${picseeBase()}/tile/${sessionId}/${z}/${x}/${y}`
}
```
- [ ] **Step 1：** 改签名（z 取代固定 0）。更新 `shortcuts.test.ts`? 无关。若有调用方编译错，下一 Task 一并改。
- [ ] **Step 2：** `npx vue-tsc -b`（LargeImageCanvas 调用处会报错，预期，下一 Task 修）。
- [ ] **Step 3：提交**（与 Task 6 合并提交亦可，保持前端一个 story）。

---

## Task 6：前端——LargeImageCanvas 选层渲染 + 批量 transform + 粗层兜底

**Files:** `src/components/LargeImageCanvas.vue`

**选层：**
```ts
function selectLevel(): number {
  const dpr = window.devicePixelRatio || 1
  const effScale = zoom.value * dpr
  const ideal = Math.floor(Math.log2(1 / Math.max(effScale, 1e-6)))
  // Phase 1A：只建到 level1，渲染层 clamp 到 [0, min(1, maxLevel)]
  const maxRenderable = Math.min(1, props.session.maxLevel)
  return Math.min(Math.max(ideal, 0), maxRenderable)
}
```
（Phase 1B 再引入迟滞与 ready-level 上限；1A 用固定 clamp=1。）

**render() 重构（按层）：**
- tileCache/loadingTiles/failedTiles/inflight 的 key 改 `${sessionId}:${z}:${tx}:${ty}`。
- 计算可见范围（沿用 `computeVisibleRect`，rotation=0 时有效；rotation≠0 仍走旧 preview 分支，1A 不动旋转）。
- 选层 `L=selectLevel()`；该层 `levelW=ceil(width/2^L)`、`levelH=ceil(height/2^L)`、`levelTiles` 网格；**层内 tile 在原图坐标按 `2^L` 放置**：tile (tx,ty) 覆盖原图 `[tx*512*2^L, ...]`，宽 `min(512, levelW-tx*512)*2^L`。
- **粗层兜底**：先画 preview（最底），再画**已缓存的更粗层 L+1**（若 L<maxRenderable 且其 tile 已缓存）覆盖其区域，最后画目标层 L。每层渲染**只 `save+setTransform` 一次**，循环内仅 `drawImage`。
- 删除 `needTiles` 里的 `visibleTileCount <= floor(TILE_CACHE_LIMIT*0.7)` 与 `zoom*dpr>previewScale` 判定（改为：tileable && rotation===0 即按层渲染；是否需要 tile 由"选层是否=0 且 1:1"决定——L 总是有意义）。

**loadTile(z, tx, ty)：** URL `tileUrl(sessionId, z, tx, ty)`；onload 写 cache（key 含 z）触发重绘。

- [ ] **Step 1：** 改 tileCache 等键含 z；新增 `selectLevel`、按层网格/放置；批量 transform。
- [ ] **Step 2：** `npx vue-tsc -b` 通过；`npm run build` 通过。
- [ ] **Step 3：提交** `feat(large-image): per-level tile rendering with coarse fallback (frontend)`。

---

## Task 7：前端——可重试 tile（替换永久负缓存）

**Files:** `src/components/LargeImageCanvas.vue`

- 移除"一次 onerror 即永久 `failedTiles`"。改为 `tileRetry: Map<key, {attempts, nextAt}>`：
  - onerror：`attempts<MAX_RETRY(默认 6)` → 设 `nextAt = now + backoff(attempts)`（如 `min(2000, 150*2^attempts)` ms），`scheduleRender()` 后由定时/下一帧到点重发；`attempts>=MAX` → 记入 `permanentFailed`（真越界，停止）。
  - 成功加载即清除该 key 的 retry 记录。
  - 切换 session/层时清空 retry 与 inflight（沿用 `cancelInflightImages`）。
- 由于 `<img>` onerror 拿不到 HTTP 状态码，统一按"可重试"处理，靠 `MAX_RETRY` 上限兜底真越界。
- [ ] **Step 1：** 实现退避重试，删除永久 `failedTiles` 语义。
- [ ] **Step 2：** `npx vue-tsc -b`、`npm run build` 通过。
- [ ] **Step 3：提交** `fix(large-image): retry not-ready/failed tiles with backoff`（fix，单独提交）。

---

## Task 8：联调验证 + 全量检查

- [ ] **Step 1：后端全测** `cd src-tauri && cargo test 2>&1 | tail -30`，预期 0 failures。
- [ ] **Step 2：** `cargo fmt --check`（必要时 `cargo fmt`）；`cargo clippy 2>&1 | tail` 无新增 error。
- [ ] **Step 3：前端** `npx vue-tsc -b` 与 `npm run build` 通过；`npx vitest run` 既有用例通过。
- [ ] **Step 4：手测**（`tauri dev`，基准目录 `白光-Back`）：
  - 19–20% 缩放**无摩尔纹**（level1 建好后；建塔窗口期短暂仍为旧 preview，约 1s 自愈）；
  - 拖动**更顺滑**、闪影明显减少；
  - 深度放大走 level0、像素精确；
  - 切图正常，无报错刷屏（425 退避不应刷错误）。
- [ ] **Step 5：** 不单独提交（各 Task 已提交）；汇总 push。

---

## 自查（spec 覆盖）

- §1 z 语义/ceilDiv/per-level 网格 → Task 1/2/3/6 ✓；选层公式 → Task 6 ✓（迟滞留 1B）。
- §2 level0 不预生成、level1 盒式 mip、流式、奇数边/预乘 alpha、先 part 后 rename、取消、is_temp 清理 → Task 1/2 ✓；磁盘独立配额/淘汰留 Phase 2。
- §3 删低倍率保护、按层渲染、批量 transform、粗层兜底、可重试、key 含 z → Task 6/7 ✓；旋转留 1B；前端按字节淘汰留 1B。
- §4 tile 启用 z、425 可重试、open 返 maxLevel → Task 3/4/5 ✓；就绪事件→用重试替代（MVP 简化）。

**Phase 1A 不含（留 1B/2）：** level≥2 与迟滞、接缝 gutter 精修、旋转 LOD、前端按字节淘汰、持久磁盘塔/独立配额/邻居预建。
