#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod scriptures;
mod scriptures_niv;

use scriptures::SCRIPTURE_DATA;
use scriptures_niv::NIV_SCRIPTURE_DATA;

use ab_glyph::{Font, FontVec, PxScale, PxScaleFont, ScaleFont};
use chrono::{Local, Timelike};
use image::Rgba;
use imageproc::drawing::draw_text_mut;
use rand::Rng;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

// =====================================================================
// Built-in Font Registry
// =====================================================================

struct FontInfo {
    id: &'static str,
    display_name: &'static str,
    filename: &'static str,
    url: &'static str,
}

const BUILTIN_FONTS: &[FontInfo] = &[
    FontInfo {
        id: "NotoSansSC",
        display_name: "思源黑体 Noto Sans SC",
        filename: "NotoSansSC-Regular.ttf",
        url: "https://fonts.gstatic.com/s/notosanssc/v40/k3kCo84MPvpLmixcA63oeAL7Iqp5IZJF9bmaG9_FnYw.ttf",
    },
    FontInfo {
        id: "NotoSerifSC",
        display_name: "思源宋体 Noto Serif SC",
        filename: "NotoSerifSC-Regular.ttf",
        url: "https://fonts.gstatic.com/s/notoserifsc/v35/H4cyBXePl9DZ0Xe7gG9cyOj7uK2-n-D2rd4FY7SCqyWv.ttf",
    },
    FontInfo {
        id: "LXGWWenKai",
        display_name: "霞鹜文楷 LXGW WenKai",
        filename: "LXGWWenKai-Regular.ttf",
        url: "https://github.com/lxgw/LxgwWenKai/releases/download/v1.522/LXGWWenKai-Regular.ttf",
    },
];

/// Ensure a built-in font is available locally, downloading it if necessary.
/// Returns the absolute path to the font file on success.
async fn ensure_font(app: &AppHandle, font_name: &str) -> Result<PathBuf, String> {
    let font_info = BUILTIN_FONTS
        .iter()
        .find(|f| f.id == font_name)
        .ok_or_else(|| {
            format!(
                "未知字体: {}，可选字体: {}",
                font_name,
                BUILTIN_FONTS.iter().map(|f| f.id).collect::<Vec<_>>().join(", ")
            )
        })?;

    let font_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取应用数据目录: {}", e))?
        .join("fonts");

    fs::create_dir_all(&font_dir).map_err(|e| format!("创建字体目录失败: {}", e))?;

    let font_path = font_dir.join(font_info.filename);

    if font_path.exists() {
        return Ok(font_path);
    }

    // Font not found locally — download it
    let url_owned = font_info.url.to_string();
    let display_name = font_info.display_name.to_string();
    let bytes = tokio::task::spawn_blocking(move || {
        let response = reqwest::blocking::get(&url_owned)
            .map_err(|e| format!("字体下载请求失败: {}", e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("字体下载失败 (HTTP {})", status));
        }
        response
            .bytes()
            .map_err(|e| format!("读取字体数据失败: {}", e))
    })
    .await
    .map_err(|e| format!("下载任务执行失败: {}", e))??;

    // Write to a temporary file first, then rename — avoids partial writes
    let temp_path = font_path.with_extension("tmp");
    let mut file = File::create(&temp_path)
        .map_err(|e| format!("创建临时字体文件失败: {}", e))?;
    file.write_all(&bytes)
        .map_err(|e| format!("写入字体文件失败: {}", e))?;
    fs::rename(&temp_path, &font_path)
        .map_err(|e| format!("重命名字体文件失败: {}", e))?;

    eprintln!("[字体] {} 下载完成 ({} KB)", display_name, bytes.len() / 1024);
    Ok(font_path)
}

// =====================================================================
// Path Resolution Helper
// =====================================================================

