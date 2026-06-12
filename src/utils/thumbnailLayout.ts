/**
 * 虚拟滚动尺寸常量（JS 与 CSS 共享同一套数值，修改时两处同步）。
 *
 * 纵向 item（flex-row）：
 *   高度 = thumbSize + ITEM_PAD（padding 6×2=12）+ ITEM_BORDER（1×2=2）+ ITEM_MARGIN（margin-bottom 4）
 *        = thumbSize + 18
 *
 * 横向 item（flex-col）：
 *   宽度 = thumbSize + ITEM_PAD（padding 6×2=12）
 *   步进 = 宽度 + ITEM_BORDER（2）+ ITEM_MARGIN（4）
 *        = thumbSize + 18
 *
 * 两向步进相同，均为 thumbSize + ITEM_STEP_EXTRA（= 18）。
 *
 * CSS 对应注释见 ThumbnailSidebar.vue .thumbnail-sidebar__item。
 */
export const ITEM_PAD = 12       // 水平/垂直 padding 合计（6px × 2）
export const ITEM_BORDER = 2     // border 合计（1px × 2）
export const ITEM_MARGIN = 4     // 纵向 margin-bottom / 横向 margin-right
export const ITEM_STEP_EXTRA = ITEM_PAD + ITEM_BORDER + ITEM_MARGIN  // = 18

/**
 * 计算 item 步进（滚动轴方向上的间距，包含 item 自身 border/margin）。
 * 纵向和横向步进相同。
 */
export function calcItemStep(thumbSizePx: number): number {
  return thumbSizePx + ITEM_STEP_EXTRA
}

/**
 * 根据滚动位置计算起始索引（纯函数，便于单元测试）。
 */
export function calcStartIndex(
  scrollPx: number,
  step: number,
  buffer: number,
  total: number,
): number {
  const raw = Math.floor(scrollPx / step)
  return Math.max(0, Math.min(raw - buffer, total - 1))
}
