//! 压缩引擎：PNG 走 imagequant(pngquant 官方 Rust 绑定) + oxipng，
//! JPEG 走 mozjpeg，WebP 走 libwebp。
//! 全部无参数 —— 质量区间在库内部自动择优，调用方不传任何 quality。

use std::fs;
use std::io::Cursor;

use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

/// PNG 量化质量区间 [下限, 上限]。
/// 语义是「相似度」而非「压缩强度」：允许最高 80，但不低于 60。
/// imagequant 会在区间内迭代试探，自动找到体积最小且肉眼无差的那个点。
const PNG_QUALITY_MIN: u8 = 60;
const PNG_QUALITY_MAX: u8 = 80;
const PNG_SPEED: u8 = 4; // 1(最慢最好) ~ 10(最快)

/// JPEG 目标质量。mozjpeg 的 trellis 量化会在此基础上再省 10-20%。
const JPEG_QUALITY: f32 = 82.0;

/// WebP 目标质量。
const WEBP_QUALITY: f32 = 80.0;

/// 无损收尾强度：1(快) ~ 6(极慢)。TinyPNG 的收尾大致落在 2-3。
const OXIPNG_LEVEL: u8 = 3;

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 输出边长相对经 EXIF 方向校正后图像的百分比。
    pub scale_percent: u8,
    /// 目标宽度（像素）。若设置，将自动计算 scale_percent 并保持宽高比。
    pub target_width: Option<u32>,
    /// 目标高度（像素）。若设置（且未设置 target_width），将自动计算 scale_percent 并保持宽高比。
    pub target_height: Option<u32>,
    /// 是否保留版权信息（copyright）。false = 移除。
    pub keep_copyright: bool,
    /// 是否保留位置信息（GPS / 地理坐标）。false = 移除。
    pub keep_location: bool,
    /// 是否保留创建日期。false = 移除。
    pub keep_creation: bool,
}

impl Options {
    /// 三项元数据是否全部保留（决定 JPEG / WebP 是否整体保留 EXIF）。
    pub fn keep_all_metadata(&self) -> bool {
        self.keep_copyright && self.keep_location && self.keep_creation
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scale_percent: 100,
            target_width: None,
            target_height: None,
            keep_copyright: false,
            keep_location: false,
            keep_creation: false,
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub orig_size: u64,
    pub new_size: u64,
    /// true = 未采用有损重编码结果；输出为原图，或仅做了安全 PNG 元数据清理。
    pub keep_original: bool,
    pub out_width: u32,
    pub out_height: u32,
    pub metadata_stripped: bool,
}

struct DecodedImage {
    image: DynamicImage,
    source_width: u32,
    source_height: u32,
}

struct EncodedImage {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

pub fn compress_file(src: &std::path::Path, dst: &std::path::Path, opts: &Options) -> Result<Outcome, String> {
    if !(1..=100).contains(&opts.scale_percent) {
        return Err("缩放百分比必须在 1 到 100 之间".to_string());
    }

    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let format = match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::WebP,
        other => return Err(format!("不支持的格式: {other}")),
    };

    let orig = fs::read(src).map_err(|e| format!("读取失败: {e}"))?;
    let orig_size = orig.len() as u64;
    let decoded = decode_image(&orig, format, opts)?;

    let (encoded, meta_stripped) = match format {
        ImageFormat::Png => {
            let (e, _) = encode_png(&decoded.image, opts)?;
            // 重编码会丢弃原图文本/EXIF 块，这里按选项把「要保留」的块重新注入
            let kept = collect_kept_png_chunks(&orig, opts);
            let bytes = inject_png_chunks(&e.bytes, &kept);
            let stripped = source_has_png_meta(&orig) && !opts.keep_all_metadata();
            (EncodedImage { bytes, width: e.width, height: e.height }, stripped)
        }
        ImageFormat::Jpeg => {
            let e = encode_jpeg(&decoded.image)?;
            let bytes = if opts.keep_all_metadata() {
                embed_jpeg_exif(&orig, &e.bytes).unwrap_or_else(|_| e.bytes)
            } else {
                e.bytes
            };
            let stripped = !opts.keep_all_metadata();
            (
                EncodedImage { bytes, width: e.width, height: e.height },
                stripped,
            )
        }
        ImageFormat::WebP => {
            let e = encode_webp(&decoded.image)?;
            let bytes = if opts.keep_all_metadata() {
                embed_webp_exif(&orig, &e.bytes).unwrap_or_else(|_| e.bytes)
            } else {
                e.bytes
            };
            let stripped = !opts.keep_all_metadata();
            (
                EncodedImage { bytes, width: e.width, height: e.height },
                stripped,
            )
        }
        _ => unreachable!("已在扩展名校验中限制格式"),
    };

