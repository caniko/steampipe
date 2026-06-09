use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::core::config::{Scene, Tolerance, VisualConfig};

const DEFAULT_SSIM_THRESHOLD: f64 = 0.985;
const DEFAULT_MAX_PIXEL_DELTA: u8 = 255;

#[derive(Debug, Clone)]
pub struct Engine {
    visual: VisualConfig,
}

impl Engine {
    pub fn new(visual: VisualConfig) -> Self {
        Self { visual }
    }

    #[allow(dead_code)]
    pub fn compare(
        &self,
        actual_path: &Path,
        scene: &Scene,
    ) -> Result<ComparisonResult, VisualError> {
        let golden_path =
            self.visual
                .golden_path(&scene.name)
                .map_err(|source| VisualError::Config {
                    message: source.to_string(),
                })?;
        self.compare_with_golden(actual_path, &golden_path, scene)
    }

    pub fn compare_with_golden(
        &self,
        actual_path: &Path,
        golden_path: &Path,
        scene: &Scene,
    ) -> Result<ComparisonResult, VisualError> {
        let actual_original = load_rgba(actual_path).map_err(|source| VisualError::ImageRead {
            path: actual_path.to_path_buf(),
            source,
        })?;
        let golden_original = load_rgba(golden_path).map_err(|source| VisualError::ImageRead {
            path: golden_path.to_path_buf(),
            source,
        })?;

        let expected = expected_resolution(scene, &golden_original)?;
        validate_resolution("actual", actual_path, &actual_original, expected)?;
        validate_resolution("golden", golden_path, &golden_original, expected)?;

        let mut actual = if let Some(roi) = &scene.roi {
            crate::game::visual::roi::crop(&actual_original, roi).map_err(|source| {
                VisualError::Config {
                    message: source.to_string(),
                }
            })?
        } else {
            actual_original
        };
        let mut golden = if let Some(roi) = &scene.roi {
            crate::game::visual::roi::crop(&golden_original, roi).map_err(|source| {
                VisualError::Config {
                    message: source.to_string(),
                }
            })?
        } else {
            golden_original
        };

        if actual.dimensions() != golden.dimensions() {
            return Err(VisualError::ResolutionMismatch {
                path: actual_path.to_path_buf(),
                expected: golden.dimensions(),
                actual: actual.dimensions(),
            });
        }

        let mask_stats =
            crate::game::visual::mask::apply_mask(&mut actual, &scene.mask).map_err(|source| {
                VisualError::Config {
                    message: source.to_string(),
                }
            })?;
        crate::game::visual::mask::apply_mask(&mut golden, &scene.mask).map_err(|source| {
            VisualError::Config {
                message: source.to_string(),
            }
        })?;

        let ssim = crate::game::visual::ssim::mssim(&actual, &golden)
            .map_err(|source| VisualError::Compare { source })?;
        let max_delta = max_pixel_delta(&actual, &golden);
        let tolerance = effective_tolerance(self.visual.default_tolerance.as_ref(), scene);
        let passed = ssim >= tolerance.ssim && max_delta <= tolerance.max_pixel_delta;
        let diff_image = render_diff(&actual, &golden, &scene.mask);

        Ok(ComparisonResult {
            scene: scene.name.clone(),
            passed,
            ssim,
            max_delta,
            threshold: tolerance.ssim,
            max_pixel_delta_threshold: tolerance.max_pixel_delta,
            mask_area_percent: mask_stats.mask_area_percent,
            masked_pixels: mask_stats.masked_pixels,
            total_pixels: mask_stats.total_pixels,
            diff_image,
        })
    }
}

