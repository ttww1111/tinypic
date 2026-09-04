mod engine;

use base64::Engine as _;
use image::{DynamicImage, ImageFormat, ImageReader};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use tauri::Emitter;

const THUMBNAIL_PIXEL_LIMIT: u64 = 80_000_000;

static CANCEL: AtomicBool = AtomicBool::new(false);
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumbnail: Option<String>,
    pub format: Option<String>,
    pub mime: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedClip {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub mime: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressSettings {
    #[serde(default = "default_scale", alias = "scalePercent")]
    pub scale_percent: u8,
    #[serde(default = "default_target_width", alias = "targetWidth")]
    pub target_width: Option<u32>,
    #[serde(default = "default_target_height", alias = "targetHeight")]
    pub target_height: Option<u32>,
    #[serde(default = "default_keep_copyright", alias = "keepCopyright")]
    pub keep_copyright: bool,
    #[serde(default = "default_keep_location", alias = "keepLocation")]
    pub keep_location: bool,
    #[serde(default = "default_keep_creation", alias = "keepCreation")]
    pub keep_creation: bool,
    #[serde(default = "default_mode", alias = "outputMode", alias = "mode")]
    pub output_mode: String,
    #[serde(default = "default_suffix")]
    pub suffix: String,
}

fn default_scale() -> u8 { 100 }
fn default_target_width() -> Option<u32> { None }
fn default_target_height() -> Option<u32> { None }
fn default_keep_copyright() -> bool { false }
fn default_keep_location() -> bool { false }
fn default_keep_creation() -> bool { false }
fn default_mode() -> String { "new_file".to_string() }
fn default_suffix() -> String { "_tiny".to_string() }

impl Default for CompressSettings {
    fn default() -> Self {
        Self { scale_percent: default_scale(), target_width: default_target_width(), target_height: default_target_height(), keep_copyright: default_keep_copyright(), keep_location: default_keep_location(), keep_creation: default_keep_creation(), output_mode: default_mode(), suffix: default_suffix() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOutcome {
    pub path: String,
    pub name: String,
    pub orig: u64,
    pub new: u64,
    pub kept: bool,
    pub error: Option<String>,
    pub output_path: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub metadata_stripped: bool,
    pub effective_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    pub files: Vec<FileOutcome>,
    pub total_in: u64,
    pub total_out: u64,
    pub out_root: String,
    pub output_mode: String,
    pub cancelled: bool,
}

#[tauri::command]
fn file_sizes(paths: Vec<String>) -> Result<HashMap<String, u64>, String> {
    let mut result = HashMap::new();
    for path in paths {
        if let Ok(metadata) = fs::metadata(&path) { result.insert(path, metadata.len()); }
    }
    Ok(result)
}

#[tauri::command]
fn image_infos(paths: Vec<String>) -> Result<Vec<ImageInfo>, String> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().map_err(|error| error.to_string())?;
    Ok(pool.install(|| paths.par_iter().map(|path| inspect_image(path)).collect()))
}

fn inspect_image(path: &str) -> ImageInfo {
    let source = Path::new(path);
    let name = source.file_name().unwrap_or_default().to_string_lossy().to_string();
    let size = fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    let extension_format = source.extension().and_then(|extension| format_from_name(&extension.to_string_lossy()));
    let mut info = ImageInfo {
        path: path.to_string(), name, size, width: None, height: None, thumbnail: None,
        format: extension_format.map(|(_, format, _)| format.to_string()),
        mime: extension_format.map(|(_, _, mime)| mime.to_string()), error: None,
    };

    let (width, height) = match image::image_dimensions(path) {
        Ok(dimensions) => dimensions,
        Err(error) => { info.error = Some(error.to_string()); return info; }
    };
    info.width = Some(width);
    info.height = Some(height);

    if let Ok(reader) = ImageReader::open(path).and_then(|reader| reader.with_guessed_format()) {
        if let Some((_, format, mime)) = reader.format().and_then(format_from_image) {
            if info.format.is_none() { info.format = Some(format.to_string()); }
            if info.mime.is_none() { info.mime = Some(mime.to_string()); }
        }
    }
    if !should_make_thumbnail(width, height) { return info; }

    match image::open(path) {
        Ok(image) => info.thumbnail = make_thumbnail(&image).ok(),
        Err(error) => info.error = Some(error.to_string()),
    }
    info
}

fn should_make_thumbnail(width: u32, height: u32) -> bool {
    u64::from(width) * u64::from(height) <= THUMBNAIL_PIXEL_LIMIT
}

fn format_from_name(name: &str) -> Option<(ImageFormat, &'static str, &'static str)> {
    match name.to_ascii_lowercase().as_str() {
        "png" => Some((ImageFormat::Png, "png", "image/png")),
        "jpg" => Some((ImageFormat::Jpeg, "jpg", "image/jpeg")),
        "jpeg" => Some((ImageFormat::Jpeg, "jpeg", "image/jpeg")),
        "webp" => Some((ImageFormat::WebP, "webp", "image/webp")),
        _ => None,
    }
}

fn format_from_image(format: ImageFormat) -> Option<(ImageFormat, &'static str, &'static str)> {
    match format {
        ImageFormat::Png => format_from_name("png"),
        ImageFormat::Jpeg => format_from_name("jpg"),
        ImageFormat::WebP => format_from_name("webp"),
        _ => None,
    }
}

fn make_thumbnail(image: &DynamicImage) -> Result<String, String> {
    let thumbnail = image.thumbnail(64, 64);
    let mut bytes = Cursor::new(Vec::new());
    thumbnail.write_to(&mut bytes, ImageFormat::Png).map_err(|error| error.to_string())?;
    Ok(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())))
}

#[tauri::command]
fn save_clipboard_image(data: String) -> Result<SavedClip, String> {
    let bytes = decode_image_data(&data)?;
    let (format, extension, mime) = magic_format(&bytes).ok_or_else(|| "剪贴板内容不是支持的 PNG、JPEG 或 WebP 图片".to_string())?;
    let image = image::load_from_memory_with_format(&bytes, format).map_err(|error| format!("图片解码失败: {error}"))?;
    let root = clip_temp_root();
    fs::create_dir_all(&root).map_err(|error| format!("创建临时目录失败: {error}"))?;
    let sequence = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let path = unique_path(&root, &format!("clipboard_{sequence}"), extension);
    fs::write(&path, &bytes).map_err(|error| format!("保存剪贴板图片失败: {error}"))?;
    Ok(SavedClip {
        path: path.to_string_lossy().to_string(),
        name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        size: bytes.len() as u64, width: image.width(), height: image.height(),
        format: extension.to_string(), mime: mime.to_string(),
    })
}

fn decode_image_data(data: &str) -> Result<Vec<u8>, String> {
    let encoded = data.split_once(',').map(|(_, value)| value).unwrap_or(data).trim();
    base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|error| format!("base64 解码失败: {error}"))
}