    // 只在新编码严格更小时采用，避免以体积和画质交换一个「已压缩」标记。
    let (out, keep_original, out_width, out_height, metadata_stripped) =
        if (encoded.bytes.len() as u64) < orig_size {
            (
                encoded.bytes,
                false,
                encoded.width,
                encoded.height,
                meta_stripped,
            )
        } else if format == ImageFormat::Png {
            // 量化结果不够小时，仍可在不改动像素的前提下安全清理 PNG 块；
            // 同样必须变小才采用。
            match strip_png_metadata(&orig, opts) {
                Ok((cleaned, removed)) if (cleaned.len() as u64) < orig_size => (
                    cleaned,
                    true,
                    decoded.source_width,
                    decoded.source_height,
                    removed,
                ),
                _ => (
                    orig.clone(),
                    true,
                    decoded.source_width,
                    decoded.source_height,
                    false,
                ),
            }
        } else {
            (
                orig.clone(),
                true,
                decoded.source_width,
                decoded.source_height,
                false,
            )
        };

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {e}"))?;
    }
    fs::write(dst, &out).map_err(|e| format!("写入失败: {e}"))?;

    Ok(Outcome {
        orig_size,
        new_size: out.len() as u64,
        keep_original,
        out_width,
        out_height,
        metadata_stripped,
    })
}

// ---------------------------------------------------------------- PNG

/// 编码（量化 + oxipng 优化），并按选项细粒度清理文本 / EXIF / 时间块。
/// 返回 (优化后的 PNG 字节, 是否移除了任何元数据块)。
fn encode_png(img: &DynamicImage, opts: &Options) -> Result<(EncodedImage, bool), String> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let pixels: Vec<imagequant::RGBA> = rgba
        .pixels()
        .map(|p| imagequant::RGBA::new(p[0], p[1], p[2], p[3]))
        .collect();

    let mut liq = imagequant::Attributes::new();
    liq.set_speed(PNG_SPEED.into())
        .map_err(|e| format!("imagequant 初始化失败: {e}"))?;
    liq.set_quality(PNG_QUALITY_MIN, PNG_QUALITY_MAX)
        .map_err(|e| format!("imagequant 设置质量失败: {e}"))?;

    let raw_png = match liq.new_image_borrowed(&pixels, w, h, 0.0) {
        Ok(mut im) => {
            let bytes = match liq.quantize(&mut im) {
                Ok(mut res) => {
                    let _ = res.set_dithering_level(1.0);
                    match res.remapped(&mut im) {
                        Ok((palette, indices)) => encode_indexed_png(w as u32, h as u32, &palette, &indices)?,
                        Err(_) => encode_rgba_png(&rgba)?,
                    }
                }
                Err(_) => encode_rgba_png(&rgba)?,
            };
            bytes
        }
        Err(_) => encode_rgba_png(&rgba)?,
    };

    let (bytes, removed) = filter_png_metadata(&raw_png, opts)?;
    let mut oxi = oxipng::Options::from_preset(OXIPNG_LEVEL);
    oxi.strip = oxipng::StripChunks::None;
    let bytes = oxipng::optimize_from_memory(&bytes, &oxi).map_err(|e| format!("oxipng 失败: {e}"))?;
    Ok((EncodedImage { bytes, width: w as u32, height: h as u32 }, removed))
}

/// 仅做安全元数据清理（不重编码像素）时的 PNG 处理。
fn strip_png_metadata(data: &[u8], opts: &Options) -> Result<(Vec<u8>, bool), String> {
    let (filtered, removed) = filter_png_metadata(data, opts)?;
    let mut oxi = oxipng::Options::from_preset(OXIPNG_LEVEL);
    oxi.strip = oxipng::StripChunks::None;
    let bytes = oxipng::optimize_from_memory(&filtered, &oxi).map_err(|e| format!("oxipng 失败: {e}"))?;
    Ok((bytes, removed))
}

/// 按「版权 / 位置 / 创建日期」三类细粒度过滤 PNG 块：
/// - tEXt / iTXt / zTXt 文本块：按 keyword 归类到三类，对应开关关闭则丢弃；
///   未归类的文本块（如描述、作者、软件）保守保留。
/// - eXIf：PNG 内嵌 EXIF 无法细分，仅当三项全保留时保留。
/// - tIME：创建 / 修改时间，由 keep_creation 控制。
/// - 其余块（IHDR/IDAT/PLTE/iCCP/tRNS/bKGD/pHYs/gAMA/sRGB 等）一律保留。
fn filter_png_metadata(data: &[u8], opts: &Options) -> Result<(Vec<u8>, bool), String> {
    if data.len() < 8 || &data[0..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("不是有效的 PNG 数据".to_string());
    }
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[0..8]);
    let mut removed = false;
    let mut pos = 8;
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let chunk_type = &data[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start + len;
        let crc_end = data_end + 4;
        if crc_end > data.len() {
            break;
        }
        let keep = should_keep_png_chunk(chunk_type, &data[data_start..data_end], opts, &mut removed);
        if keep {
            out.extend_from_slice(&data[pos..crc_end]);
        }
        pos = crc_end;
    }
    Ok((out, removed))
}

