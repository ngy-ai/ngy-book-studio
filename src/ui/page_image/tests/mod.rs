//! Shared fixtures for the page image tests.

use super::*;

fn png_fixture() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
    let mut output = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut output, image::ImageFormat::Png)
        .expect("encode PNG fixture");
    output.into_inner()
}

fn page(
    book_id: &str,
    unit_id: &str,
    page_number: u32,
    source: SourceLocator,
) -> PublishedVisualPage {
    PublishedVisualPage {
        file_name: format!("Page{page_number:05}.png"),
        media_type: "image/png".to_string(),
        bytes: png_fixture(),
        content_unit_id: Some(unit_id.to_string()),
        locator: DocumentLocator::unit(book_id, unit_id).with_source(source),
        width: 800,
        height: 1_200,
    }
}

fn slide_page(book_id: &str, unit_id: &str, page_number: u32) -> PublishedVisualPage {
    page(
        book_id,
        unit_id,
        page_number,
        SourceLocator::slide(page_number),
    )
}

fn rendered_page(book_id: &str, unit_id: &str, page_number: u32) -> PublishedVisualPage {
    let mut page = page(
        book_id,
        unit_id,
        page_number,
        SourceLocator::office_rendered_page(page_number),
    );
    page.content_unit_id = None;
    page
}

fn section_page(
    book_id: &str,
    unit_id: &str,
    persisted_page_number: u32,
    section_index: u32,
) -> PublishedVisualPage {
    page(
        book_id,
        unit_id,
        persisted_page_number,
        SourceLocator::office_section(section_index),
    )
}

fn worksheet_page(
    book_id: &str,
    unit_id: &str,
    persisted_page_number: u32,
    name: &str,
    range: &str,
) -> PublishedVisualPage {
    page(
        book_id,
        unit_id,
        persisted_page_number,
        SourceLocator::worksheet(name, Some(range.to_string())),
    )
}

fn djvu_page(book_id: &str, unit_id: &str, page_number: u32) -> PublishedVisualPage {
    page(
        book_id,
        unit_id,
        page_number,
        SourceLocator::djvu_page(page_number),
    )
}

mod pages_test;

mod reference_test;
