//! Parsing a vision response into chunks.

use super::*;

pub(crate) fn parse_vision_output(response: &str) -> Result<VisionOutput> {
    ensure!(
        response.len() <= MAX_VISION_RESPONSE_BYTES,
        "视觉模型响应超过大小上限"
    );
    let mut output = serde_json::from_str::<VisionOutput>(response.trim())
        .context("视觉模型没有返回约定的 JSON")?;
    ensure!(!output.regions.is_empty(), "视觉模型没有返回可索引区域");
    ensure!(
        output.regions.len() <= MAX_VISION_REGIONS_PER_PAGE,
        "视觉模型返回区域数量超过上限"
    );

    let mut seen_regions = HashSet::with_capacity(output.regions.len());
    let mut total_text_chars = 0_usize;
    for (index, region) in output.regions.iter_mut().enumerate() {
        region.ocr = region.ocr.trim().to_string();
        region.description = region.description.trim().to_string();
        let ocr_chars = region.ocr.chars().count();
        let description_chars = region.description.chars().count();
        ensure!(
            ocr_chars <= MAX_VISION_REGION_TEXT_CHARS
                && description_chars <= MAX_VISION_REGION_TEXT_CHARS,
            "视觉模型第 {} 个区域文本超过大小上限",
            index + 1
        );
        total_text_chars = total_text_chars
            .checked_add(ocr_chars)
            .and_then(|total| total.checked_add(description_chars))
            .context("视觉模型页面文本长度溢出")?;
        ensure!(
            total_text_chars <= MAX_VISION_TOTAL_TEXT_CHARS,
            "视觉模型页面文本超过总大小上限"
        );
        ensure!(
            !region.ocr.is_empty() || !region.description.is_empty(),
            "视觉模型第 {} 个区域没有返回可索引内容",
            index + 1
        );
        if let Some(bounds) = region.bounds() {
            bounds
                .validate()
                .with_context(|| format!("视觉模型第 {} 个区域坐标无效", index + 1))?;
        }
        ensure!(
            seen_regions.insert(region.bounds()),
            "视觉模型返回了重复区域"
        );
    }
    output.regions.sort_by_key(|region| match region.bounds() {
        None => (0, 0, 0, 0, 0),
        Some(bounds) => (1, bounds.top, bounds.left, bounds.bottom, bounds.right),
    });
    Ok(output)
}

pub(crate) fn visual_chunk_body(output: &VisionRegion) -> String {
    match (output.ocr.is_empty(), output.description.is_empty()) {
        (false, false) => format!(
            "OCR：\n{}\n\n页面说明：\n{}",
            output.ocr, output.description
        ),
        (false, true) => format!("OCR：\n{}", output.ocr),
        (true, false) => format!("页面说明：\n{}", output.description),
        (true, true) => unreachable!("validated vision output is non-empty"),
    }
}

pub(crate) fn visual_chunk_ordinal(page_index: usize, region_index: usize) -> Result<usize> {
    ensure!(
        region_index < VISUAL_CHUNK_ORDINALS_PER_PAGE,
        "视觉区域序号超过页面槽位上限"
    );
    VISUAL_CHUNK_ORDINAL_BASE
        .checked_add(
            page_index
                .checked_mul(VISUAL_CHUNK_ORDINALS_PER_PAGE)
                .context("视觉页面数量超过索引上限")?,
        )
        .and_then(|base| base.checked_add(region_index))
        .context("视觉页面序号超过索引上限")
}

pub(crate) fn visual_chunk_id(
    source_id: &str,
    page_index: usize,
    region: Option<NormalizedRect>,
) -> String {
    let coordinate = match region {
        Some(region) => format!(
            "region:{}:{}:{}:{}",
            region.left, region.top, region.right, region.bottom
        ),
        None => "full-page".to_string(),
    };
    deterministic_id(
        "vision-chunk",
        format!("{source_id}\0{page_index}\0{coordinate}").as_bytes(),
    )
}