fn should_keep_png_chunk(typ: &[u8], chunk_data: &[u8], opts: &Options, removed: &mut bool) -> bool {
    let keep = match typ {
        b"tEXt" | b"iTXt" | b"zTXt" => {
            let key = text_keyword(typ, chunk_data).to_ascii_lowercase();
            let cat = if key.contains("copyright") || key.contains("cpr") {
                Some(opts.keep_copyright)
            } else if key.contains("gps")
                || key.contains("latitude")
                || key.contains("longitude")
                || key.contains("location")
                || key.contains("altitude")
                || key.contains("geotag")
                || key.contains("position")
            {
                Some(opts.keep_location)
            } else if key.contains("creation")
                || key.contains("datetime")
                || key.contains("date:create")
                || key.contains("date:modify")
                || key.contains("time:create")
                || key.contains("time:modify")
                || key == "xml:com.adobe.xmp"
            {
                Some(opts.keep_creation)
            } else {
                None
            };
            match cat {
                Some(k) => k,
                None => true, // 未归类的文本块保留
            }
        }
        b"eXIf" => opts.keep_all_metadata(),
        b"tIME" => opts.keep_creation,
        _ => true,
    };
    if !keep {
        *removed = true;
    }
    keep
}

/// 提取文本块（tEXt/iTXt/zTXt）的 keyword：第一个 NUL 之前的部分（Latin-1/UTF-8 均取首段）。
fn text_keyword(typ: &[u8], data: &[u8]) -> String {
    let _ = typ;
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).to_string()
}

/// 从原 PNG 中收集「应当保留」的元数据块（tEXt/iTXt/zTXt/eXIf/tIME）原始字节，
/// 用于重编码后重新注入。返回完整的 长度+类型+数据+CRC 块字节。
fn collect_kept_png_chunks(src: &[u8], opts: &Options) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if src.len() < 8 || &src[0..8] != b"\x89PNG\r\n\x1a\n" {
        return out;
    }
    let mut pos = 8;
    let mut dummy = false;
    while pos + 8 <= src.len() {
        let len = u32::from_be_bytes([src[pos], src[pos + 1], src[pos + 2], src[pos + 3]]) as usize;
        let chunk_type = &src[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start + len;
        let crc_end = data_end + 4;
        if crc_end > src.len() {
            break;
        }
        let keep = should_keep_png_chunk(chunk_type, &src[data_start..data_end], opts, &mut dummy);
        if keep && (chunk_type == b"tEXt" || chunk_type == b"iTXt" || chunk_type == b"zTXt" || chunk_type == b"eXIf" || chunk_type == b"tIME") {
            out.push(src[pos..crc_end].to_vec());
        }
        pos = crc_end;
    }
    out
}

/// 将保留的 PNG 块插入到已编码 PNG 的 IEND 之前。
fn inject_png_chunks(encoded: &[u8], chunks: &[Vec<u8>]) -> Vec<u8> {
    if chunks.is_empty() {
        return encoded.to_vec();
    }
    // position 指向 IEND 块的起始（其 4 字节长度字段），在其之前插入
    let iend = encoded
        .windows(8)
        .position(|w| w[4..] == *b"IEND")
        .unwrap_or(encoded.len());
    let extra: usize = chunks.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(encoded.len() + extra);
    out.extend_from_slice(&encoded[..iend]);
    for c in chunks {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&encoded[iend..]);
    out
}

/// 原 PNG 是否包含任何可归类元数据块（用于判断 metadata_stripped）。
fn source_has_png_meta(src: &[u8]) -> bool {
    if src.len() < 8 || &src[0..8] != b"\x89PNG\r\n\x1a\n" {
        return false;
    }
    let mut pos = 8;
    while pos + 8 <= src.len() {
        let len = u32::from_be_bytes([src[pos], src[pos + 1], src[pos + 2], src[pos + 3]]) as usize;
        let chunk_type = &src[pos + 4..pos + 8];
        let data_end = pos + 8 + len;
        let crc_end = data_end + 4;
        if crc_end > src.len() {
            break;
        }
        if chunk_type == b"tEXt" || chunk_type == b"iTXt" || chunk_type == b"zTXt" || chunk_type == b"eXIf" || chunk_type == b"tIME" {
            return true;
        }
        pos = crc_end;
    }
    false
}

fn encode_rgba_png(rgba: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, rgba.width(), rgba.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("PNG 头写入失败: {e}"))?;
        writer
            .write_image_data(rgba.as_raw())
            .map_err(|e| format!("PNG 数据写入失败: {e}"))?;
        writer.finish().map_err(|e| format!("PNG 收尾失败: {e}"))?;
    }
    Ok(buf)
}

