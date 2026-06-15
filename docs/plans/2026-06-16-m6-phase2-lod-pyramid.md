# M6 Phase 2 — 持久金字塔 + 邻居预建 实现计划

> REQUIRED SUB-SKILL: superpowers:executing-plans。设计依据 `docs/plans/2026-06-15-m6-lod-pyramid-design.md`（§2/§4/§5 + §7 Phase 2）。
> 承接 1A（69e4f82）/1B（435a4ff）。角色：codex 研发、Opus 4.8 评审、PM 复核。TDD、frequent commits、story 与 fix 分开。

**Goal:** 让金字塔**跨会话持久复用**并**预建邻居**，使"固定 ~19–20% 缩放逐张切换"时目标层已就绪 → 切换即干净、秒出、不闪；同时给持久塔独立磁盘配额与淘汰，避免无界增长。

**Architecture:** 新增内容寻址的持久金字塔磁盘缓存（key = 文件指纹 + tileSize + 算法/色彩版本），`open_large_image` 命中即复用、未命中后台建入持久目录；新增 `prefetch_large_pyramid` 命令在子线程为邻居预建（去重/限流）；独立配额的目录级水位淘汰（保护活跃会话与在建塔）；前端在切图时对邻居调用预建。

**Tech Stack:** Rust（Tauri 2、自研 BMP reader）、Vue 3、cargo test、vitest、vue-tsc。

**承接约束（不变）：** 盒式降采样/奇数边/预乘 alpha/流式（1A 的 `generate_downscaled_raster`）、`.part`→rename、RAII claim、425 可重试、building 去重。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `src-tauri/src/large_image/pyramid_cache.rs` | 持久金字塔磁盘缓存：指纹 key、目录布局、manifest、有效性校验、目录级水位淘汰 | 新建 |
| `src-tauri/src/large_image/mod.rs` | 导出 `pub mod pyramid_cache;` | 改 |
| `src-tauri/src/large_image/session.rs` | `open_large_image` 复用/建入持久塔；各层 `LevelSource.path` 指向持久文件；`ensure_level` 写持久目录；持久塔**不随 session 关闭删除**（只删非持久的 level0 临时栅格） | 改 |
| `src-tauri/src/lib.rs` | 注册 `prefetch_large_pyramid` 命令；启动时挂载 pyramid 缓存目录/配额 | 改 |
| `src-tauri/src/settings.rs` + `src/types/settings.ts` + `src/stores/settings.ts` | 新增 `largeImage.pyramidDiskCacheMB`（默认 1024）与 `largeImage.neighborPrefetchCount`（默认 1） | 改 |
| `src/components/AppLayout.vue` | 切图时对邻居调用 `prefetch_large_pyramid`（替代/补充现有 prefetch） | 改 |

---

## Task 1：后端——持久金字塔磁盘缓存 `pyramid_cache.rs`

**Files:** Create `pyramid_cache.rs`；Modify `mod.rs`

**接口与布局：**
```rust
const PYRAMID_ALGO_VERSION: u32 = 1; // 盒式算法/格式变更时 +1，使旧缓存失效

/// 缓存指纹：标识"某文件在某参数下的金字塔"。
pub struct PyramidKey { pub hash: String } // hash = sha1/xxhash(canonical_path | size | mtime_ns | tile_size | PYRAMID_ALGO_VERSION)
pub fn pyramid_key(path:&Path, size:u64, mtime_ns:i128, tile_size:u32) -> PyramidKey;

/// 持久塔目录：{cache_root}/large-pyramid/{hash}/  下含 z{L}.bmp（L≥1）与 manifest.json。
pub fn pyramid_dir(cache_root:&Path, key:&PyramidKey) -> PathBuf;

/// 读取 manifest（存在且 algo_version 匹配且各 z 文件存在）→ Some(已就绪层 dims)；否则 None。
pub fn load_manifest(dir:&Path) -> Option<PyramidManifest>;
pub fn write_manifest(dir:&Path, m:&PyramidManifest) -> Result<(),LargeImageError>; // 原子 .part→rename

/// 目录级水位淘汰：总占用超 limit 时，按目录 mtime(最近访问) 从旧到新删除整个 {hash}/，
/// 跳过 protected（活跃会话 + 在建）的 hash。
pub fn evict_to_limit(cache_root:&Path, limit_bytes:u64, protected:&HashSet<String>);

/// touch：访问命中时更新目录 mtime，供 LRU。
pub fn touch(dir:&Path);
```
`PyramidManifest{ algo_version:u32, tile_size:u32, levels: Vec<LevelMeta{z,width,height}> }`（z=0 不入 manifest，原文件即 level0）。