fn magic_format(bytes: &[u8]) -> Option<(ImageFormat, &'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some((ImageFormat::Png, "png", "image/png")) }
    else if bytes.starts_with(b"\xff\xd8\xff") { Some((ImageFormat::Jpeg, "jpg", "image/jpeg")) }
    else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" { Some((ImageFormat::WebP, "webp", "image/webp")) }
    else { None }
}

#[tauri::command]
fn cleanup_temp() -> Result<(), String> {
    let root = clip_temp_root();
    if root.exists() { fs::remove_dir_all(root).map_err(|error| format!("清理临时文件失败: {error}"))?; }
    Ok(())
}

fn clip_temp_root() -> PathBuf { std::env::temp_dir().join("tiny-clip") }
fn is_clip_temp(path: &Path) -> bool { path.starts_with(clip_temp_root()) }

#[tauri::command]
async fn compress_batch(window: tauri::WebviewWindow, paths: Vec<String>, settings: Option<CompressSettings>) -> Result<BatchResult, String> {
    CANCEL.store(false, Ordering::SeqCst);
    let settings = settings.unwrap_or_default();
    if !(1..=100).contains(&settings.scale_percent) { return Err("缩放百分比必须在 1 到 100 之间".to_string()); }
    let requested_mode = normalize_mode(&settings.output_mode)?;
    let suffix = sanitize_suffix(&settings.suffix);
    let base = shared_parent_or_empty(&paths);
    let staging = temp_staging();
    fs::create_dir_all(&staging).map_err(|error| format!("创建 staging 目录失败: {error}"))?;
    let total = paths.len();
    let options = engine::Options { scale_percent: settings.scale_percent, target_width: settings.target_width, target_height: settings.target_height, keep_copyright: settings.keep_copyright, keep_location: settings.keep_location, keep_creation: settings.keep_creation };
    let (sender, receiver) = mpsc::channel();
    let worker_paths = paths.clone();
    let worker_staging = staging.clone();
    let worker = std::thread::spawn(move || compress_to_staging(worker_paths, worker_staging, options, sender));

    let mut compressed = Vec::with_capacity(total);
    for (index, outcome, stage) in receiver {
        let done = compressed.len() + 1;
        let _ = window.emit("progress", serde_json::json!({ "phase": "compress", "done": done, "total": total, "item": outcome.clone() }));
        compressed.push((index, outcome, stage));
    }
    worker.join().map_err(|_| "压缩工作线程异常退出".to_string())?;
    compressed.sort_by_key(|(index, _, _)| *index);

    if CANCEL.load(Ordering::SeqCst) {
        let files = cancelled_outcomes(compressed.into_iter().map(|(_, outcome, _)| outcome).collect());
        let _ = fs::remove_dir_all(&staging);
        return Ok(build_result(files, &base, "new_file", true));
    }

    let mut compressed = compressed;
    let mut outcomes: Vec<FileOutcome> = compressed.iter().map(|(_, outcome, _)| outcome.clone()).collect();
    let mut effective_modes = vec![requested_mode; total];
    for (index, _, _) in &compressed {
        let source = Path::new(&paths[*index]);
        let effective_mode = if requested_mode == "replace" && is_clip_temp(source) { "new_file" } else { requested_mode };
        effective_modes[*index] = effective_mode;
        outcomes[*index].effective_mode = Some(effective_mode.to_string());
    }
    let mut replacements = Vec::new();
    let mut new_files = Vec::new();
    for (index, _, stage) in compressed.drain(..) {
        let source = PathBuf::from(&paths[index]);
        let effective_mode = effective_modes[index];
        let mut outcome = outcomes[index].clone();

        if CANCEL.load(Ordering::SeqCst) {
            rollback_delivery(&replacements, &new_files);
            return finish_aborted(outcomes, &base, &staging, effective_modes.clone(), "已取消", true);
        }
        if outcome.error.is_some() { continue; }

        let destination = if effective_mode == "replace" { source.clone() } else { unique_output_path(&source, &suffix) };
        let delivery = if effective_mode == "replace" {
            replace_from_stage(&source, &stage, &mut replacements)
        } else {
            fs::copy(&stage, &destination).map(|_| ()).map_err(|error| error.to_string())
        };

        match delivery {
            Ok(()) => {
                if effective_mode == "new_file" { new_files.push(destination.clone()); }
                outcome.output_path = Some(destination.to_string_lossy().to_string());
            }
            Err(error) => {
                outcome.error = Some(format!("落地失败: {error}"));
                outcomes[index] = outcome.clone();
                let _ = window.emit("progress", serde_json::json!({ "phase": "deliver", "done": index + 1, "total": total, "item": outcome }));
                rollback_delivery(&replacements, &new_files);
                return finish_aborted(outcomes, &base, &staging, effective_modes.clone(), "批处理已回滚", false);
            }
        }
        outcomes[index] = outcome.clone();
        let _ = window.emit("progress", serde_json::json!({ "phase": "deliver", "done": index + 1, "total": total, "item": outcome }));
    }

    if CANCEL.load(Ordering::SeqCst) {
        rollback_delivery(&replacements, &new_files);
        return finish_aborted(outcomes, &base, &staging, effective_modes.clone(), "已取消", true);
    }
    remove_backups(&replacements);
    let _ = fs::remove_dir_all(&staging);
    Ok(build_result(outcomes, &base, batch_mode(&effective_modes), false))
}

