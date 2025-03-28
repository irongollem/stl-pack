use image::DynamicImage;
use smartcrop;
use std::num::NonZeroU32;
use std::path::Path;

pub fn generate_smart_thumbnail(image_path: &Path, size: u32) -> Result<DynamicImage, String> {
    // Open the image
    let img = image::open(image_path).map_err(|e| format!("Failed to open image: {}", e))?;

    // Find the best crop area
    let result = smartcrop::find_best_crop(
        &img,
        NonZeroU32::new(size).unwrap(),
        NonZeroU32::new(size).unwrap(),
    )
    .map_err(|e| format!("Failed to find best crop: {}", e))?;

    let c = result.crop;

    // Extract the crop suggestion
    let crop_sqr = img.crop_imm(c.x, c.y, c.width, c.height);
    let thumbnail = crop_sqr.resize(size, size, image::imageops::FilterType::Lanczos3);

    Ok(thumbnail)
}