**Tests:**
- [ ] **Step1 失败测试**：`pyramid_key` 对同输入稳定、对 size/mtime/tile_size/algo 任一不同则不同；`write_manifest`+`load_manifest` 往返；`load_manifest` 在 algo_version 不匹配或文件缺失时返回 None；`evict_to_limit` 删除最旧目录直到 ≤limit 且**跳过 protected**。
- [ ] **Step2/3/4** 红→实现→绿（`cargo test large_image::pyramid_cache`）。
- [ ] **Step5 提交** `feat(large-image): persistent content-addressed pyramid cache`。

---

## Task 2：后端——session 接入持久塔 + 生命周期分离

**Files:** Modify `session.rs`、`lib.rs`

**改动：**
1. **启动挂载**：`lib.rs` setup 读取 `pyramidDiskCacheMB`，把缓存根目录 `app_cache_dir/large-pyramid` 与配额存入 managed state（或在命令内现取）。维护"protected hash 集合"= 当前 sessions 的 key + 正在建的 key（可放进 `LargeImageState`）。
2. **open 复用/建入持久塔**：`open_large_image` 算 `pyramid_key`；`load_manifest` 命中 → 各 `LevelSource.path` 直接指向持久 `z{L}.bmp`、`ready=true`（无需重建）、`touch` 目录；未命中 → 走 1B 后台建塔，但**输出写持久目录** `z{L}.bmp`（先 `.part` 后 rename），建完写/更新 manifest。
3. **生命周期分离**：持久塔的 `LevelSource.is_temp=false`（不随 session 关闭删除）；仅非 BMP 的 level0 临时栅格（`large-raster/`）仍随关闭删除。`build_cancelled` 仍取消在途建塔，但**不删已落盘的持久层**。
4. **建塔后触发淘汰**：每次建完一个塔后 `evict_to_limit(root, limit, protected)`（protected 含当前所有活跃/在建 hash）。
5. `add_session`/`remove_session` 维护 protected 集合（加入/移除该 session 的 hash）。

**Tests:**
- [ ] **Step1 失败测试**：构造同一文件两次 open（用真实小 BMP），第二次应**命中持久塔**（不重建：可由 manifest 已存在 + 某计数/标志断言，或检查 z1 文件 mtime 未变）；关闭 session 后持久 z1 文件**仍存在**；非 BMP level0 临时栅格随关闭删除。
- [ ] **Step2/3/4** 红→实现→绿（`cargo test`）。
- [ ] **Step5 提交** `feat(large-image): reuse persistent pyramid across sessions; split temp vs persistent lifecycle`。

---

## Task 3：后端——`prefetch_large_pyramid` 命令（邻居预建）

**Files:** Modify `session.rs`（或新 `prefetch.rs`）、`lib.rs`

**接口：**
```rust
/// 为给定路径在子线程预建金字塔（至少 level1）到持久缓存。
/// 仅对"可 tile 的大图"（BMP 且达大图阈值；非 BMP/小图跳过）生效；
/// 已有有效持久塔则跳过；去重（同 hash 不并发重复建）；限流（全局单 IO 并发，复用建塔 semaphore/调度）。
#[tauri::command]
pub async fn prefetch_large_pyramid(app:AppHandle, paths:Vec<String>) -> Result<(),LargeImageError>;
```
实现：对每个 path 复用 `probe_image_file` 判定 loadMode/tileable；tileable 大图且 `load_manifest` 未命中 → `spawn_blocking` 建持久塔（复用 Task2 的建塔+manifest+淘汰路径，及 1B 的 building 去重/RAII claim，但 building 去重需扩到**跨 session 按 hash 去重**，避免邻居预建与当前会话重复建同一文件）。非阻塞返回。