#[derive(Debug)]
pub enum VisualError {
    Config {
        message: String,
    },
    ImageRead {
        path: PathBuf,
        source: image::ImageError,
    },
    ResolutionMismatch {
        path: PathBuf,
        expected: (u32, u32),
        actual: (u32, u32),
    },
    Compare {
        source: anyhow::Error,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    ImageWrite {
        path: PathBuf,
        source: image::ImageError,
    },
}

impl std::fmt::Display for VisualError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config { message } => write!(f, "{message}"),
            Self::ImageRead { path, source } => {
                write!(f, "read image {}: {source}", path.display())
            }
            Self::ResolutionMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "ResolutionMismatch: {} is {}x{}, expected {}x{}",
                path.display(),
                actual.0,
                actual.1,
                expected.0,
                expected.1
            ),
            Self::Compare { source } => write!(f, "{source}"),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::ImageWrite { path, source } => {
                write!(f, "write image {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for VisualError {}

#[derive(Debug, Clone)]
pub struct ComparisonResult {
    pub scene: String,
    pub passed: bool,
    pub ssim: f64,
    pub max_delta: u8,
    pub threshold: f64,
    pub max_pixel_delta_threshold: u8,
    pub mask_area_percent: f64,
    pub masked_pixels: usize,
    pub total_pixels: usize,
    pub diff_image: RgbaImage,
}

impl ComparisonResult {
    pub fn sidecar(&self) -> ComparisonSidecar {
        ComparisonSidecar {
            scene: self.scene.clone(),
            passed: self.passed,
            ssim: self.ssim,
            max_delta: self.max_delta,
            threshold: self.threshold,
            max_pixel_delta_threshold: self.max_pixel_delta_threshold,
            mask_area_percent: self.mask_area_percent,
            masked_pixels: self.masked_pixels,
            total_pixels: self.total_pixels,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonSidecar {
    pub scene: String,
    pub passed: bool,
    pub ssim: f64,
    pub max_delta: u8,
    pub threshold: f64,
    pub max_pixel_delta_threshold: u8,
    pub mask_area_percent: f64,
    pub masked_pixels: usize,
    pub total_pixels: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisualRunResult {
    #[serde(default)]
    pub vm: String,
    pub scene: String,
    #[serde(default)]
    pub run_label: String,
    pub passed: bool,
    pub ssim: f64,
    pub max_delta: u8,
    pub threshold: f64,
    #[serde(default)]
    pub actual_path: PathBuf,
    #[serde(default)]
    pub diff_path: PathBuf,
    #[serde(default)]
    pub golden_path: PathBuf,
    pub result_path: PathBuf,
}

pub fn write_result_artifacts(
    visual: &VisualConfig,
    scene: &Scene,
    run_label: &str,
    actual_path: &Path,
    result: &ComparisonResult,
) -> Result<VisualRunResult, VisualError> {
    let paths = visual
        .diff_path(&scene.name, run_label)
        .map_err(|source| VisualError::Config {
            message: source.to_string(),
        })?;
    let dir = paths.actual.parent().ok_or_else(|| VisualError::Config {
        message: format!("diff path has no parent: {}", paths.actual.display()),
    })?;
    std::fs::create_dir_all(dir).map_err(|source| VisualError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    std::fs::copy(actual_path, &paths.actual).map_err(|source| VisualError::Io {
        path: paths.actual.clone(),
        source,
    })?;
    let golden_path = visual
        .golden_path(&scene.name)
        .map_err(|source| VisualError::Config {
            message: source.to_string(),
        })?;
    std::fs::copy(&golden_path, &paths.golden).map_err(|source| VisualError::Io {
        path: paths.golden.clone(),
        source,
    })?;
    result
        .diff_image
        .save(&paths.diff)
        .map_err(|source| VisualError::ImageWrite {
            path: paths.diff.clone(),
            source,
        })?;
    let result_path = dir.join(format!("{}.result.json", scene.name));
    let json =
        serde_json::to_vec_pretty(&result.sidecar()).map_err(|source| VisualError::Config {
            message: source.to_string(),
        })?;
    std::fs::write(&result_path, json).map_err(|source| VisualError::Io {
        path: result_path.clone(),
        source,
    })?;
    Ok(VisualRunResult {
        vm: actual_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string(),
        scene: scene.name.clone(),
        run_label: run_label.to_string(),
        passed: result.passed,
        ssim: result.ssim,
        max_delta: result.max_delta,
        threshold: result.threshold,
        actual_path: paths.actual,
        diff_path: paths.diff,
        golden_path,
        result_path,
    })
}

fn load_rgba(path: &Path) -> Result<RgbaImage, image::ImageError> {
    image::open(path).map(|image| image.to_rgba8())
}

fn expected_resolution(scene: &Scene, golden: &RgbaImage) -> Result<(u32, u32), VisualError> {
    let width = scene.resolution.as_ref().and_then(|r| r.width);
    let height = scene.resolution.as_ref().and_then(|r| r.height);
    match (width, height) {
        (Some(width), Some(height)) => Ok((width, height)),
        (None, None) => Ok(golden.dimensions()),
        _ => Err(VisualError::Config {
            message: format!(
                "visual scene '{}' resolution must set both width and height or neither",
                scene.name
            ),
        }),
    }
}

fn validate_resolution(
    label: &str,
    path: &Path,
    image: &RgbaImage,
    expected: (u32, u32),
) -> Result<(), VisualError> {
    let actual = image.dimensions();
    if actual != expected {
        Err(VisualError::ResolutionMismatch {
            path: path.to_path_buf(),
            expected,
            actual,
        })
    } else if actual.0 == 0 || actual.1 == 0 {
        Err(VisualError::Config {
            message: format!("{label} image has zero dimensions"),
        })
    } else {
        Ok(())
    }
}

fn effective_tolerance(default: Option<&Tolerance>, scene: &Scene) -> EffectiveTolerance {
    let scene_tolerance = scene.tolerance.as_ref();
    EffectiveTolerance {
        ssim: scene_tolerance
            .and_then(|t| t.ssim)
            .or_else(|| default.and_then(|t| t.ssim))
            .unwrap_or(DEFAULT_SSIM_THRESHOLD),
        max_pixel_delta: scene_tolerance
            .and_then(|t| t.max_pixel_delta)
            .or_else(|| default.and_then(|t| t.max_pixel_delta))
            .unwrap_or(DEFAULT_MAX_PIXEL_DELTA),
    }
}

struct EffectiveTolerance {
    ssim: f64,
    max_pixel_delta: u8,
}

fn max_pixel_delta(actual: &RgbaImage, golden: &RgbaImage) -> u8 {
    actual
        .pixels()
        .zip(golden.pixels())
        .flat_map(|(a, g)| {
            [
                a[0].abs_diff(g[0]),
                a[1].abs_diff(g[1]),
                a[2].abs_diff(g[2]),
                a[3].abs_diff(g[3]),
            ]
        })
        .max()
        .unwrap_or(0)
}

fn render_diff(
    actual: &RgbaImage,
    golden: &RgbaImage,
    mask: &[crate::core::config::MaskRect],
) -> RgbaImage {
    let mut diff = RgbaImage::new(actual.width(), actual.height());
    for y in 0..actual.height() {
        for x in 0..actual.width() {
            if crate::game::visual::mask::is_masked(x, y, mask) {
                let stripe = (x + y) % 8 < 4;
                let color = if stripe {
                    Rgba([0, 0, 0, 0])
                } else {
                    Rgba([255, 255, 255, 70])
                };
                diff.put_pixel(x, y, color);
                continue;
            }
            let a = actual.get_pixel(x, y);
            let g = golden.get_pixel(x, y);
            let delta =
                a.0.iter()
                    .zip(g.0.iter())
                    .map(|(a, g)| a.abs_diff(*g))
                    .max()
                    .unwrap_or(0);
            if delta == 0 {
                let grey = ((u16::from(a[0]) + u16::from(a[1]) + u16::from(a[2])) / 3) as u8;
                diff.put_pixel(x, y, Rgba([grey, grey, grey, 77]));
            } else {
                diff.put_pixel(x, y, delta_ramp(delta));
            }
        }
    }
    diff
}

fn delta_ramp(delta: u8) -> Rgba<u8> {
    let t = f32::from(delta) / 255.0;
    let red = (255.0 * t).round() as u8;
    let blue = (255.0 * (1.0 - t)).round() as u8;
    Rgba([red, 48, blue, 255])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{MaskRect, Resolution, Roi, VisualConfig};

    fn image(width: u32, height: u32, pixel: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba(pixel))
    }

    fn write_png(path: &Path, image: &RgbaImage) {
        image.save(path).unwrap();
    }

    fn scene(name: &str) -> Scene {
        Scene {
            name: name.into(),
            resolution: Some(Resolution {
                width: Some(64),
                height: Some(64),
            }),
            ..Default::default()
        }
    }

    fn engine(tmp: &Path, scene: Scene) -> Engine {
        Engine::new(VisualConfig {
            golden_dir: Some(tmp.join("golden")),
            diff_dir: Some(tmp.join("diff")),
            scene: vec![scene],
            ..Default::default()
        })
    }

    #[test]
    fn identical_images_pass() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("golden")).unwrap();
        let actual = tmp.path().join("actual.png");
        let golden = tmp.path().join("golden/main.png");
        let img = image(64, 64, [20, 40, 60, 255]);
        write_png(&actual, &img);
        write_png(&golden, &img);
        let scene = scene("main");
        let result = engine(tmp.path(), scene.clone())
            .compare(&actual, &scene)
            .unwrap();
        assert!(result.passed);
        assert!(result.ssim > 0.999);
        assert_eq!(result.max_delta, 0);
    }

    #[test]
    fn single_pixel_delta_can_trip_max_delta() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("golden")).unwrap();
        let actual = tmp.path().join("actual.png");
        let golden = tmp.path().join("golden/main.png");
        let mut changed = image(64, 64, [20, 40, 60, 255]);
        changed.put_pixel(1, 1, Rgba([21, 40, 60, 255]));
        write_png(&actual, &changed);
        write_png(&golden, &image(64, 64, [20, 40, 60, 255]));
        let mut scene = scene("main");
        scene.tolerance = Some(Tolerance {
            ssim: Some(0.0),
            max_pixel_delta: Some(0),
        });
        let result = engine(tmp.path(), scene.clone())
            .compare(&actual, &scene)
            .unwrap();
        assert!(!result.passed);
        assert!(result.ssim > 0.99);
        assert_eq!(result.max_delta, 1);
    }

    #[test]
    fn masked_region_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("golden")).unwrap();
        let actual = tmp.path().join("actual.png");
        let golden = tmp.path().join("golden/main.png");
        let mut changed = image(64, 64, [20, 40, 60, 255]);
        for y in 0..16 {
            for x in 0..16 {
                changed.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        write_png(&actual, &changed);
        write_png(&golden, &image(64, 64, [20, 40, 60, 255]));
        let mut scene = scene("main");
        scene.mask = vec![MaskRect {
            x: 0,
            y: 0,
            w: 16,
            h: 16,
        }];
        scene.tolerance = Some(Tolerance {
            ssim: Some(0.999),
            max_pixel_delta: Some(0),
        });
        let result = engine(tmp.path(), scene.clone())
            .compare(&actual, &scene)
            .unwrap();
        assert!(result.passed);
        assert_eq!(result.max_delta, 0);
        assert_eq!(result.masked_pixels, 256);
    }

    #[test]
    fn roi_crops_before_mask_coordinates() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("golden")).unwrap();
        let actual = tmp.path().join("actual.png");
        let golden = tmp.path().join("golden/main.png");
        let mut changed = image(64, 64, [20, 40, 60, 255]);
        changed.put_pixel(12, 12, Rgba([255, 0, 0, 255]));
        write_png(&actual, &changed);
        write_png(&golden, &image(64, 64, [20, 40, 60, 255]));
        let mut scene = scene("main");
        scene.roi = Some(Roi {
            x: 10,
            y: 10,
            w: 10,
            h: 10,
        });
        scene.mask = vec![MaskRect {
            x: 2,
            y: 2,
            w: 1,
            h: 1,
        }];
        scene.tolerance = Some(Tolerance {
            ssim: Some(0.999),
            max_pixel_delta: Some(0),
        });
        let result = engine(tmp.path(), scene.clone())
            .compare(&actual, &scene)
            .unwrap();
        assert!(result.passed);
        assert_eq!(result.max_delta, 0);
    }

    #[test]
    fn resolution_mismatch_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("golden")).unwrap();
        let actual = tmp.path().join("actual.png");
        let golden = tmp.path().join("golden/main.png");
        write_png(&actual, &image(32, 64, [20, 40, 60, 255]));
        write_png(&golden, &image(64, 64, [20, 40, 60, 255]));
        let scene = scene("main");
        let err = engine(tmp.path(), scene.clone())
            .compare(&actual, &scene)
            .unwrap_err();
        assert!(matches!(err, VisualError::ResolutionMismatch { .. }));
    }

    // ── Pure unit tests (no I/O) ────────────────────────────────────────

    #[test]
    fn max_pixel_delta_identical_images() {
        let img = image(4, 4, [20, 40, 60, 255]);
        assert_eq!(max_pixel_delta(&img, &img), 0);
    }

    #[test]
    fn max_pixel_delta_one_pixel_diff() {
        let actual = image(4, 4, [20, 40, 60, 255]);
        let mut golden = image(4, 4, [20, 40, 60, 255]);
        golden.put_pixel(2, 2, Rgba([21, 40, 60, 255]));
        assert_eq!(max_pixel_delta(&actual, &golden), 1);
    }

    #[test]
    fn max_pixel_delta_large_diff() {
        let actual = image(4, 4, [0, 0, 0, 255]);
        let mut golden = image(4, 4, [0, 0, 0, 255]);
        golden.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        assert_eq!(max_pixel_delta(&actual, &golden), 255);
    }

    #[test]
    fn max_pixel_delta_alpha_channel() {
        let actual = image(2, 2, [0, 0, 0, 0]);
        let golden = image(2, 2, [0, 0, 0, 255]);
        assert_eq!(max_pixel_delta(&actual, &golden), 255);
    }

    #[test]
    fn max_pixel_delta_empty_no_panic() {
        let img = image(0, 0, [0, 0, 0, 0]);
        assert_eq!(max_pixel_delta(&img, &img), 0);
    }

    #[test]
    fn delta_ramp_low_delta() {
        let color = delta_ramp(0);
        assert_eq!(color[0], 0);   // red = 0
        assert_eq!(color[2], 255); // blue = 255
    }

    #[test]
    fn delta_ramp_high_delta() {
        let color = delta_ramp(255);
        assert_eq!(color[0], 255); // red = 255
        assert_eq!(color[2], 0);   // blue = 0
    }

    #[test]
    fn delta_ramp_mid_delta() {
        let color = delta_ramp(128);
        // t = 128/255 ≈ 0.502, red ≈ 128, blue ≈ 127
        assert!(color[0] > 100 && color[0] < 200);
        assert!(color[2] > 100 && color[2] < 200);
        assert_eq!(color[1], 48); // green always 48
        assert_eq!(color[3], 255); // alpha always 255
    }

    #[test]
    fn effective_tolerance_scene_overrides_default() {
        let default_tol = Tolerance {
            ssim: Some(0.9),
            max_pixel_delta: Some(10),
        };
        let scene = Scene {
            name: "test".into(),
            tolerance: Some(Tolerance {
                ssim: Some(0.99),
                max_pixel_delta: Some(5),
            }),
            ..Default::default()
        };
        let tol = effective_tolerance(Some(&default_tol), &scene);
        assert_eq!(tol.ssim, 0.99);
        assert_eq!(tol.max_pixel_delta, 5);
    }

    #[test]
    fn effective_tolerance_falls_back_to_default() {
        let default_tol = Tolerance {
            ssim: Some(0.9),
            max_pixel_delta: Some(10),
        };
        let scene = Scene {
            name: "test".into(),
            tolerance: None,
            ..Default::default()
        };
        let tol = effective_tolerance(Some(&default_tol), &scene);
        assert_eq!(tol.ssim, 0.9);
        assert_eq!(tol.max_pixel_delta, 10);
    }

    #[test]
    fn effective_tolerance_falls_back_to_hardcoded_defaults() {
        let scene = Scene {
            name: "test".into(),
            tolerance: None,
            ..Default::default()
        };
        let tol = effective_tolerance(None, &scene);
        assert_eq!(tol.ssim, 0.985);
        assert_eq!(tol.max_pixel_delta, 255);
    }

    #[test]
    fn effective_tolerance_partial_scene_tolerance() {
        let scene = Scene {
            name: "test".into(),
            tolerance: Some(Tolerance {
                ssim: Some(0.95),
                max_pixel_delta: None,
            }),
            ..Default::default()
        };
        let tol = effective_tolerance(None, &scene);
        assert_eq!(tol.ssim, 0.95);
        assert_eq!(tol.max_pixel_delta, 255);
    }

    #[test]
    fn validate_resolution_matching() {
        let img = image(64, 64, [0, 0, 0, 0]);
        let path = Path::new("test.png");
        assert!(validate_resolution("test", path, &img, (64, 64)).is_ok());
    }

    #[test]
    fn validate_resolution_width_mismatch() {
        let img = image(32, 64, [0, 0, 0, 0]);
        let path = Path::new("test.png");
        let err = validate_resolution("test", path, &img, (64, 64)).unwrap_err();
        assert!(matches!(err, VisualError::ResolutionMismatch { .. }));
    }

    #[test]
    fn validate_resolution_zero_dimensions() {
        let img = image(0, 64, [0, 0, 0, 0]);
        let path = Path::new("test.png");
        let err = validate_resolution("test", path, &img, (0, 64)).unwrap_err();
        assert!(matches!(err, VisualError::Config { .. }));
    }

    #[test]
    fn expected_resolution_uses_golden_when_neither_set() {
        let scene = Scene {
            name: "test".into(),
            resolution: None,
            ..Default::default()
        };
        let golden = image(128, 128, [0, 0, 0, 0]);
        let (w, h) = expected_resolution(&scene, &golden).unwrap();
        assert_eq!(w, 128);
        assert_eq!(h, 128);
    }

    #[test]
    fn expected_resolution_uses_scene_when_both_set() {
        let scene = Scene {
            name: "test".into(),
            resolution: Some(Resolution {
                width: Some(100),
                height: Some(200),
            }),
            ..Default::default()
        };
        let golden = image(128, 128, [0, 0, 0, 0]);
        let (w, h) = expected_resolution(&scene, &golden).unwrap();
        assert_eq!(w, 100);
        assert_eq!(h, 200);
    }

    #[test]
    fn expected_resolution_errors_when_one_dimension_set() {
        let scene = Scene {
            name: "test".into(),
            resolution: Some(Resolution {
                width: Some(100),
                height: None,
            }),
            ..Default::default()
        };
        let golden = image(128, 128, [0, 0, 0, 0]);
        let err = expected_resolution(&scene, &golden).unwrap_err();
        assert!(matches!(err, VisualError::Config { .. }));
    }

    #[test]
    fn comparison_result_sidecar_maps_all_fields() {
        let diff = image(4, 4, [0, 0, 0, 0]);
        let result = ComparisonResult {
            scene: "main".into(),
            passed: true,
            ssim: 0.995,
            max_delta: 3,
            threshold: 0.99,
            max_pixel_delta_threshold: 10,
            mask_area_percent: 0.5,
            masked_pixels: 100,
            total_pixels: 1000,
            diff_image: diff,
        };
        let sidecar = result.sidecar();
        assert_eq!(sidecar.passed, true);
        assert_eq!(sidecar.ssim, 0.995);
        assert_eq!(sidecar.mask_area_percent, 0.5);
        assert_eq!(sidecar.masked_pixels, 100);
    }

    #[test]
    fn render_diff_detects_changed_area() {
        let width = 8;
        let height = 8;
        let mut actual = image(width, height, [20, 40, 60, 255]);
        let golden = image(width, height, [20, 40, 60, 255]);
        // Change top-left pixel
        actual.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let diff = render_diff(&actual, &golden, &[]);
        // Top-left pixel should be different (not grey)
        let tl = diff.get_pixel(0, 0);
        assert!(tl[0] > 0 || tl[1] > 0 || tl[2] > 0);
        assert_ne!(tl[0], tl[1]); // not grey if color diff
        // Bottom-right pixel should be grey (unchanged)
        let br = diff.get_pixel(width - 1, height - 1);
        assert_eq!(br[3], 77); // alpha = 77 for identical pixels
    }
}