/// Resolve a path: if relative, prepend app_data_dir; if absolute, use as-is.
fn resolve_path(app: &AppHandle, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取应用数据目录: {}", e))?;
    fs::create_dir_all(&data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;
    Ok(data_dir.join(relative))
}

// =====================================================================
// Data & Image Processing Module
// =====================================================================

/// Initialize local scripture database
#[tauri::command]
async fn init_database(app: AppHandle, db_path: String) -> Result<String, String> {
    let path = resolve_path(&app, &db_path)?;
    let path_str = path.to_string_lossy().to_string();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }

    let conn = Connection::open(&path).map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS scriptures (
            id INTEGER PRIMARY KEY,
            content TEXT NOT NULL UNIQUE,
            reference TEXT NOT NULL,
            version TEXT DEFAULT '和合本'
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // Migration: add UNIQUE constraint if missing (existing databases)
    let has_unique: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_index_list('scriptures') WHERE unique = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if !has_unique {
        // Recreate table with UNIQUE constraint, preserving existing data
        conn.execute_batch(
            "CREATE TABLE scriptures_new (
                id INTEGER PRIMARY KEY,
                content TEXT NOT NULL UNIQUE,
                reference TEXT NOT NULL,
                version TEXT DEFAULT '和合本'
            );
            INSERT OR IGNORE INTO scriptures_new SELECT * FROM scriptures;
            DROP TABLE scriptures;
            ALTER TABLE scriptures_new RENAME TO scriptures;",
        )
        .map_err(|e| format!("数据库迁移失败: {}", e))?;
    }

    conn.execute(
        "CREATE TABLE IF NOT EXISTS favorites (
            id INTEGER PRIMARY KEY,
            content TEXT NOT NULL,
            reference TEXT NOT NULL,
            image_path TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // Seed Chinese Union Version (和合本) verses
    let cuv_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM scriptures WHERE version = '和合本'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if (cuv_count as usize) < SCRIPTURE_DATA.len() {
        for (content, reference) in SCRIPTURE_DATA.iter() {
            conn.execute(
                "INSERT OR IGNORE INTO scriptures (content, reference, version) VALUES (?1, ?2, '和合本')",
                [*content, *reference],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    // Seed NIV verses
    let niv_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM scriptures WHERE version = 'NIV'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if (niv_count as usize) < NIV_SCRIPTURE_DATA.len() {
        for (content, reference) in NIV_SCRIPTURE_DATA.iter() {
            conn.execute(
                "INSERT OR IGNORE INTO scriptures (content, reference, version) VALUES (?1, ?2, 'NIV')",
                [*content, *reference],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    Ok(format!("数据库初始化成功 ({})", path_str))
}

/// Get a random scripture from the database, filtered by version
#[tauri::command]
async fn get_random_scripture(
    app: AppHandle,
    db_path: String,
    version: String,
) -> Result<(String, String), String> {
    let path = resolve_path(&app, &db_path)?;
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM scriptures WHERE version = ?1",
            [&version],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if count == 0 {
        return Err(format!("数据库中没有 {} 版本的经文", version));
    }

    let mut rng = rand::thread_rng();
    let offset = rng.gen_range(0..count);

    let result = conn
        .query_row(
            "SELECT content, reference FROM scriptures WHERE version = ?1 LIMIT 1 OFFSET ?2",
            [&version, &offset.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(result)
}

/// Download an image from URL to local path
#[tauri::command]
async fn download_image(app: AppHandle, url: String, save_path: String) -> Result<String, String> {
    let path = resolve_path(&app, &save_path)?;
    let path_str = path.to_string_lossy().to_string();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }

    let url_clone = url.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        let response =
            reqwest::blocking::get(&url_clone).map_err(|e| format!("网络请求失败: {}", e))?;
        response
            .bytes()
            .map_err(|e| format!("读取图片数据失败: {}", e))
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))??;

    let mut file = File::create(&path).map_err(|e| format!("创建图片文件失败: {}", e))?;
    file.write_all(&bytes)
        .map_err(|e| format!("保存图片失败: {}", e))?;

    Ok(format!("图片下载成功至: {}", path_str))
}

/// Fetch Bing wallpaper — randomly picks from the last 7 days
#[tauri::command]
async fn fetch_bing_daily(app: AppHandle, save_path: String) -> Result<String, String> {
    let path = resolve_path(&app, &save_path)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }

    let path_clone = path.clone();
    tokio::task::spawn_blocking(move || {
        // Request 30 days of wallpapers, then pick one randomly
        let api_url = "https://www.bing.com/HPImageArchive.aspx?format=js&idx=0&n=30&mkt=zh-CN";

        let resp =
            reqwest::blocking::get(api_url).map_err(|e| format!("Bing API 请求失败: {}", e))?;
        let json: serde_json::Value = resp.json().map_err(|e| format!("解析 JSON 失败: {}", e))?;

        let images = json["images"]
            .as_array()
            .ok_or("无法从 Bing 数据中提取图片列表")?;

        if images.is_empty() {
            return Err("Bing 未返回任何壁纸".to_string());
        }

        let mut rng = rand::thread_rng();
        let idx = rng.gen_range(0..images.len());
        let img_url_partial = images[idx]["url"]
            .as_str()
            .ok_or("无法从 Bing 数据中提取 URL")?;
        let full_img_url = format!("https://www.bing.com{}", img_url_partial);

        let img_resp =
            reqwest::blocking::get(&full_img_url).map_err(|e| format!("下载图片失败: {}", e))?;
        let bytes = img_resp
            .bytes()
            .map_err(|e| format!("读取图片流失败: {}", e))?;
        let mut file = File::create(&path_clone).map_err(|e| format!("创建图片文件失败: {}", e))?;
        file.write_all(&bytes)
            .map_err(|e| format!("保存壁纸失败: {}", e))?;

        Ok(())
    })
    .await
    .map_err(|e| format!("任务执行失败: {}", e))??;

    let path_str = path.to_string_lossy().to_string();
    Ok(format!("Bing 壁纸已成功拉取至: {}", path_str))
}

/// Get a random image from a local folder
#[tauri::command]
async fn get_random_local_image(folder_path: String) -> Result<String, String> {
    let entries = fs::read_dir(&folder_path).map_err(|e| format!("无法读取文件夹: {}", e))?;
    let mut images = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let ext = ext.to_lowercase();
                if ext == "jpg" || ext == "jpeg" || ext == "png" {
                    images.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    if images.is_empty() {
        return Err("该文件夹下未找到 jpg 或 png 格式的图片".to_string());
    }

    let mut rng = rand::thread_rng();
    let selected = images[rng.gen_range(0..images.len())].clone();

    Ok(selected)
}

/// Return the list of available built-in fonts for the frontend dropdown.
#[tauri::command]
async fn get_font_list() -> Result<Vec<serde_json::Value>, String> {
    let list: Vec<serde_json::Value> = BUILTIN_FONTS
        .iter()
        .map(|f| {
            serde_json::json!({
                "id": f.id,
                "display_name": f.display_name,
            })
        })
        .collect();
    Ok(list)
}

/// Ensure a font is downloaded and available. Returns the local path.
#[tauri::command]
async fn ensure_font_downloaded(app: AppHandle, font_name: String) -> Result<String, String> {
    let path = ensure_font(&app, &font_name).await?;
    Ok(path.to_string_lossy().to_string())
}

// =====================================================================
// Image Compositing & Wallpaper Engine
// =====================================================================

/// Wrap text into lines that fit within `max_width` pixels.
/// Uses character-level wrapping: breaks between any two CJK characters,
/// and at space boundaries for Latin text.
fn wrap_text(text: &str, max_width: f32, scaled_font: &PxScaleFont<&FontVec>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut current_width: f32 = 0.0;

    for ch in text.chars() {
        let glyph_id = scaled_font.glyph_id(ch);
        let char_width = scaled_font.h_advance(glyph_id)
            + current_line
                .chars()
                .last()
                .map(|prev| scaled_font.kern(scaled_font.glyph_id(prev), glyph_id))
                .unwrap_or(0.0);

        // Break if adding this char would exceed max_width
        if !current_line.is_empty() && current_width + char_width > max_width {
            // Skip space at wrap point instead of starting new line with it
            if ch != ' ' {
                lines.push(current_line.clone());
                current_line.clear();
                current_line.push(ch);
                current_width = char_width;
            } else {
                lines.push(current_line.clone());
                current_line.clear();
                current_width = 0.0;
            }
            continue;
        }

        current_line.push(ch);
        current_width += char_width;
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        lines.push(text.to_string());
    }

    lines
}

/// Smart image-text compositing engine with brightness analysis.
/// Returns the absolute output path so the frontend can display a preview.
#[tauri::command]
async fn generate_wallpaper(
    app: AppHandle,
    input_path: String,
    output_path: String,
    text: String,
    font_name: String,
    font_size: f32,
) -> Result<String, String> {
    let inp = resolve_path(&app, &input_path)?;
    let out = resolve_path(&app, &output_path)?;

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }

    // Resolve font — download if needed
    let font_path = ensure_font(&app, &font_name).await?;

    let mut img = image::open(&inp)
        .map_err(|e| format!("无法打开图片: {}", e))?
        .to_rgba8();

    let font_data =
        fs::read(&font_path).map_err(|e| format!("无法读取字体文件: {}", e))?;
    let font = FontVec::try_from_vec(font_data).map_err(|e| format!("加载字体失败: {}", e))?;

    let scale = PxScale::from(font_size);
    let image_height = img.height();
    let image_width = img.width();

    // Calculate average brightness in the bottom 1/3
    let y_start = image_height - (image_height / 3);
    let mut total_luma: u64 = 0;
    let mut pixel_count: u64 = 0;

    for y in y_start..image_height {
        for x in 0..image_width {
            let pixel = img.get_pixel(x, y);
            let luma = (0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32)
                as u64;
            total_luma += luma;
            pixel_count += 1;
        }
    }
    let avg_luma = if pixel_count > 0 {
        total_luma / pixel_count
    } else {
        128
    };

    let (text_color, shadow_color) = if avg_luma > 140 {
        (Rgba([30, 30, 30, 255]), Rgba([255, 255, 255, 180]))
    } else {
        (Rgba([245, 245, 245, 255]), Rgba([0, 0, 0, 180]))
    };

    // Word-wrap: compute lines that fit within image width with margins
    let scaled_font = font.as_scaled(scale);
    let margin_x: f32 = 100.0;
    let max_text_width = image_width as f32 - margin_x * 2.0;
    let lines = wrap_text(&text, max_text_width, &scaled_font);

    // Calculate line height and total text block height
    let line_height = scaled_font.height() + scaled_font.line_gap();
    let total_block_height = line_height * lines.len() as f32;

    // Vertically center the text block in the bottom 1/3 region
    let bottom_third_top = y_start as f32;
    let bottom_third_height = (image_height - y_start) as f32;
    let block_start_y = bottom_third_top + (bottom_third_height - total_block_height) / 2.0;

    // Draw each line with shadow
    let x_position = margin_x as i32;
    for (i, line) in lines.iter().enumerate() {
        let y = (block_start_y + line_height * i as f32) as i32;

        // Shadow (offset +2px)
        draw_text_mut(
            &mut img,
            shadow_color,
            x_position + 2,
            y + 2,
            scale,
            &font,
            line,
        );
        // Main text
        draw_text_mut(&mut img, text_color, x_position, y, scale, &font, line);
    }

    // Convert RGBA to RGB for JPEG compatibility
    let rgb_img = image::DynamicImage::ImageRgba8(img).to_rgb8();

    // Save the composited image
    rgb_img.save(&out).map_err(|e| format!("保存失败: {}", e))?;

    // Export TSV metadata file alongside the image
    let out_str = out.to_string_lossy().to_string();
    let tsv_path = out_str
        .replace(".jpg", "_metadata.tsv")
        .replace(".png", "_metadata.tsv");
    let mut tsv_file = File::create(&tsv_path).map_err(|e| format!("创建TSV文件失败: {}", e))?;
    let date_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let tsv_content = format!(
        "Date\tScripture_Content\tSource_Image\n{}\t{}\t{}\n",
        date_str,
        text,
        inp.to_string_lossy()
    );
    tsv_file
        .write_all(tsv_content.as_bytes())
        .map_err(|e| format!("写入TSV失败: {}", e))?;

    // Return absolute output path for frontend preview
    Ok(out_str)
}

/// Set system desktop wallpaper (platform-specific implementation)
#[tauri::command]
async fn set_system_wallpaper(app: AppHandle, wallpaper_path: String) -> Result<String, String> {
    let path = resolve_path(&app, &wallpaper_path)?;
    let path_str = path.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        wallpaper::set_from_path(&path_str).map_err(|e| format!("设置 Windows 壁纸失败: {}", e))?;
        let _ = wallpaper::set_mode(wallpaper::Mode::Crop);
        return Ok("Windows 系统壁纸已成功更新".to_string());
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let script = format!(
            "tell application \"System Events\" to set picture of every desktop to POSIX file \"{}\"",
            path_str
        );
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .status()
            .map_err(|e| format!("执行 AppleScript 失败: {}", e))?;
        if status.success() {
            Ok("Mac 系统壁纸已成功更新".to_string())
        } else {
            Err("AppleScript 权限被拒绝".to_string())
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = path_str;
        return Err("暂不支持该操作系统的壁纸自动设置".to_string());
    }
}

// =====================================================================
// Config Persistence & Data Export
// =====================================================================

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppConfig {
    pub font_size: f32,
    pub update_time: String,
    pub font_name: String,
    pub wallpaper_mode: String,
    pub local_folder: String,
    pub img_api_url: String,
    #[serde(default = "default_scripture_version")]
    pub scripture_version: String,
}

fn default_scripture_version() -> String {
    "和合本".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font_size: 55.0,
            update_time: "06:00".to_string(),
            font_name: "NotoSansSC".to_string(),
            wallpaper_mode: "bing".to_string(),
            local_folder: String::new(),
            img_api_url: "https://picsum.photos/1920/1080".to_string(),
            scripture_version: default_scripture_version(),
        }
    }
}

#[tauri::command]
async fn load_config(app: AppHandle, config_path: String) -> Result<AppConfig, String> {
    let path = resolve_path(&app, &config_path)?;

    if path.exists() {
        let data = fs::read_to_string(&path).map_err(|e| format!("读取配置失败: {}", e))?;

        // Migration: convert old font_path to font_name
        let data = if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&data) {
            if val.get("font_name").is_none() && val.get("font_path").is_some() {
                val.as_object_mut().unwrap().insert(
                    "font_name".to_string(),
                    serde_json::Value::String("NotoSansSC".to_string()),
                );
                val.as_object_mut().unwrap().remove("font_path");
                serde_json::to_string_pretty(&val).unwrap_or(data)
            } else {
                data
            }
        } else {
            data
        };

        let mut config: AppConfig =
            serde_json::from_str(&data).unwrap_or_else(|_| AppConfig::default());
        // Fix legacy grayscale URL
        if config.img_api_url.contains("grayscale") {
            config.img_api_url = "https://picsum.photos/1920/1080".to_string();
        }
        // Validate font_name — reset to default if unknown
        if !BUILTIN_FONTS.iter().any(|f| f.id == config.font_name) {
            config.font_name = "NotoSansSC".to_string();
        }
        Ok(config)
    } else {
        let default_config = AppConfig::default();
        let _ = save_config_inner(&path, &default_config);
        Ok(default_config)
    }
}

#[tauri::command]
async fn save_config(
    app: AppHandle,
    config_path: String,
    config: AppConfig,
) -> Result<String, String> {
    let path = resolve_path(&app, &config_path)?;
    save_config_inner(&path, &config)
}

fn save_config_inner(path: &Path, config: &AppConfig) -> Result<String, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    let json_data =
        serde_json::to_string_pretty(config).map_err(|e| format!("序列化失败: {}", e))?;
    fs::write(path, json_data).map_err(|e| format!("写入配置文件失败: {}", e))?;
    Ok("配置已成功保存".to_string())
}

/// Add a wallpaper to favorites
#[tauri::command]
async fn add_favorite(
    app: AppHandle,
    db_path: String,
    content: String,
    reference: String,
    image_path: String,
) -> Result<i64, String> {
    let path = resolve_path(&app, &db_path)?;
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;

    let created_at = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO favorites (content, reference, image_path, created_at) VALUES (?1, ?2, ?3, ?4)",
        [&content, &reference, &image_path, &created_at],
    )
    .map_err(|e| format!("收藏失败: {}", e))?;

    Ok(conn.last_insert_rowid())
}