fn compress_to_staging(paths: Vec<String>, staging: PathBuf, options: engine::Options, sender: mpsc::Sender<(usize, FileOutcome, PathBuf)>) {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build();
    match pool {
        Ok(pool) => pool.install(|| paths.par_iter().enumerate().for_each_with(sender, |sender, (index, path)| {
            let source = PathBuf::from(path);
            let name = source.file_name().unwrap_or_default().to_string_lossy().to_string();
            let stage = staging.join(format!("{index}_{name}"));
            let mut outcome = empty_outcome(path, &name);
            if CANCEL.load(Ordering::SeqCst) { outcome.error = Some("已取消".to_string()); }
            else {
                match engine::compress_file(&source, &stage, &options) {
                    Ok(result) => { outcome.orig = result.orig_size; outcome.new = result.new_size; outcome.kept = result.keep_original; outcome.width = Some(result.out_width); outcome.height = Some(result.out_height); outcome.metadata_stripped = result.metadata_stripped; }
                    Err(error) => outcome.error = Some(error),
                }
            }
            let _ = sender.send((index, outcome, stage));
        })),
        Err(error) => for (index, path) in paths.iter().enumerate() {
            let name = Path::new(path).file_name().unwrap_or_default().to_string_lossy().to_string();
            let mut outcome = empty_outcome(path, &name);
            outcome.error = Some(error.to_string());
            let _ = sender.send((index, outcome, staging.join(format!("{index}_{name}"))));
        },
    }
}