fn encode_indexed_png(
    w: u32,
    h: u32,
    palette: &[imagequant::RGBA],
    indices: &[u8],
) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Eight);

        let mut pal_rgb = Vec::with_capacity(palette.len() * 3);
        let mut pal_a = Vec::with_capacity(palette.len());
        let mut has_alpha = false;
        for c in palette {
            pal_rgb.extend_from_slice(&[c.r, c.g, c.b]);
            has_alpha |= c.a != 255;
            pal_a.push(c.a);
        }
        enc.set_palette(pal_rgb);
        if has_alpha {
            enc.set_trns(pal_a);
        }

        let mut writer = enc
            .write_header()
            .map_err(|e| format!("PNG 头写入失败: {e}"))?;
        writer
            .write_image_data(indices)
            .map_err(|e| format!("PNG 数据写入失败: {e}"))?;
        writer.finish().map_err(|e| format!("PNG 收尾失败: {e}"))?;
    }
    Ok(buf)
}

// ---------------------------------------------------------------- JPEG

fn encode_jpeg(img: &DynamicImage) -> Result<EncodedImage, String> {
    let rgb = img.to_rgb8();
    let (width, height) = rgb.dimensions();
    let raw = rgb.into_raw();

    let mut comp = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
    comp.set_size(width as usize, height as usize);
    comp.set_quality(JPEG_QUALITY);
    comp.set_optimize_coding(true);
    comp.set_use_scans_in_trellis(true);
    comp.set_optimize_scans(true);
    let mut comp = comp
        .start_compress(Vec::new())
        .map_err(|e| format!("mozjpeg 启动失败: {e}"))?;
    comp.write_scanlines(&raw)
        .map_err(|e| format!("mozjpeg 写入失败: {e}"))?;
    let bytes = comp
        .finish()
        .map_err(|e| format!("mozjpeg 完成失败: {e}"))?;
    Ok(EncodedImage {
        bytes,
        width,
        height,
    })
}

