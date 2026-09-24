//! Renders the production library window against a temporary library, so the
//! card/list switch and the sort order are verified on real GPUI frames and
//! not only through the pure helpers.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear());
    cx.run_until_parked();
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} must be rendered"));
    assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    cx.simulate_click(bounds.center(), Modifiers::none());
    redraw(cx);
}

fn row_top(cx: &mut VisualTestContext, book_id: &str) -> gpui::Pixels {
    // `debug_bounds` only accepts `&'static str`. The ids live in the fixture
    // for the whole test, so leaking the selector here is bounded and
    // deliberate.
    let selector: &'static str = Box::leak(format!("book-row-{book_id}").into_boxed_str());
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("row of {book_id} must be rendered"))
        .top()
}

/// Builds a shelf whose catalog order (newest import first) is opposite to the
/// title order, then checks that the list layout follows the picked sort.
#[gpui::test]
fn list_view_renders_rows_in_the_selected_sort_order(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let directory = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(directory.path()).unwrap());
    let mut books = Vec::new();
    let mut snapshot = None;
    for (title, author) in [("Zeta", "甲"), ("alpha", "乙"), ("Beta", "丙")] {
        let created = services.runtime().block_on(async {
            services
                .spawn_library_projected(move |library| library.create_book(title, author))
                .await
                .unwrap()
                .unwrap()
        });
        books.push((title.to_string(), created.value.id.clone()));
        snapshot = Some(created.snapshot);
    }
    let snapshot = snapshot.expect("the fixture creates at least one book");

    let mut view = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| EpubReaderApp::new(snapshot, Arc::clone(&services), window, cx));
        view = Some(app.clone());
        Root::new(app, window, cx)
    });
    let app = view.expect("the library window is built with the fixture");
    redraw(visual);
    assert!(
        visual.debug_bounds("library-book-grid").is_some(),
        "the shelf opens on the card grid"
    );

    click(visual, "library-view-list");
    assert!(
        visual.debug_bounds("library-book-list").is_some(),
        "the list toggle must render the one-book-per-row layout"
    );

    visual.update(|_, cx| {
        app.update(cx, |this, cx| {
            this.set_book_sort_field(BookSortField::Title, cx);
        });
    });
    redraw(visual);

    let alpha = row_top(visual, &books[1].1);
    let beta = row_top(visual, &books[2].1);
    let zeta = row_top(visual, &books[0].1);
    assert!(
        alpha < beta && beta < zeta,
        "title ascending must stack alpha above Beta above Zeta: {alpha:?} {beta:?} {zeta:?}"
    );

    visual.update(|_, cx| {
        app.update(cx, |this, cx| {
            this.set_book_sort_direction(BookSortDirection::Descending, cx);
        });
    });
    redraw(visual);

    let alpha = row_top(visual, &books[1].1);
    let beta = row_top(visual, &books[2].1);
    let zeta = row_top(visual, &books[0].1);
    assert!(
        zeta < beta && beta < alpha,
        "flipping the direction must restack the same rows: {zeta:?} {beta:?} {alpha:?}"
    );
}
