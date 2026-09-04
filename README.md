# 小图 / TinyPic

> 无参数图片压缩桌面工具 —— 引擎自动调参到 TinyPNG 级别质量，无需手动质量滑块。

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**小图（TinyPic）** 是一款基于 Tauri 2 的轻量桌面图片压缩工具，主打「无感压缩」：
拖入或粘贴图片即自动压缩，**不暴露任何质量滑块**，引擎自动把体积压到接近 TinyPNG 的水平。

## 特性

- 🗜️ **自动调参**：内置 imagequant / mozjpeg / libwebp，按格式自动选择最优参数，输出质量对标 TinyPNG
- 🖼️ **支持 PNG / JPEG / WebP** 三种格式
- ⚡ **拖放即压**：拖入文件、点击选择，或直接 `Ctrl+V` 粘贴剪贴板图片，立即开始压缩
- 📐 **调整尺寸**：可按宽度或高度等比缩放（留空则保持原稿大小）
- 🧾 **可选保留元数据**：分别控制是否保留版权信息 / 位置信息 / 创建日期
- 📂 **批量与输出管理**：实时显示节省比例与压缩后大小，一键打开输出文件夹

## 技术栈

- [Tauri 2](https://v2.tauri.app/)（Rust 后端 + WebView 前端）
- 编码库：imagequant（PNG 量化）/ mozjpeg（JPEG）/ libwebp（WebP）/ oxipng
- 前端：Vite + 原生 JS / CSS

## 下载与安装

前往 [Releases](https://github.com/ttww1111/tinypic/releases) 下载 Windows 安装包
`TinyPic_x.x.x_x64-setup.exe`，双击安装即可。

## 从源码构建

前置依赖：

- [Rust](https://www.rust-lang.org/)（stable）
- [Node.js](https://nodejs.org/) 18+
- Windows：需 [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/) 运行时（通常已随系统自带）

步骤：

```bash
npm install
npm run build
cargo tauri build
```

构建产物：

- 可执行文件：`src-tauri/target/release/tinypic.exe`
- 安装包：`src-tauri/target/release/bundle/nsis/TinyPic_x.x.x_x64-setup.exe`

## 使用

1. 打开小图，把图片拖入窗口（或点选 / `Ctrl+V` 粘贴）；
2. 引擎自动压缩，列表实时显示「节省 X% · 压缩后 Y」；
3. 完成后点「打开文件夹」查看结果。

## 仓库

https://github.com/ttww1111/tinypic

## 许可证

[MIT](LICENSE) © 2026 Tony