/// 从原 JPEG 中提取 EXIF APP1 段（含 "Exif\0\0" 头与 TIFF 数据）。
fn extract_jpeg_exif(orig: &[u8]) -> Option<Vec<u8>> {
    if orig.len() < 4 || &orig[0..2] != b"\xff\xd8" {
        return None;
    }
    let mut i = 2; // 跳过 SOI
    while i + 4 <= orig.len() {
        if orig[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = orig[i + 1];
        // 遇到 EOI 或 SOS 后不再有 APP 段
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if i + 4 > orig.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([orig[i + 2], orig[i + 3]]) as usize;
        let seg_end = i + 2 + seg_len;
        if marker == 0xE1 && seg_end <= orig.len() && i + 10 <= orig.len() && &orig[i + 4..i + 8] == b"Exif" && orig[i + 8] == 0 && orig[i + 9] == 0 {
            return Some(orig[i..seg_end].to_vec());
        }
        i = seg_end;
    }
    None
}

/// 在已编码 JPEG（mozjpeg 输出，无 EXIF）的 SOI 之后插入原图 EXIF 段。
fn embed_jpeg_exif(orig: &[u8], encoded: &[u8]) -> Result<Vec<u8>, String> {
    let seg = extract_jpeg_exif(orig).ok_or_else(|| "原图不含 EXIF".to_string())?;
    if encoded.len() < 2 || encoded[0] != 0xFF || encoded[1] != 0xD8 {
        return Err("编码结果不是 JPEG".to_string());
    }
    let mut out = Vec::with_capacity(encoded.len() + seg.len());
    out.extend_from_slice(&encoded[0..2]); // SOI
    out.extend_from_slice(&seg);
    out.extend_from_slice(&encoded[2..]);
    Ok(out)
}

// ---------------------------------------------------------------- WebP

fn encode_webp(img: &DynamicImage) -> Result<EncodedImage, String> {
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let raw = rgba.into_raw();
    let enc = webp::Encoder::from_rgba(&raw, width, height);
    Ok(EncodedImage {
        bytes: enc.encode(WEBP_QUALITY).to_vec(),
        width,
        height,
    })
}

/// 从原 WebP (RIFF) 中提取 EXIF 块数据。
fn extract_webp_exif(orig: &[u8]) -> Option<Vec<u8>> {
    if orig.len() < 12 || &orig[0..4] != b"RIFF" || &orig[8..12] != b"WEBP" {
        return None;
    }
    let mut pos = 12;
    while pos + 8 <= orig.len() {
        let fourcc = &orig[pos..pos + 4];
        let size = u32::from_le_bytes([orig[pos + 4], orig[pos + 5], orig[pos + 6], orig[pos + 7]]) as usize;
        let data_start = pos + 8;
        let data_end = (data_start + size).min(orig.len());
        if fourcc == b"EXIF" {
            return Some(orig[data_start..data_end].to_vec());
        }
        let padded = size + (size & 1);
        pos = data_start + padded;
    }
    None
}

/// 将 EXIF 数据重新组装进已编码 WebP 容器（追加为 EXIF 块）。
fn embed_webp_exif(orig: &[u8], encoded: &[u8]) -> Result<Vec<u8>, String> {
    let exif = extract_webp_exif(orig).ok_or_else(|| "原图不含 EXIF".to_string())?;
    if encoded.len() < 12 || &encoded[0..4] != b"RIFF" || &encoded[8..12] != b"WEBP" {
        return Err("编码结果不是 WebP".to_string());
    }
    // 解析已编码 WebP 的所有块
    let mut chunks: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut pos = 12;
    while pos + 8 <= encoded.len() {
        let fourcc = encoded[pos..pos + 4].to_vec();
        let size = u32::from_le_bytes([encoded[pos + 4], encoded[pos + 5], encoded[pos + 6], encoded[pos + 7]]) as usize;
        let data_start = pos + 8;
        let data_end = (data_start + size).min(encoded.len());
        chunks.push((fourcc, encoded[data_start..data_end].to_vec()));
        let padded = size + (size & 1);
        pos = data_start + padded;
    }
    // 重组：图片数据块在前，EXIF 块追加其后
    let mut body = Vec::new();
    for (fourcc, data) in &chunks {
        body.extend_from_slice(fourcc);
        let size = data.len() as u32;
        body.extend_from_slice(&size.to_le_bytes());
        body.extend_from_slice(data);
        if data.len() & 1 == 1 {
            body.push(0);
        }
    }
    let mut exif_block = Vec::new();
    exif_block.extend_from_slice(b"EXIF");
    let esize = exif.len() as u32;
    exif_block.extend_from_slice(&esize.to_le_bytes());
    exif_block.extend_from_slice(&exif);
    if exif.len() & 1 == 1 {
        exif_block.push(0);
    }
    let mut out = Vec::with_capacity(12 + body.len() + exif_block.len());
    out.extend_from_slice(b"RIFF");
    let total = (body.len() + exif_block.len()) as u32;
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    out.extend_from_slice(&exif_block);
    Ok(out)
}

// ---------------------------------------------------------------- 工具

fn decode_image(data: &[u8], format: ImageFormat, opts: &Options) -> Result<DecodedImage, String> {
    let (mut image, source_width, source_height) = decode_image_rgb(data, format)?;

    let scale_percent = resolve_scale_percent(opts, source_width, source_height);

    if scale_percent != 100 {
        let (width, height) = scaled_dimensions(image.width(), image.height(), scale_percent);
        image = image.resize_exact(width, height, FilterType::Lanczos3);
    }

    Ok(DecodedImage {
        image,
        source_width,
        source_height,
    })
}

/// 解码为 sRGB DynamicImage（不缩放）。返回 (图, 旋转前宽, 旋转前高)。
/// 压缩与缩略图共用。CMYK/YCCK JPEG（Illustrator/Photoshop 导出的印刷稿常见）必须走
/// 「raw CMYK + ICC profile → moxcms 精确转换到 sRGB」路径，
/// 否则 image 0.25 内部的 zune-jpeg 只做公式换算、输出明显偏色（变亮、变绿、变鲜艳）。
pub fn decode_image_rgb(data: &[u8], format: ImageFormat) -> Result<(DynamicImage, u32, u32), String> {
    if format == ImageFormat::Jpeg && is_ink_jpeg(data) {
        let image = decode_cmyk_dynamic(data)?;
        let (source_width, source_height) = (image.width(), image.height());
        return Ok((image, source_width, source_height));
    }

    let reader = ImageReader::with_format(Cursor::new(data), format);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("解码器初始化失败: {e}"))?;
    let (source_width, source_height) = decoder.dimensions();
    let orientation = decoder
        .orientation()
        .unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| format!("解码失败: {e}"))?;
    image.apply_orientation(orientation);
    Ok((image, source_width, source_height))
}

fn resolve_scale_percent(opts: &Options, source_width: u32, source_height: u32) -> u8 {
    if let Some(target_width) = opts.target_width {
        if source_width == 0 {
            100
        } else {
            let pct = (u64::from(target_width) * 100) / u64::from(source_width);
            pct.clamp(1, 100) as u8
        }
    } else if let Some(target_height) = opts.target_height {
        if source_height == 0 {
            100
        } else {
            let pct = (u64::from(target_height) * 100) / u64::from(source_height);
            pct.clamp(1, 100) as u8
        }
    } else {
        opts.scale_percent
    }
}

/// 是否为 Adobe 印刷色 JPEG（CMYK 或 YCCK，Illustrator/Photoshop 导出常见）。
fn is_ink_jpeg(data: &[u8]) -> bool {
    let mut decoder = zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(data));
    if decoder.decode_headers().is_err() {
        return false;
    }
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    matches!(decoder.input_colorspace(), Some(ColorSpace::CMYK | ColorSpace::YCCK))
}

