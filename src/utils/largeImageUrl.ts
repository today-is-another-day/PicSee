/**
 * 平台相关的大图协议 URL 辅助函数。
 *
 * macOS WKWebView：自定义协议为 picsee://localhost/...
 * Windows/Linux（Tauri 2.x）：自定义协议为 http://picsee.localhost/...
 *
 * 第一阶段（M4）只需 macOS 正确，Windows/Linux 分支已预留，待 M6 验证。
 */

/** 判断是否 macOS（基于 userAgent）。 */
function isMacOS(): boolean {
  return /Mac OS X/i.test(navigator.userAgent)
}

/**
 * 生成 picsee:// 协议的 base URL。
 * macOS: picsee://localhost
 * Windows/Linux: http://picsee.localhost
 */
function picseeBase(): string {
  return isMacOS() ? 'picsee://localhost' : 'http://picsee.localhost'
}

/**
 * 拼接 preview URL。
 * 格式：{base}/preview/{sessionId}
 */
export function previewUrl(sessionId: number): string {
  return `${picseeBase()}/preview/${sessionId}`
}

/**
 * 拼接 tile URL。
 * 格式：{base}/tile/{sessionId}/{z}/{x}/{y}
 */
export function tileUrl(sessionId: number, z: number, x: number, y: number): string {
  return `${picseeBase()}/tile/${sessionId}/${z}/${x}/${y}`
}