fn finish_aborted(files: Vec<FileOutcome>, base: &Path, staging: &Path, modes: Vec<&str>, message: &str, cancelled: bool) -> Result<BatchResult, String> {
    let files = mark_aborted_outcomes(files, message);
    let _ = fs::remove_dir_all(staging);
    Ok(build_result(files, base, batch_mode(&modes), cancelled))
}

fn mark_aborted_outcomes(mut files: Vec<FileOutcome>, message: &str) -> Vec<FileOutcome> {
    for outcome in &mut files {
        if outcome.error.is_none() {
            outcome.output_path = None;
            outcome.error = Some(message.to_string());
        }
    }
    files
}

fn cancelled_outcomes(files: Vec<FileOutcome>) -> Vec<FileOutcome> {
    mark_aborted_outcomes(files, "已取消")
}

fn empty_outcome(path: &str, name: &str) -> FileOutcome {
    FileOutcome { path: path.to_string(), name: name.to_string(), orig: 0, new: 0, kept: false, error: None, output_path: None, width: None, height: None, metadata_stripped: false, effective_mode: None }
}

fn normalize_mode(mode: &str) -> Result<&'static str, String> {
    match mode { "new_file" | "newFile" => Ok("new_file"), "replace" => Ok("replace"), "ask" => Err("输出模式 ask 必须由前端解析为 new_file 或 replace".to_string()), _ => Err(format!("不支持的输出模式: {mode}")) }
}

