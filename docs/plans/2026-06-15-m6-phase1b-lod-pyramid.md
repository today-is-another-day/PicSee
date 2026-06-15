# M6 Phase 1B — 当前图完整 LOD 实现计划

> REQUIRED SUB-SKILL: superpowers:executing-plans。设计依据 `docs/plans/2026-06-15-m6-lod-pyramid-design.md`（§1/§2/§3 + §7 Phase 1B）。
> 承接 Phase 1A（commit 1b69df3 / 69e4f82）。角色：codex 研发、Opus 4.8 评审、PM 复核。TDD、frequent commits、story 与 fix 分开。

**Goal:** 把 LOD 从两级（0/1）扩展到完整金字塔，并补齐多级选层迟滞、旋转 LOD、tile 接缝处理、前端按字节淘汰，使**任意缩放（含 fit-window 与旋转态）都干净、顺滑、无半屏空洞**。

**Architecture:** 后端后台按 mip 链建 `level 2..maxLevel`，并在 tile 请求命中未就绪层时按需触发该层构建（去重）；前端选层放开到 `[0, maxLevel]` 并用完整迟滞，渲染支持旋转（可见矩形按 rotation 反算），tile 缓存改按估算字节淘汰。

**Tech Stack:** Rust（Tauri 2、自研 BMP reader、lru）、Vue 3 Canvas 2D、cargo test、vitest、vue-tsc。

**承接 1A 的约束（不变）：** ceilDiv 尺寸、奇数边只平均实际子像素、32bit 预乘 alpha、流式建塔、`.part`→rename、`build_cancelled` 取消、is_temp 清理、425 可重试、tile key 含 z。

---

## 文件结构（均为修改）

| 文件 | 1B 职责 |
|---|---|
| `src-tauri/src/large_image/session.rs` | 后台建 `2..maxLevel`；tile 请求未就绪层时按需触发构建（去重/并发安全） |
| `src/components/LargeImageCanvas.vue` | 选层放开 `[0,maxLevel]` + 完整迟滞；旋转感知 `computeVisibleRect`/选层渲染；tile 缓存按字节淘汰；接缝 gutter |

---

## Task 1：后端——构建完整金字塔（2..maxLevel）+ 按需触发

**Files:** Modify `src-tauri/src/large_image/session.rs`

**改动：**
1. **后台建塔扩到全层**：`open_large_image` 末尾后台任务由"建 level1"改为"按 z=1..max_level 顺序 `ensure_level`"（粗层优先 = 先建当前选中层及其依赖链；最简实现：1→2→…→max_level 顺序，每级在前一级之上派生，level1 之后都很快）。每级之间检查 `build_cancelled` 可中止。
2. **按需触发（去重）**：`handle_tile_request` 遇到目标层 `!ready` 时，除返回 425 外，**spawn 一个后台 `ensure_level(z)`**（若该层未在建）。用 per-session 的"building 层集合"`Mutex<HashSet<u32>>`（或每层 `building:AtomicBool`）去重，确保同一层不并发重复建；`ensure_level` 内部已有 levels 锁，构建前置 building、完成/失败清除。
3. **`ensure_level(z)` 链式**：确保 `z-1` ready（递归 ensure），再 `generate_downscaled_raster(level[z-1].path, .part)`→rename→置 ready。已在 1A 实现链式，确认对任意 z 正确（不止 z=1）。

**Tests:**
- [ ] **Step1 失败测试**：构造小图 session，`ensure_level(3)` 后 `levels[1..=3]` 全 ready 且尺寸为各级 ceilDiv（如 100×100 → L1=50,L2=25,L3=13）；`handle_tile_request` 对未就绪深层返回 425 且触发后台构建后最终可取（可用轮询/直接调 ensure 验证 ready 翻转）。并发两次 `ensure_level(2)` 不产生重复文件/不 panic（building 去重）。
- [ ] **Step2** `cargo test large_image::session` 红。
- [ ] **Step3** 实现。
- [ ] **Step4** `cargo test` 绿（全量 0 failures）。
- [ ] **Step5 提交** `feat(large-image): build full LOD pyramid with on-demand level build`。

---

## Task 2：前端——完整多级选层 + 迟滞

**Files:** Modify `src/components/LargeImageCanvas.vue`

**改 `selectLevel()`：**
- 放开上限：`maxRenderable = props.session.maxLevel`（不再 clamp 到 1）。
- 完整迟滞（替换 1A 的 0.45/0.55 死区）：维护 `selectedLevel`；按"物理采样率"切层——设当前层 L 的采样率 `rate = effScale * 2^L`（=该层 1 像素占多少 device px）。`rate < 0.45` → 切更粗层（L+1）；`rate > 1.1` → 切更细层（L-1）；区间内维持 L。多级时逐级迁移并 clamp `[0, maxRenderable]`。初次（selectedLevel===null）用 `clamp(floor(log2(1/effScale)),0,maxRenderable)`。
- 渲染层用 `min(selectedLevel, 已就绪最高层?)`——Phase 1B 不引入就绪查询，仍靠"未就绪→425→粗层兜底+重试"，故选**理想层**即可，未就绪时兜底层覆盖。
- S1 预算守卫保留但因深层 tile 数恒定，基本不再触发（仍作安全网）。

