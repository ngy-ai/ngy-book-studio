//! The page model and how persisted pages become it.

use super::*;

pub(crate) struct PageImage {
    pub(crate) file_name: String,
    pub(crate) page_number: u32,
    /// Natural pixel size of the published page image, used for reader zoom.
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) image: Arc<Image>,
    pub(crate) locator: DocumentLocator,
}

pub(crate) fn pages_from_persisted_pages(
    book_id: &str,
    kind: VisualPageSourceKind,
    pages: Vec<PublishedVisualPage>,
) -> Result<Vec<PageImage>> {
    anyhow::ensure!(!pages.is_empty(), "视觉任务尚未发布任何页面");
    let mut locators = HashSet::with_capacity(pages.len());
    let mut file_names = HashSet::with_capacity(pages.len());
    let mut previous_file_name = None;
    let mut previous_source_page = None;
    pages
        .into_iter()
        .enumerate()
        .map(|(page_index, page)| {
            page.locator.validate().context("视觉页面定位信息无效")?;
            anyhow::ensure!(page.locator.book_id == book_id, "视觉页面不属于当前图书");
            anyhow::ensure!(page.width > 0 && page.height > 0, "视觉页面尺寸无效");
            let page_number = u32::try_from(page_index + 1).context("视觉页面数量超出支持范围")?;
            match kind {
                VisualPageSourceKind::OfficeEnhanced => match page.locator.source.as_ref() {
                    Some(SourceLocator::OfficeRenderedPage { .. }) => anyhow::ensure!(
                        page.content_unit_id.is_none(),
                        "Word/Excel 增强预览页不能关联内容单元"
                    ),
                    _ => anyhow::ensure!(
                        page.content_unit_id.as_deref() == Some(page.locator.unit_id.as_str()),
                        "Office 增强页面与内容单元定位不一致"
                    ),
                },
                VisualPageSourceKind::Djvu => {
                    let Some(SourceLocator::DjvuPage { page: source_page }) =
                        page.locator.source.as_ref()
                    else {
                        anyhow::bail!("DjVu 页面缺少源页码定位");
                    };
                    anyhow::ensure!(
                        page.content_unit_id.as_deref() == Some(page.locator.unit_id.as_str()),
                        "DjVu 页面与内容单元定位不一致"
                    );
                    anyhow::ensure!(
                        previous_source_page.is_none_or(|previous| previous < *source_page),
                        "DjVu 页面源页码顺序与持久化页面顺序不一致"
                    );
                    anyhow::ensure!(
                        page_number == *source_page,
                        "DjVu 页面源页码与持久化页面顺序不一致"
                    );
                    previous_source_page = Some(*source_page);
                }
            }
            anyhow::ensure!(
                matches!(
                    page.locator.source.as_ref(),
                    Some(
                        SourceLocator::Slide { .. }
                            | SourceLocator::OfficeSection { .. }
                            | SourceLocator::Worksheet { .. }
                            | SourceLocator::OfficeRenderedPage { .. }
                            | SourceLocator::DjvuPage { .. }
                    )
                ),
                "视觉页面使用了不支持的源定位"
            );
            let locator_key =
                serde_json::to_string(&page.locator).context("无法序列化视觉页面定位信息")?;
            anyhow::ensure!(locators.insert(locator_key), "视觉页面包含重复的定位信息");
            anyhow::ensure!(!page.file_name.trim().is_empty(), "视觉页面文件名为空");
            anyhow::ensure!(
                file_names.insert(page.file_name.clone()),
                "视觉页面包含重复的文件名"
            );
            anyhow::ensure!(
                previous_file_name
                    .as_deref()
                    .is_none_or(|previous| previous < page.file_name.as_str()),
                "视觉页面文件名顺序与持久化页面顺序不一致"
            );
            previous_file_name = Some(page.file_name.clone());
            let format = image_format_from_mime(&page.media_type)
                .with_context(|| format!("不支持的视觉页面图片格式：{}", page.media_type))?;
            Ok(PageImage {
                file_name: page.file_name,
                page_number,
                width: page.width,
                height: page.height,
                image: Arc::new(Image::from_bytes(format, page.bytes)),
                locator: page.locator,
            })
        })
        .collect()
}