/// List all favorites
#[tauri::command]
async fn list_favorites(
    app: AppHandle,
    db_path: String,
) -> Result<Vec<(i64, String, String, String, String)>, String> {
    let path = resolve_path(&app, &db_path)?;
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, content, reference, image_path, created_at FROM favorites ORDER BY id DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Remove a favorite by id
#[tauri::command]
async fn remove_favorite(
    app: AppHandle,
    db_path: String,
    favorite_id: i64,
) -> Result<String, String> {
    let path = resolve_path(&app, &db_path)?;
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;

    conn.execute("DELETE FROM favorites WHERE id = ?1", [favorite_id])
        .map_err(|e| format!("删除收藏失败: {}", e))?;

    Ok("已删除收藏".to_string())
}

// =====================================================================
// Application Entry Point
// =====================================================================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // Hide window on startup when launched with --hidden (e.g. autostart)
            if std::env::args().any(|arg| arg == "--hidden") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            // Build system tray menu
            let update_now =
                MenuItemBuilder::with_id("update_now", "立即刷新今日经文").build(app)?;
            let show_window = MenuItemBuilder::with_id("show", "打开控制面板").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "退出后台程序").build(app)?;

            let menu = MenuBuilder::new(app)
                .item(&update_now)
                .separator()
                .item(&show_window)
                .separator()
                .item(&quit)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap())
                .menu(&menu)
                .tooltip("经文壁纸 Scripture Wallpaper")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "update_now" => {
                        let _ = app.emit("trigger-update", ());
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button,
                        button_state,
                        ..
                    } = event
                    {
                        // Only respond to left button press (not release)
                        if button == tauri::tray::MouseButton::Left
                            && button_state == tauri::tray::MouseButtonState::Down
                        {
                            let app = tray.app_handle();
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Register global shortcut: Ctrl+Shift+W (Cmd+Shift+W on Mac)
            let shortcut_handle = app.handle().clone();
            let global_shortcut = app.global_shortcut();
            let _ = global_shortcut.on_shortcut(
                "CommandOrControl+Shift+W",
                move |_app, _shortcut, _event| {
                    let _ = shortcut_handle.emit("trigger-update", ());
                },
            );

            // Background cron scheduler thread
            let cron_handle = app.handle().clone();
            thread::spawn(move || loop {
                let now = Local::now();
                if now.hour() == 6 && now.minute() == 0 {
                    let _ = cron_handle.emit("trigger-update", ());
                    thread::sleep(Duration::from_secs(61));
                } else {
                    thread::sleep(Duration::from_secs(30));
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            init_database,
            get_random_scripture,
            download_image,
            fetch_bing_daily,
            get_random_local_image,
            generate_wallpaper,
            set_system_wallpaper,
            load_config,
            save_config,
            add_favorite,
            list_favorites,
            remove_favorite,
            get_font_list,
            ensure_font_downloaded,
        ])
        .run(tauri::generate_context!())
        .expect("运行 Tauri 应用时发生错误");
}

fn main() {
    run();
}
