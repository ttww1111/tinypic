# 微图 / TinyPic

> 无参数图片压缩桌面工具 —— 引擎自动调参到 TinyPNG 级别质量，无需手动质量滑块。

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**微图（TinyPic）** 是一款基于 Tauri 2 的轻量桌面图片压缩工具，主打「无感压缩」：
拖入或粘贴图片即自动压缩，**不暴露任何质量滑块**，引擎自动把体积压到接近 TinyPNG 的水平。

## 特性

- 🗜️ **自动调参**：内置 imagequant / mozjpeg / libwebp，按格式自动选择最优参数，输出质量对标 TinyPNG
- 🖼️ **支持 PNG / JPEG / WebP** 三种格式
- ⚡ **拖放即压**：拖入文件、点击选择，或直接 `Ctrl+V` 粘贴剪贴板图片，立即开始压缩
- 📐 **调整尺寸**：可按宽度或高度等比缩放（留空则保持原稿大小）
- 🧾 **可选保留元数据**：分别控制是否保留版权信息 / 位置信息 / 创建日期

## 技术栈

- [Tauri 2](https://v2.tauri.app/)（Rust 后端 + WebView 前端）
- 编码库：imagequant（PNG 量化）/ mozjpeg（JPEG）/ libwebp（WebP）/ oxipng
- 前端：Vite + 原生 JS / CSS

## 下载与安装

前往 [Releases](https://github.com/ttww1111/tinypic/releases) 下载 Windows 版（v0.1.0）：

- **安装版** `TinyPic_0.1.0_x64-setup.exe`：双击安装，开始菜单 / 卸载程序里可卸载。
- **绿色免安装版** `TinyPic_0.1.0_x64_portable.zip`：解压出 `微图.exe` 直接双击运行，不写注册表，可放 U 盘 / 任意目录随身携带。

> 运行依赖系统自带的 **WebView2 运行时**（Win10 / 11 绝大多数已预装；若双击没反应，去微软官网装一次 Evergreen WebView2 Runtime 即可，属系统组件、非应用安装）。

## 使用

1. 打开微图，把图片拖入窗口（或点选 / `Ctrl+V` 粘贴）；
2. 引擎自动压缩，列表每行显示节省比例（如 `-9.2%`，体积变大时显示 `+X%`）；
3. 完成后点「打开文件夹」查看结果。

## 设置

点右上角齿轮打开设置页：

- **保存方式**：每次询问 / 替换原文件 / 存为新文件（`_tiny` 后缀）。
- **调整尺寸**：填宽度或高度其一，另一项按参考图等比自动换算；两项都留空则保持原尺寸。
- **保留元数据**：分别开关版权信息 / 位置信息 / 创建日期。

### 关于设置的保存

- **宽度 / 高度不持久化**：每次重新打开软件都会清空，因为不同图片尺寸不同，避免串味。
- 其余设置（保存方式、后缀、保留元数据）会保存在本机 `AppData\Local\com.tinypic.tinypic\...\Local Storage`（WebView2 的 localStorage）。
  > 注意：绿色版同样会把这部分设置写到上述 AppData 目录，并不会随 `微图.exe` 一起带走。当前版本**不是**「数据完全随 U 盘」的便携模式；若需要设置也跟着 exe 走，后续可把 webview 数据目录改到 exe 同目录。

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
- 安装包：`src-tauri/target/release/bundle/nsis/TinyPic_0.1.0_x64-setup.exe`

## 仓库

https://github.com/ttww1111/tinypic

## 许可证

[MIT](LICENSE) © 2026 Tony
