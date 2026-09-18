//! Detecting the concrete Kindle format and flattening its navigation.

use super::*;

pub(crate) fn kindle_format(bytes: &[u8], extension: Option<&str>) -> BookFormat {
    if mobi_version(bytes).is_some_and(|version| version >= 8) || extension == Some("azw3") {
        BookFormat::Azw3
    } else if extension == Some("azw") {
        BookFormat::Azw
    } else {
        BookFormat::Mobi
    }
}

/// Shared with the KFX importer: `ebook-rs` exposes the same `NavPoint` tree
/// for both PalmDB MOBI/AZW and KFX containers.
pub(crate) fn flatten_nav(points: &[ebook_rs::NavPoint]) -> Vec<&ebook_rs::NavPoint> {
    pub(crate) fn visit<'a>(
        points: &'a [ebook_rs::NavPoint],
        output: &mut Vec<&'a ebook_rs::NavPoint>,
    ) {
        for point in points {
            output.push(point);
            visit(&point.subitems, output);
        }
    }
    let mut output = Vec::new();
    visit(points, &mut output);
    output
}

pub(crate) fn kindle_toc_node(
    point: &ebook_rs::NavPoint,
    href_to_unit: &HashMap<String, String>,
    book_id: &str,
) -> Option<TocNode> {
    let unit_id = href_to_unit
        .get(&href_key(&point.href))
        .or_else(|| href_to_unit.get(&href_key(&point.full_path)))?
        .clone();
    let mut node = TocNode::new(
        deterministic_id(
            "toc",
            format!("{book_id}\0{}\0{}", point.id, point.href).as_bytes(),
        ),
        safe_title(Some(&point.label), "Untitled"),
        TocTarget::unit(unit_id),
    );
    node.children = point
        .subitems
        .iter()
        .filter_map(|child| kindle_toc_node(child, href_to_unit, book_id))
        .collect();
    Some(node)
}

pub(crate) fn href_key(href: &str) -> String {
    href.split(['?', '#'])
        .next()
        .unwrap_or(href)
        .trim_start_matches('/')
        .to_ascii_lowercase()
}
