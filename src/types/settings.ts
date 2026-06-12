export type AppLanguage = 'system' | 'zh-CN' | 'en-US'
export type AppTheme = 'system' | 'light' | 'dark'
export type ZoomMode = 'fit-window' | 'fit-width' | 'actual-size' | 'remember'
export type ThumbnailPosition = 'left' | 'bottom'
export type ThumbnailSize = 96 | 160 | 256

/** 与需求 v0.3 严格对应的应用设置结构。 */
export interface AppSettings {
  language: AppLanguage
  theme: AppTheme
  viewer: {
    defaultZoomMode: ZoomMode
    zoomStep: number
    smoothZoom: boolean
    zoomToCursor: boolean
    resetZoomOnSwitch: boolean
  }
  largeImage: {
    fileSizeThresholdMB: number
    pixelThreshold: number
    sideThreshold: number
    previewMaxSize: 2048 | 4096 | 8192
    tileSize: 256 | 512 | 1024
    enableTilePrefetch: boolean
    prefetchRadius: number
  }
  cache: {
    memoryCacheLimitMB: number
    diskCacheLimitMB: number
    enableDiskCache: boolean
    clearTempTileOnExit: boolean
  }
  performance: {
    tileConcurrency: number
    decodeConcurrency: number
    thumbnailConcurrency: number
    preloadNormalCount: number
    preloadLargePreviewCount: number
  }
  layout: {
    showThumbnailBar: boolean
    thumbnailPosition: ThumbnailPosition
    /** 缩略图尺寸（最长边像素），影响缩略图栏 item 显示大小。 */
    thumbnailSize: ThumbnailSize
    showStatusBar: boolean
    compactMode: boolean
  }
}
