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