/// CMYK/YCCK JPEG → sRGB DynamicImage（不缩放）。压缩输出与缩略图共用。
fn decode_cmyk_dynamic(data: &[u8]) -> Result<DynamicImage, String> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let input_space = {
        let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(data));
        decoder.decode_headers().map_err(|e| format!("JPEG 解析失败: {e}"))?;
        decoder.input_colorspace().ok_or("无法确定 JPEG 色彩空间")?
    };

    // zune-jpeg 不支持跨空间转出 CMYK（没有 YCCK→CMYK 分支），
    // 但 input==output 时是直拷贝 —— 按原始空间拿 raw 4 通道数据
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(
        ZCursor::new(data),
        DecoderOptions::default().jpeg_set_out_colorspace(input_space),
    );
    decoder.decode_headers().map_err(|e| format!("JPEG 解析失败: {e}"))?;
    let info = decoder.info().ok_or("无法读取 JPEG 信息")?;
    let source_width = u32::try_from(info.width).map_err(|_| "JPEG 宽度异常".to_string())?;
    let source_height = u32::try_from(info.height).map_err(|_| "JPEG 高度异常".to_string())?;
    let icc = decoder.icc_profile();
    let raw = decoder.decode().map_err(|e| format!("解码失败: {e}"))?;

    // 统一转成「标准约定 CMYK」（0=无墨）：
    // - YCCK（Adobe transform=2，Illustrator 常见）：先做 YCbCr→RGB 反推 CMY，K 反相直通
    // - CMYK（Adobe transform=0）：存储即反相值，直接 255-x 反转
    let standard_cmyk;
    let inverted;
    if input_space == ColorSpace::YCCK {
        standard_cmyk = ycck_to_cmyk(&raw);
        inverted = false;
    } else {
        standard_cmyk = raw;
        inverted = jpeg_has_adobe_marker(data);
    }

    // CMYK 印刷稿一般不带 EXIF 方向（Illustrator 导出没有 EXIF Orientation），不做旋转
    Ok(DynamicImage::ImageRgb8(cmyk_to_srgb(
        &standard_cmyk,
        source_width,
        source_height,
        icc.as_deref(),
        inverted,
    )?))
}

/// YCCK（Adobe 反相域）→ 标准约定 CMYK（0=无墨）。
/// Y/Cb/Cr 按 JPEG 标准公式反推 RGB，取补得到 C/M/Y；K 反相直通。
fn ycck_to_cmyk(ycck: &[u8]) -> Vec<u8> {
    let pixels = ycck.len() / 4;
    let mut out = vec![0u8; pixels * 4];
    for index in 0..pixels {
        let y = ycck[index * 4] as f32;
        let cb = ycck[index * 4 + 1] as f32 - 128.0;
        let cr = ycck[index * 4 + 2] as f32 - 128.0;
        let r = y + 1.402 * cr;
        let g = y - 0.344_136 * cb - 0.714_136 * cr;
        let b = y + 1.772 * cb;
        out[index * 4] = 255 - (255.0 - r).round().clamp(0.0, 255.0) as u8;
        out[index * 4 + 1] = 255 - (255.0 - g).round().clamp(0.0, 255.0) as u8;
        out[index * 4 + 2] = 255 - (255.0 - b).round().clamp(0.0, 255.0) as u8;
        out[index * 4 + 3] = 255 - ycck[index * 4 + 3];
    }
    out
}

/// 扫描 JPEG 段找 APP14 "Adobe" 标记 —— Adobe 工具导出的 CMYK 通道值是反相的（0=满墨）。
fn jpeg_has_adobe_marker(data: &[u8]) -> bool {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 { return false; }
    let mut pos = 2;
    while pos + 4 <= data.len() && data[pos] == 0xFF {
        let marker = data[pos + 1];
        if marker == 0xFF { pos += 1; continue; } // 填充字节
        if marker == 0xD9 || marker == 0xDA { break; } // EOI / SOS：数据流开始，停止扫描
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) { pos += 2; continue; }
        let seg_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if seg_len < 2 || pos + 2 + seg_len > data.len() { break; }
        if marker == 0xEE && pos + 9 <= data.len() && &data[pos + 4..pos + 9] == b"Adobe" { return true; }
        pos += 2 + seg_len;
    }
    false
}