**Tests:**
- [ ] **Step1 失败测试**：对一个真实小 BMP 调 `prefetch_large_pyramid` 后其持久塔/manifest 出现；对非图/小图路径安全跳过不报错；重复调用不重复建（去重）。
- [ ] **Step2/3/4** 红→实现→绿。注：`#[tauri::command]` 本体可薄封装一个可测的 `prefetch_paths(root, paths, ...)` 纯逻辑函数做单测。
- [ ] **Step5 提交** `feat(large-image): prefetch_large_pyramid command for neighbor prebuild`。

---

## Task 4：前端——切图时预建邻居金字塔

**Files:** Modify `src/components/AppLayout.vue`、`src/types/image.ts`（如需命令类型）、`src/stores/settings.ts`/`src/types/settings.ts`（新设置项）

**改动：**
- 设置项：`largeImage.neighborPrefetchCount`（默认 1）。
- 在现有"切图预加载 watcher"（监听 `currentEntry.path`）里，对前后 `neighborPrefetchCount` 个邻居路径调用 `void invoke('prefetch_large_pyramid', { paths: neighborPaths }).catch(()=>{})`。后端自行判定/跳过非大图。**保持** 1A 修复后的"不在主线程 `<img>` 预热大图"原则——本调用是后端子线程预建，不碰主线程。
- 与既有 `prefetch_system_decode` 并存（系统格式仍走它；大 BMP/tile 图走 `prefetch_large_pyramid`）。

**Tests:**
- [ ] **Step1** `npx vue-tsc -b`、`npm run build`、`npx vitest run` 既有不回归（前端此 Task 主要是布线，纯逻辑少；若抽邻居计算函数可加 vitest）。
- [ ] **Step2 提交** `feat(large-image): prebuild neighbor pyramids on navigation (frontend)`。

---

## Task 5：联调 + 验收 + 文档

- [ ] **Step1 后端全量** `cd src-tauri && cargo test 2>&1 | tail -20`（0 failures）、`cargo fmt --check`、`cargo clippy 2>&1 | tail`。
- [ ] **Step2 前端** `vue-tsc -b`/`vitest`/`npm run build` 绿。
- [ ] **Step3 PM GUI 手测**（`tauri dev`，`白光-Back`）：
  - 在 ~19–20% 逐张切换：目标层应**已预建** → 切换即干净清晰、无摩尔纹、无 ~1s 建塔窗口；
  - 第二次打开同一图：**秒级复用持久塔**，无重建延迟；
  - 持续浏览大量大图：持久缓存占用受 `pyramidDiskCacheMB` 约束、按 LRU 淘汰、活跃图不被淘汰；
  - 切图频繁时无主线程卡顿（预建在子线程）。
- [ ] **Step4 文档**：在设计文档 §7 标注 Phase 2 完成；如有行为/设置变更，更新相关 README/设置说明（如有）。

---

## 自查（spec §7 Phase 2 覆盖）

- 持久磁盘塔（跨 session 复用）→ Task1/2 ✓
- 独立磁盘配额 + 目录级水位淘汰（保护活跃/在建）→ Task1/2 ✓
- `prefetch_large_pyramid` 邻居预建（去重/限流/子线程）→ Task3 ✓
- 前端切图预建邻居 → Task4 ✓
- 验收（切换秒净 / 复用 / 配额）→ Task5 ✓

## 风险与缓解
- **跨 session 按 hash 去重**：building 去重需从"per-session 层"升级为"全局 per-hash"，避免当前会话与邻居预建并发重复建同一文件 → 用全局 `Mutex<HashSet<hash>>`（managed state）+ RAII claim。
- **磁盘竞争**：邻居预建多张 900MB 级读 → 全局单 IO 并发 + 限流，避免拖慢当前图。
- **指纹失效**：mtime/size 变化或 `PYRAMID_ALGO_VERSION` 升级即弃旧缓存，避免脏数据。
