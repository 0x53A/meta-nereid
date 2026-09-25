use serde::Deserialize;
use std::io::Read;
use std::path::Path;

const BASE_URL: &str = "https://appstore-api.rebble.io/api/v1";
const HARDWARE: &str = "chalk";
const PAGE_SIZE: u32 = 20;

#[derive(Debug, Clone, Deserialize)]
pub struct StoreResponse {
    pub data: Vec<StoreApp>,
    #[allow(dead_code)]
    pub limit: u32,
    #[allow(dead_code)]
    pub offset: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreApp {
    pub id: String,
    pub title: String,
    pub author: String,
    #[serde(rename = "type")]
    pub app_type: String,
    pub hearts: u32,
    pub latest_release: Option<LatestRelease>,
    #[serde(default)]
    pub hardware_platforms: Vec<HardwarePlatform>,
    #[serde(default)]
    pub screenshot_images: Vec<ScreenshotImage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LatestRelease {
    pub pbw_file: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HardwarePlatform {
    pub name: String,
    #[serde(default)]
    pub images: PlatformImages,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlatformImages {
    #[serde(default)]
    pub screenshot: String,
    #[serde(default)]
    pub list: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ScreenshotImage {
    Map(std::collections::HashMap<String, String>),
    Str(String),
}

impl StoreApp {
    /// Filename used when saving to disk
    pub fn local_filename(&self) -> String {
        format!("{}.pbw", self.id)
    }

    /// Check if this app is already downloaded
    pub fn is_installed(&self, pbw_dir: &Path) -> bool {
        pbw_dir.join(self.local_filename()).exists()
    }

    /// Get the best screenshot URL for chalk (180x180)
    pub fn chalk_screenshot_url(&self) -> Option<&str> {
        // Prefer chalk platform-specific screenshot
        for hp in &self.hardware_platforms {
            if hp.name == "chalk" && !hp.images.screenshot.is_empty() {
                return Some(&hp.images.screenshot);
            }
        }
        // Fall back to first screenshot_images entry
        for si in &self.screenshot_images {
            match si {
                ScreenshotImage::Map(map) => {
                    // Try any resolution
                    if let Some(url) = map.values().next() {
                        return Some(url);
                    }
                }
                ScreenshotImage::Str(url) => return Some(url),
            }
        }
        None
    }
}

/// Collection slugs supported by Rebble API
pub const COLLECTION_ALL: &str = "all";
pub const COLLECTION_MOST_LOVED: &str = "most-loved";

/// Fetch a collection of apps.
/// `slug` = "all", "most-loved", or "recently-updated"
/// `app_type` = "watchfaces" or "watchapps"
pub fn fetch_collection(
    slug: &str,
    app_type: &str,
    offset: u32,
) -> Result<StoreResponse, String> {
    let url = format!(
        "{}/apps/collection/{}/{}?hardware={}&limit={}&offset={}",
        BASE_URL, slug, app_type, HARDWARE, PAGE_SIZE, offset
    );
    eprintln!("Store: fetching {}", url);

    let resp = ureq::get(&url)
        .call()
        .map_err(|e| format!("HTTP error: {}", e))?;

    resp.into_json::<StoreResponse>()
        .map_err(|e| format!("JSON error: {}", e))
}

pub fn download_pbw(app: &StoreApp, pbw_dir: &Path) -> Result<String, String> {
    let release = app
        .latest_release
        .as_ref()
        .ok_or_else(|| "No release available".to_string())?;

    let dest = pbw_dir.join(app.local_filename());
    eprintln!(
        "Store: downloading {} -> {}",
        release.pbw_file,
        dest.display()
    );

    std::fs::create_dir_all(pbw_dir).map_err(|e| format!("mkdir error: {}", e))?;

    let resp = ureq::get(&release.pbw_file)
        .call()
        .map_err(|e| format!("Download error: {}", e))?;

    let mut reader = resp.into_reader();
    let mut file =
        std::fs::File::create(&dest).map_err(|e| format!("File create error: {}", e))?;
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("Write error: {}", e))?;

    eprintln!(
        "Store: downloaded {} ({} bytes)",
        app.title,
        std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0)
    );
    Ok(dest.to_string_lossy().to_string())
}

/// Download a screenshot PNG and return the bytes
pub fn download_screenshot(url: &str) -> Result<Vec<u8>, String> {
    eprintln!("Store: fetching screenshot {}", url);
    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("Screenshot HTTP error: {}", e))?;

    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("Screenshot read error: {}", e))?;
    Ok(buf)
}
