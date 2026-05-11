use image::{Rgba, RgbaImage};

use crate::core::config::MaskRect;

pub fn apply_mask(image: &mut RgbaImage, mask: &[MaskRect]) -> anyhow::Result<MaskStats> {
    validate_mask(image.width(), image.height(), mask)?;
    let mut masked = vec![false; image.width() as usize * image.height() as usize];
    for rect in mask {
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                masked[y as usize * image.width() as usize + x as usize] = true;
            }
        }
    }
    let masked_pixels = masked.into_iter().filter(|masked| *masked).count();
    let total_pixels = image.width() as usize * image.height() as usize;
    let mask_area_percent = if total_pixels == 0 {
        0.0
    } else {
        masked_pixels as f64 * 100.0 / total_pixels as f64
    };
    Ok(MaskStats {
        masked_pixels,
        total_pixels,
        mask_area_percent,
    })
}

pub fn is_masked(x: u32, y: u32, mask: &[MaskRect]) -> bool {
    mask.iter().any(|rect| {
        x >= rect.x
            && x < rect.x.saturating_add(rect.w)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.h)
    })
}

pub fn validate_mask(width: u32, height: u32, mask: &[MaskRect]) -> anyhow::Result<()> {
    for rect in mask {
        let right = rect
            .x
            .checked_add(rect.w)
            .ok_or_else(|| anyhow::anyhow!("mask x+w overflows"))?;
        let bottom = rect
            .y
            .checked_add(rect.h)
            .ok_or_else(|| anyhow::anyhow!("mask y+h overflows"))?;
        if rect.w == 0 || rect.h == 0 {
            anyhow::bail!("mask rectangles must have non-zero width and height");
        }
        if right > width || bottom > height {
            anyhow::bail!(
                "mask {}x{} at ({}, {}) exceeds comparison image {}x{}",
                rect.w,
                rect.h,
                rect.x,
                rect.y,
                width,
                height
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct MaskStats {
    pub masked_pixels: usize,
    pub total_pixels: usize,
    pub mask_area_percent: f64,
}
