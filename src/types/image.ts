/** 后端返回的图片文件条目。 */
export interface ImageEntry {
  path: string
  name: string
  size: number
  modified: number
}

/** open_directory 命令返回值。 */
export interface OpenDirectoryResult {
  directory: string
  entries: ImageEntry[]
}

export type OpenImageFileResult = ImageEntry | null
export type OpenDirectoryCommandResult = OpenDirectoryResult | null
export type ScanDirectoryResult = ImageEntry[]

/** 后端 thumbnails.rs 返回的结构化错误，便于前端按 code 映射 i18n。 */
export interface ThumbnailBackendError {
  code:
    | 'UNSUPPORTED_FORMAT'
    | 'NOT_ALLOWED'
    | 'IO_ERROR'
    | 'FILE_TOO_LARGE'
    | 'DECODE_ERROR'
    | 'IMAGE_TOO_LARGE'
    | string
  message: string
}

// ─── 大图引擎类型（M4）───────────────────────────────────────────

/** probe_image 返回的加载模式。 */
export type LoadMode = 'normal' | 'largeCandidate' | 'tileRequired'

/** probe_image command 返回值。 */
export interface ImageProbe {
  width: number
  height: number
  format: string
  fileSize: number
  isLarge: boolean
  loadMode: LoadMode
}

/** open_large_image command 返回值。 */
export interface OpenLargeImageResult {
  sessionId: number
  generation: number
  width: number
  height: number
  tileSize: number
  previewMaxSize: number
}

/** 前端维护的大图会话状态。 */
export interface LargeImageSession extends OpenLargeImageResult {
  path: string
}