/// CMYK raw 数据 → sRGB。
/// 有 ICC profile 时走 moxcms 精确转换（与 TinyPNG 同级效果）；
/// 无 ICC 时退回 naive 减色公式（R = 255 - C·(1-K)）。
/// `inverted` = Adobe 反相约定（存储值 0=满墨），先反转成 ICC 标准约定（0=无墨）。
fn cmyk_to_srgb(cmyk: &[u8], width: u32, height: u32, icc: Option<&[u8]>, inverted: bool) -> Result<image::RgbImage, String> {
    let pixels = width as usize * height as usize;
    if pixels == 0 || cmyk.len() < pixels * 4 {
        return Err("CMYK 数据不完整".to_string());
    }

    let to_standard = |value: u8| if inverted { 255 - value } else { value };

    if let Some(icc_bytes) = icc {
        let source_profile = moxcms::ColorProfile::new_from_slice(icc_bytes)
            .map_err(|error| format!("ICC profile 解析失败: {error}"))?;
        let srgb = moxcms::ColorProfile::new_srgb();
        let transform = source_profile
            .create_transform_8bit(
                // moxcms 约定：CMYK 数据用 Layout::Rgba 的 4 个通道槽承载（见 profile.rs check_layout）
                moxcms::Layout::Rgba,
                &srgb,
                moxcms::Layout::Rgb,
                moxcms::TransformOptions::default(),
            )
            .map_err(|error| format!("创建色彩转换失败: {error}"))?;
        let mut rgba_cmyk = vec![0u8; pixels * 4];
        for index in 0..pixels {
            let src = &cmyk[index * 4..index * 4 + 4];
            let dst = &mut rgba_cmyk[index * 4..index * 4 + 4];
            dst[0] = to_standard(src[0]);
            dst[1] = to_standard(src[1]);
            dst[2] = to_standard(src[2]);
            dst[3] = to_standard(src[3]);
        }
        let mut out = vec![0u8; pixels * 3];
        transform.transform(&rgba_cmyk, &mut out).map_err(|error| format!("CMYK→sRGB 转换失败: {error}"))?;
        return image::RgbImage::from_raw(width, height, out).ok_or_else(|| "RGB 数据尺寸不符".to_string());
    }

    // 无 ICC：naive 减色兜底
    let mut out = vec![0u8; pixels * 3];
    for index in 0..pixels {
        let src = &cmyk[index * 4..index * 4 + 4];
        let k = to_standard(src[3]) as u32;
        let one_minus_k = 255 - k;
        let calc = |channel: u8| -> u8 {
            let value = to_standard(channel) as u32;
            255 - ((value * one_minus_k + 127) / 255) as u8
        };
        out[index * 3] = calc(src[0]);
        out[index * 3 + 1] = calc(src[1]);
        out[index * 3 + 2] = calc(src[2]);
    }
    image::RgbImage::from_raw(width, height, out).ok_or_else(|| "RGB 数据尺寸不符".to_string())
}

fn scaled_dimensions(width: u32, height: u32, scale_percent: u8) -> (u32, u32) {
    let scale = u64::from(scale_percent);
    let scaled = |dimension: u32| {
        ((u64::from(dimension) * scale + 50) / 100)
            .max(1)
            .min(u64::from(u32::MAX)) as u32
    };
    (scaled(width), scaled(height))
}