fn sanitize_suffix(suffix: &str) -> String {
    let cleaned: String = suffix.chars().filter(|character| !matches!(character, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    if cleaned.trim().is_empty() { default_suffix() } else { cleaned }
}

fn batch_mode(modes: &[&str]) -> String {
    match (modes.iter().any(|mode| *mode == "replace"), modes.iter().any(|mode| *mode == "new_file")) { (true, true) => "mixed".to_string(), (true, false) => "replace".to_string(), _ => "new_file".to_string() }
}

fn temp_staging() -> PathBuf { let sequence = TEMP_SEQ.fetch_add(1, Ordering::SeqCst); std::env::temp_dir().join(format!("tiny-staging-{}-{sequence}", std::process::id())) }

fn build_result(files: Vec<FileOutcome>, base: &Path, mode: impl Into<String>, cancelled: bool) -> BatchResult {
    let total_in = files.iter().map(|file| file.orig).sum();
    let total_out = files.iter().map(|file| if file.new > 0 { file.new } else { file.orig }).sum();
    BatchResult { files, total_in, total_out, out_root: base.to_string_lossy().to_string(), output_mode: mode.into(), cancelled }
}

fn unique_output_path(source: &Path, suffix: &str) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let extension = source.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    let suffix = sanitize_suffix(suffix);
    let first = source.with_file_name(format!("{stem}{suffix}{extension}"));
    if !first.exists() { return first; }
    let mut index = 1;
    loop {
        let candidate = source.with_file_name(format!("{stem}{suffix}_{index}{extension}"));
        if !candidate.exists() { return candidate; }
        index += 1;
    }
}

fn unique_path(directory: &Path, stem: &str, extension: &str) -> PathBuf {
    let first = directory.join(format!("{stem}.{extension}"));
    if !first.exists() { return first; }
    let mut index = 1;
    loop { let candidate = directory.join(format!("{stem}_{index}.{extension}")); if !candidate.exists() { return candidate; } index += 1; }
}

fn replace_from_stage(source: &Path, stage: &Path, replacements: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), String> {
    let backup_dir = source.parent().unwrap_or_else(|| Path::new(".")).join("tinybak");
    fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;
    let backup = unique_path(&backup_dir, &source.file_name().unwrap_or_default().to_string_lossy(), "bak");
    fs::rename(source, &backup).map_err(|error| format!("备份原图失败: {error}"))?;
    if let Err(error) = fs::copy(stage, source).and_then(|_| fs::remove_file(stage)) {
        let _ = fs::rename(&backup, source);
        return Err(error.to_string());
    }
    replacements.push((source.to_path_buf(), backup));
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct ReplacePair {
    pub path: String,
    pub output_path: String,
}

#[tauri::command]
fn replace_output_files(mapping: Vec<ReplacePair>) -> Result<Vec<FileOutcome>, String> {
    let mut outcomes = Vec::with_capacity(mapping.len());
    for pair in mapping {
        let source = PathBuf::from(&pair.path);
        let new_file = PathBuf::from(&pair.output_path);
        let name = source.file_name().unwrap_or_default().to_string_lossy().to_string();
        let mut outcome = empty_outcome(&pair.path, &name);
        if !new_file.exists() {
            outcome.error = Some("新文件不存在，无法替换原文件".to_string());
            outcomes.push(outcome);
            continue;
        }
        let backup_dir = source.parent().unwrap_or_else(|| Path::new(".")).join("tinybak");
        fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;
        let backup = unique_path(&backup_dir, &source.file_name().unwrap_or_default().to_string_lossy(), "bak");
        if let Err(error) = fs::rename(&source, &backup) {
            outcome.error = Some(format!("备份原图失败: {error}"));
            outcomes.push(outcome);
            continue;
        }
        match fs::copy(&new_file, &source) {
            Ok(_) => {
                let _ = fs::remove_file(&new_file);
                let _ = fs::remove_file(&backup);
                outcome.output_path = Some(pair.path.clone());
                outcome.kept = false;
            }
            Err(error) => {
                let _ = fs::rename(&backup, &source);
                outcome.error = Some(format!("替换原图失败: {error}"));
            }
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

fn rollback_delivery(replacements: &[(PathBuf, PathBuf)], new_files: &[PathBuf]) {
    for output in new_files { let _ = fs::remove_file(output); }
    for (source, backup) in replacements.iter().rev() { let _ = fs::remove_file(source); let _ = fs::rename(backup, source); }
}

fn remove_backups(replacements: &[(PathBuf, PathBuf)]) { for (_, backup) in replacements { let _ = fs::remove_file(backup); } }

fn shared_parent_or_empty(paths: &[String]) -> PathBuf {
    let mut parents = paths.iter().filter_map(|p| Path::new(p).parent().map(|x| x.to_path_buf()));
    let first = match parents.next() { Some(p) => p, None => return PathBuf::new() };
    if parents.all(|p| p == first) { first } else { PathBuf::new() }
}

#[tauri::command]
fn cancel_compress() { CANCEL.store(true, Ordering::SeqCst); }

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![compress_batch, file_sizes, image_infos, save_clipboard_image, cleanup_temp, cancel_compress, replace_output_files])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(|_, event| if matches!(event, tauri::RunEvent::Exit { .. }) { let _ = cleanup_temp(); });
}

#[cfg(test)]
mod e2e {
    use super::*;

    #[test]
    fn output_name_uses_min_suffix_and_never_overwrites() {
        let directory = std::env::temp_dir().join("tiny-name-test");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("产品图.jpg");
        fs::write(&source, b"x").unwrap();
        assert_eq!(unique_output_path(&source, "_min").file_name().unwrap(), "产品图_min.jpg");
        fs::write(directory.join("产品图_min.jpg"), b"x").unwrap();
        assert_eq!(unique_output_path(&source, "_min").file_name().unwrap(), "产品图_min_1.jpg");
        assert_eq!(sanitize_suffix("/:*?\"<>|"), "_tiny");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn magic_bytes_return_format_and_mime() {
        assert_eq!(magic_format(b"\x89PNG\r\n\x1a\n").unwrap().1, "png");
        assert_eq!(magic_format(b"\x89PNG\r\n\x1a\n").unwrap().2, "image/png");
        assert_eq!(magic_format(b"RIFF\0\0\0\0WEBPVP8 ").unwrap().1, "webp");
        assert!(magic_format(b"not-an-image").is_none());
    }

    #[test]
    fn image_info_reports_dimensions_thumbnail_and_format() {
        let path = std::env::temp_dir().join("tiny-info-test.png");
        image::RgbaImage::new(100, 50).save(&path).unwrap();
        let info = inspect_image(path.to_str().unwrap());
        assert_eq!((info.width, info.height), (Some(100), Some(50)));
        assert_eq!(info.format.as_deref(), Some("png"));
        assert_eq!(info.mime.as_deref(), Some("image/png"));
        assert!(info.thumbnail.unwrap().starts_with("data:image/png;base64,"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn thumbnail_limit_prevents_large_decode() {
        assert!(should_make_thumbnail(10_000, 8_000));
        assert!(!should_make_thumbnail(10_001, 8_000));
    }

    #[test]
    fn output_modes_and_clip_paths_are_resolved_safely() {
        assert_eq!(CompressSettings::default().suffix, "_tiny");
        assert_eq!(normalize_mode("replace").unwrap(), "replace");
        assert!(normalize_mode("ask").is_err());
        assert!(is_clip_temp(&clip_temp_root().join("clipboard_1.png")));
        assert_eq!(batch_mode(&["replace", "new_file"]), "mixed");
    }

    #[test]
    fn aborted_outcomes_keep_all_items_and_clear_outputs() {
        let mut success = empty_outcome("a.jpg", "a.jpg");
        success.output_path = Some("a_min.jpg".to_string());
        let mut pending = empty_outcome("b.jpg", "b.jpg");
        pending.output_path = Some("b_min.jpg".to_string());
        let mut failed = empty_outcome("c.jpg", "c.jpg");
        failed.error = Some("压缩失败".to_string());
        let cancelled = mark_aborted_outcomes(vec![success.clone(), pending, failed.clone()], "已取消");
        assert_eq!(cancelled.len(), 3);
        assert_eq!(cancelled[0].output_path, None);
        assert_eq!(cancelled[0].error.as_deref(), Some("已取消"));
        assert_eq!(cancelled[1].output_path, None);
        assert_eq!(cancelled[1].error.as_deref(), Some("已取消"));
        assert_eq!(cancelled[2].error.as_deref(), Some("压缩失败"));
        let rolled_back = mark_aborted_outcomes(vec![success, failed], "批处理已回滚");
        assert_eq!(rolled_back.len(), 2);
        assert_eq!(rolled_back[0].output_path, None);
        assert_eq!(rolled_back[0].error.as_deref(), Some("批处理已回滚"));
    }

    #[test]
    fn replace_rolls_back_original_image() {
        let directory = std::env::temp_dir().join("tiny-rollback-test");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("a.jpg");
        let stage = directory.join("stage.jpg");
        fs::write(&source, b"original").unwrap();
        fs::write(&stage, b"new").unwrap();
        let mut replacements = Vec::new();
        replace_from_stage(&source, &stage, &mut replacements).unwrap();
        rollback_delivery(&replacements, &[]);
        assert_eq!(fs::read(&source).unwrap(), b"original");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn replace_output_files_overwrites_source_and_cleans_up() {
        let directory = std::env::temp_dir().join("tiny-replace-commit");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("orig.jpg");
        let new_file = directory.join("orig_tiny.jpg");
        fs::write(&source, b"original-bytes").unwrap();
        fs::write(&new_file, b"compressed-bytes").unwrap();
        let mapping = vec![ReplacePair { path: source.to_str().unwrap().to_string(), output_path: new_file.to_str().unwrap().to_string() }];
        let committed = replace_output_files(mapping).unwrap();
        assert_eq!(committed.len(), 1);
        assert!(committed[0].error.is_none(), "应成功覆盖");
        assert_eq!(committed[0].output_path.as_deref(), Some(source.to_str().unwrap()));
        assert_eq!(fs::read(&source).unwrap(), b"compressed-bytes", "原图内容应被新文件替换");
        assert!(!new_file.exists(), "_tiny 文件应被删除");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn replace_output_files_rolls_back_when_new_missing() {
        let directory = std::env::temp_dir().join("tiny-replace-fail");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("orig.jpg");
        let new_file = directory.join("missing_tiny.jpg");
        fs::write(&source, b"original-bytes").unwrap();
        let mapping = vec![ReplacePair { path: source.to_str().unwrap().to_string(), output_path: new_file.to_str().unwrap().to_string() }];
        let committed = replace_output_files(mapping).unwrap();
        assert_eq!(committed.len(), 1);
        assert!(committed[0].error.is_some(), "新文件缺失应报错");
        assert_eq!(fs::read(&source).unwrap(), b"original-bytes", "原图应保持不变");
        let _ = fs::remove_dir_all(&directory);
    }
}
