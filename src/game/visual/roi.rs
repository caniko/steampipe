use image::{GenericImageView, RgbaImage};

use crate::core::config::Roi;

pub fn crop(image: &RgbaImage, roi: &Roi) -> anyhow::Result<RgbaImage> {
    validate_roi(image.width(), image.height(), roi)?;
    Ok(image.view(roi.x, roi.y, roi.w, roi.h).to_image())
}

pub fn validate_roi(width: u32, height: u32, roi: &Roi) -> anyhow::Result<()> {
    let right = roi
        .x
        .checked_add(roi.w)
        .ok_or_else(|| anyhow::anyhow!("ROI x+w overflows"))?;
    let bottom = roi
        .y
        .checked_add(roi.h)
        .ok_or_else(|| anyhow::anyhow!("ROI y+h overflows"))?;
    if roi.w == 0 || roi.h == 0 {
        anyhow::bail!("ROI must have non-zero width and height");
    }
    if right > width || bottom > height {
        anyhow::bail!(
            "ROI {}x{} at ({}, {}) exceeds image {}x{}",
            roi.w,
            roi.h,
            roi.x,
            roi.y,
            width,
            height
        );
    }
    Ok(())
}