#[allow(dead_code)]
pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod validate {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use std::path::Path;

    /// 解码原图与压缩图（统一转 RGBA），在 RGB 三通道上算 PSNR。
    /// >35dB 一般视为肉眼无差。
    fn psnr(orig: &str, new: &str) -> Option<f64> {
        let o = image::open(orig).ok()?.to_rgba8();
        let n = image::open(new).ok()?.to_rgba8();
        if o.dimensions() != n.dimensions() {
            return None;
        }
        let mut mse = 0.0f64;
        let mut count = 0u64;
        for (a, b) in o.pixels().zip(n.pixels()) {
            for c in 0..3 {
                let d = a[c] as f64 - b[c] as f64;
                mse += d * d;
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        mse /= count as f64;
        if mse == 0.0 {
            return Some(f64::INFINITY);
        }
        Some(10.0 * (255.0_f64 * 255.0_f64 / mse).log10())
    }

    #[test]
    fn scales_output_dimensions() {
        let image = image::RgbImage::from_fn(200, 100, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut source = Vec::new();
        JpegEncoder::new_with_quality(&mut source, 100)
            .encode_image(&DynamicImage::ImageRgb8(image))
            .unwrap();

        let dir = std::env::temp_dir().join(format!("tinypic_scale_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.jpg");
        let dst = dir.join("output.jpg");
        std::fs::write(&src, source).unwrap();

        let result = compress_file(
            &src,
            &dst,
            &Options { scale_percent: 50, target_width: None, target_height: None, keep_copyright: false, keep_location: false, keep_creation: false },
        )
        .unwrap();
        assert!(!result.keep_original);
        assert_eq!((result.out_width, result.out_height), (100, 50));
        assert_eq!(image::image_dimensions(&dst).unwrap(), (100, 50));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn target_width_scales_proportionally() {
        let image = image::RgbImage::from_fn(200, 100, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut source = Vec::new();
        JpegEncoder::new_with_quality(&mut source, 100)
            .encode_image(&DynamicImage::ImageRgb8(image))
            .unwrap();

        let dir = std::env::temp_dir().join(format!("tinypic_tw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.jpg");
        let dst = dir.join("output.jpg");
        std::fs::write(&src, source).unwrap();

        let target = 80u32; // 200 -> 80 即 40%，高度应 100 -> 40
        let result = compress_file(
            &src,
            &dst,
            &Options { scale_percent: 100, target_width: Some(target), target_height: None, keep_copyright: false, keep_location: false, keep_creation: false },
        )
        .unwrap();
        assert!(!result.keep_original);
        assert_eq!((result.out_width, result.out_height), (80, 40));
        assert_eq!(image::image_dimensions(&dst).unwrap(), (80, 40));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn target_height_scales_proportionally() {
        let image = image::RgbImage::from_fn(200, 100, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut source = Vec::new();
        JpegEncoder::new_with_quality(&mut source, 100)
            .encode_image(&DynamicImage::ImageRgb8(image))
            .unwrap();

        let dir = std::env::temp_dir().join(format!("tinypic_th_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("source.jpg");
        let dst = dir.join("output.jpg");
        std::fs::write(&src, source).unwrap();

        let target = 40u32; // 100 -> 40 即 40%，宽度应 200 -> 80
        let result = compress_file(
            &src,
            &dst,
            &Options { scale_percent: 100, target_width: None, target_height: Some(target), keep_copyright: false, keep_location: false, keep_creation: false },
        )
        .unwrap();
        assert!(!result.keep_original);
        assert_eq!((result.out_width, result.out_height), (80, 40));
        assert_eq!(image::image_dimensions(&dst).unwrap(), (80, 40));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn real_image_validation() {
        let base = format!("{}/../tests", env!("CARGO_MANIFEST_DIR"));
        let cases = ["photo.png", "photo.jpg", "logo.png", "logo.webp"];
        let out = std::env::temp_dir().join("tiny_validate");
        std::fs::create_dir_all(&out).unwrap();
        for name in cases {
            let src = Path::new(&base).join(name);
            let dst = out.join(name);
            let opts = Options::default();
            let r = compress_file(&src, &dst, &opts).expect("compress");
            let p = psnr(src.to_str().unwrap(), dst.to_str().unwrap()).unwrap_or(f64::NAN);
            let pct = if r.orig_size > 0 {
                (1.0 - r.new_size as f64 / r.orig_size as f64) * 100.0
            } else {
                0.0
            };
            println!(
                "VALIDATE {name}: {:.1}KB -> {:.1}KB ({:.1}% saved), kept={}, {}x{}, metadata_stripped={}, PSNR={:.2}dB",
                r.orig_size as f64 / 1024.0,
                r.new_size as f64 / 1024.0,
                pct,
                r.keep_original,
                r.out_width,
                r.out_height,
                r.metadata_stripped,
                p
            );
        }
    }

    #[test]
    fn png_text_chunk_filtering() {
        // 构造含版权 / 位置 / 创建 / 未归类 文本块的合法 PNG（注入到 IEND 之前），
        // 验证按开关细粒度保留 / 剥离。
        use image::RgbaImage;
        let img = RgbaImage::new(20, 20);
        let mut base = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut base, 20, 20);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(img.as_raw()).unwrap();
            w.finish().unwrap();
        }
        let make_text = |keyword: &str, value: &str| -> Vec<u8> {
            let mut payload = Vec::new();
            payload.extend_from_slice(keyword.as_bytes());
            payload.push(0);
            payload.extend_from_slice(value.as_bytes());
            let len = payload.len() as u32;
            let mut chunk = Vec::new();
            chunk.extend_from_slice(&len.to_be_bytes());
            chunk.extend_from_slice(b"tEXt");
            chunk.extend_from_slice(&payload);
            let crc = png_crc(&[b"tEXt".to_vec(), payload].concat());
            chunk.extend_from_slice(&crc.to_be_bytes());
            chunk
        };
        let chunks = vec![
            make_text("Copyright", "ACME Studio"),
            make_text("GPSLatitude", "31.23N"),
            make_text("Creation Time", "2024-01-01T00:00:00Z"),
            make_text("Software", "Demo"),
        ];
        let src_png = inject_png_chunks(&base, &chunks);

        let dir = std::env::temp_dir().join(format!("tiny_png_filter_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("m.png");
        let dst = dir.join("o.png");
        std::fs::write(&src, &src_png).unwrap();

        // 仅保留版权：应移除 GPS / Creation，保留 Copyright 与 未归类 Software
        let r = compress_file(&src, &dst, &Options { scale_percent: 100, target_width: None, target_height: None, keep_copyright: true, keep_location: false, keep_creation: false }).unwrap();
        assert!(r.metadata_stripped, "应检测到已剥离部分元数据");
        let out = std::fs::read(&dst).unwrap();
        assert!(out.windows(9).any(|w| w == b"Copyright"), "Copyright 应保留");
        assert!(!out.windows(11).any(|w| w == b"GPSLatitude"), "GPS 应被移除");
        assert!(!out.windows(12).any(|w| w == b"Creation Time"), "Creation 应被移除");
        assert!(out.windows(8).any(|w| w == b"Software"), "未归类 Software 应保留");

        let _ = std::fs::remove_dir_all(dir);
    }

    fn png_crc(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xffff_ffff;
        for &byte in data {
            let idx = ((crc ^ (byte as u32)) & 0xff) as usize;
            // 预计算表（PNG CRC32 多项式 0xEDB88320）
            crc = CRC_TABLE[idx] ^ (crc >> 8);
        }
        0xffff_ffff & !crc
    }

    static CRC_TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
}
