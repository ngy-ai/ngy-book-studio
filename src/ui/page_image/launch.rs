//! Opening the page window and naming it.

use super::*;

pub(crate) fn open_page_image_window(
    book_id: String,
    book_title: String,
    kind: VisualPageSourceKind,
    initial_page: u32,
    pages: Vec<PublishedVisualPage>,
    library: LibraryStore,
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    anyhow::ensure!(!book_id.trim().is_empty(), "图书 ID 不能为空");
    let pages = pages_from_persisted_pages(&book_id, kind, pages)?;
    // Reading progress is written against the book's reading incarnation so a
    // later re-import or reopen cannot resurrect a stale position.
    let book_incarnation = library.progress_incarnation(&book_id);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1180.), px(820.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(520.))),
        titlebar: Some(TitlebarOptions {
            title: Some(format!("《{book_title}》· {}", page_window_title(kind)).into()),
            ..Default::default()
        }),
        app_id: Some(page_window_app_id(kind).to_string()),
        ..Default::default()
    };
    let window_book_id = book_id.clone();
    cx.open_window(options, move |window, cx| {
        let reader = cx.new(|cx| {
            PageImageApp::new(
                book_id,
                book_title,
                kind,
                initial_page,
                book_incarnation,
                pages,
                library,
                services,
                library_view,
                window,
                cx,
            )
        });
        let close_reader = reader.downgrade();
        on_window_close(window, cx, move |_window, cx| {
            close_reader
                .update(cx, |reader, cx| reader.handle_window_close(cx))
                .unwrap_or(true)
        });
        // Deleting the book must take this window with it. There is no child
        // WebView and nothing left to save, so the window can go after the
        // current frame.
        let removed_reader = reader.downgrade();
        register_book_window(
            window_book_id,
            window,
            move |window, cx| {
                if let Some(reader) = removed_reader.upgrade() {
                    reader.update(cx, |reader, cx| reader.close_for_removed_book(cx));
                }
                remove_window_after_current_frame(window, cx, None);
            },
            cx,
        );
        cx.new(|cx| Root::new(reader, window, cx))
    })?;
    Ok(())
}

pub(crate) fn page_window_title(kind: VisualPageSourceKind) -> &'static str {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => "Office 增强预览",
        VisualPageSourceKind::Djvu => "DjVu 阅读",
    }
}

pub(crate) fn page_window_app_id(kind: VisualPageSourceKind) -> &'static str {
    match kind {
        VisualPageSourceKind::OfficeEnhanced => "dev.ngy.book-studio.office-slides",
        VisualPageSourceKind::Djvu => "dev.ngy.book-studio.djvu-reader",
    }
}
