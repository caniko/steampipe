use image::{DynamicImage, RgbaImage};

// SSIM crate choice: `image-compare` 0.4.2 is pure Rust, has no system
// dependencies, and is already aligned with this project via `image` 0.25.
// `dssim-core` is oriented around DSSIM/MS-SSIM distance semantics rather than
// the scene threshold model here, and `fast-ssim2` currently requires Rust 1.89
// while this crate declares MSRV 1.88. The wrapper converts masked/ROI RGBA
// frames to luma and uses MSSIMSimple, keeping threshold semantics as
// "1.0 means identical, lower means less similar".
pub fn mssim(actual: &RgbaImage, golden: &RgbaImage) -> anyhow::Result<f64> {
    let actual = DynamicImage::ImageRgba8(actual.clone()).into_luma8();
    let golden = DynamicImage::ImageRgba8(golden.clone()).into_luma8();
    let result = image_compare::gray_similarity_structure(
        &image_compare::Algorithm::MSSIMSimple,
        &actual,
        &golden,
    )
    .map_err(|e| anyhow::anyhow!("compute SSIM: {e}"))?;
    Ok(result.score)
}