**Tests（vitest，对纯函数抽取选层逻辑）：**
- [ ] **Step1 失败测试**：把选层抽成可测纯函数 `pickLevel(effScale, prevLevel, maxLevel)`，断言：effScale=1→0；0.4→1；0.1→3（19200 级别 maxLevel=6）；在 rate 临界带内维持 prevLevel（迟滞）。
- [ ] **Step2** `npx vitest run` 红。
- [ ] **Step3** 实现 + 接线。
- [ ] **Step4** `vue-tsc -b`、`vitest`、`npm run build` 绿。
- [ ] **Step5 提交** `feat(large-image): full multi-level selection with hysteresis`。

---

## Task 3：前端——旋转 LOD（rotation 90/180/270）

**Files:** Modify `src/components/LargeImageCanvas.vue`

**改动：**
- 删除 tile 渲染对 `rotation.value === 0` 的限制；旋转态也走按层 tile。
- `computeVisibleRect()` 改**旋转感知**：当前实现按未旋转算可见原图矩形；需对 rotation=90/180/270 用逆变换求视口四角映射到原图坐标系的包围盒（min/max）。给出明确公式：已知 `applySourceTransform` 的正变换，反解视口 `[0,vpW]×[0,vpH]` 四角 → 原图坐标 → `imgX/imgY/imgX1/imgY1` 取 min/max 并 clamp 到 `[0,imgW]×[0,imgH]`。
- tile 在原图坐标按 `2^L` 放置不变（`applySourceTransform` 已含旋转分支，旋转随之正确）。
- preview 兜底分支同样在旋转态有效（preview 也经 applySourceTransform）。

**Tests:**
- [ ] **Step1** 抽 `visibleImageRect(viewport, zoom, offset, rotation, imgW, imgH)` 纯函数，vitest 断言 rotation=0/90/180/270 下四角包围盒正确（用已知 viewport/zoom/offset 手算期望）。
- [ ] **Step2/3/4** 红→实现→绿（vitest + vue-tsc + build）。
- [ ] **Step5 提交** `feat(large-image): LOD tile rendering under rotation`。

---

## Task 4：前端——tile 缓存按估算字节淘汰

**Files:** Modify `src/components/LargeImageCanvas.vue`

**改动：**
- `tileCache` 由"≤256 个"改为"≤预算字节"：每个 `HTMLImageElement` 估算解码字节 `naturalWidth*naturalHeight*4`（未知时按 `tileSize*tileSize*4` 估）。维护 `tileCacheBytes`，`lruAccess` 写入时累加、淘汰最旧直到 `<= 预算`。
- 预算：取 `settingsStore.settings.cache.memoryCacheLimitMB` 的一个比例（如 40%，与后端 tile LRU 对齐口径），但**前端独立计**；给个常量比例与下限（如 ≥64MB）。
- onload 时 `naturalWidth/Height` 已知，按真实字节累计。

**Tests:**
- [ ] **Step1** 抽 `evictByBytes(map, bytes, limit)` 纯函数（或对 cache 包一层）vitest：插入超预算后字节 `<= limit`、最旧被淘汰。
- [ ] **Step2/3/4** 红→实现→绿。
- [ ] **Step5 提交** `feat(large-image): evict tile cache by estimated bytes`。

---

## Task 5：接缝 gutter + 联调 + 验收

**Files:** Modify `src/components/LargeImageCanvas.vue`

- **接缝**：相邻 tile 在分数 device 坐标下 `drawImage` 可能出现 1px 缝/重叠。采用：绘制时把目标矩形按设备像素取整对齐，或源/目标各留 0.5px overlap（apron）。给出最小实现：目标矩形用 `Math.round` 对齐到 device px（在 setTransform 已含 dpr 的前提下，按整数原图像素边界绘制即可避免缝）。验证不同 dpr 下无缝/无重影。
- **联调**：`tauri dev`（PM 环境）手测——fit-window 干净（深层接管,不再 preview-only 摩尔纹）、各倍率平滑缩放无层抖动、旋转后清晰可放大、长时间拖动内存稳定（按字节淘汰）。
- **验收**：记录 level1 建塔实测耗时（后端 `#[cfg(debug_assertions)]` println 或新埋点）；19–20% 与各倍率拖动帧率（沿用临时埋点或手感）。

- [ ] **Step1** 实现接缝对齐。
- [ ] **Step2** `vue-tsc`/`build`/`vitest` 绿；`cargo test` 绿（若动后端）。
- [ ] **Step3 提交** `fix(large-image): align tile draw to device pixels to remove seams`（fix，单独提交）。
- [ ] **Step4** PM GUI 手测 + 记录验收数据（不入代码）。

---

## 自查（spec §7 1B 覆盖）

- 完整金字塔 2..maxLevel + 按需触发 → Task1 ✓
- 多级选层 + 迟滞（0.45/1.1）→ Task2 ✓
- 旋转 LOD → Task3 ✓
- 前端按字节淘汰 → Task4 ✓
- 接缝 gutter + 性能验收 → Task5 ✓

**1B 不含（留 Phase 2）：** 持久磁盘塔（跨 session 复用）、独立磁盘配额与淘汰器、邻居预建 `prefetch_large_pyramid`。
