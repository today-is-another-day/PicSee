#!/usr/bin/env node
/**
 * gen_test_bmp.mjs — 生成参数化渐变 BMP 测试文件
 *
 * 用法：
 *   node scripts/gen_test_bmp.mjs [width] [height] [output]
 *   node scripts/gen_test_bmp.mjs 14000 10000 test-assets/test-400mb.bmp
 *
 * 默认：14000×10000，输出到 test-assets/test-large.bmp
 * 生成 24bit 未压缩 BMP（BI_RGB），渐变图案（R=x%256, G=y%256, B=0）
 *
 * 注意：生成的 BMP 文件已加入 .gitignore，不提交到版本库。
 */

import { createWriteStream, mkdirSync } from 'node:fs'
import { dirname } from 'node:path'

const width = parseInt(process.argv[2] ?? '14000', 10)
const height = parseInt(process.argv[3] ?? '10000', 10)
const output = process.argv[4] ?? 'test-assets/test-large.bmp'

const bpp = 3 // 24bit BGR
const rowStride = Math.ceil((width * bpp) / 4) * 4 // 4 字节对齐
const pixelDataSize = rowStride * height
const fileSize = 54 + pixelDataSize

console.log(`生成 BMP：${width}×${height}，大小约 ${(fileSize / (1024 ** 3)).toFixed(2)}GB`)
console.log(`输出：${output}`)

mkdirSync(dirname(output), { recursive: true })
const stream = createWriteStream(output)

// ── BMP 文件头（54 字节）──────────────────────────────────────────
const header = Buffer.alloc(54)
header.write('BM', 0, 'ascii')
header.writeUInt32LE(fileSize, 2)
header.writeUInt32LE(0, 6)         // reserved
header.writeUInt32LE(54, 10)       // pixelDataOffset
header.writeUInt32LE(40, 14)       // DIB header size
header.writeInt32LE(width, 18)     // width
header.writeInt32LE(height, 22)    // height（正数=bottom-up）
header.writeUInt16LE(1, 26)        // color planes
header.writeUInt16LE(24, 28)       // bits per pixel
header.writeUInt32LE(0, 30)        // compression (BI_RGB)
header.writeUInt32LE(pixelDataSize, 34)
header.writeInt32LE(2835, 38)      // XPelsPerMeter (~72 DPI)
header.writeInt32LE(2835, 42)      // YPelsPerMeter

stream.write(header)

// ── 像素数据（bottom-up：文件首行是图像最后一行）──────────────────
// 每次写一行，避免整图占用内存
const rowBuf = Buffer.alloc(rowStride, 0)

let rowsWritten = 0

// BMP bottom-up：imgY=height-1 对应文件第 0 行，imgY=0 对应文件最后一行
function writeAllRows() {
  for (let imgY = height - 1; imgY >= 0; imgY--) {
    for (let x = 0; x < width; x++) {
      const offset = x * 3
      rowBuf[offset] = 0              // B
      rowBuf[offset + 1] = imgY % 256 // G
      rowBuf[offset + 2] = x % 256    // R
    }
    stream.write(rowBuf.slice(0, rowStride))
    rowsWritten++
    if (rowsWritten % 1000 === 0) {
      process.stdout.write(`\r  写入行 ${rowsWritten}/${height} (${(rowsWritten / height * 100).toFixed(1)}%)`)
    }
  }
}

writeAllRows()
stream.end(() => {
  console.log(`\n完成！文件大小：${(fileSize / (1024 ** 3)).toFixed(3)}GB → ${output}`)
})
