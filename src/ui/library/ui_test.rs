//! Renders the production library window against a temporary library, so the
//! card/list switch and the sort order are verified on real GPUI frames and
//! not only through the pure helpers.

use super::*;
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

fn redraw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, app| window.draw(app).clear(app));
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

/// Left edge of a group row's name in the sidebar. The label sits after the
/// disclosure lane, so this coordinate is the row's visual leading spine.
fn group_name_left(cx: &mut VisualTestContext, group_id: &str) -> gpui_kit::Pixels {
    let selector: &'static str =
        Box::leak(format!("group-select-text-{group_id}").into_boxed_str());
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("group row of {group_id} must be rendered"))
        .left()
}

fn row_top(cx: &mut VisualTestContext, book_id: &str) -> gpui_kit::Pixels {
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
#[gpui_kit::test]
fn list_view_renders_rows_in_the_selected_sort_order(cx: &mut TestAppContext) {
    init_ui_test(cx);
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

/// Rows of the group tree share one leading spine: a name's length only moves
/// where the text ends, and a level only adds one indent step. This guards the
/// frame the user reported — when the label inside the row `Button` is centered
/// instead of leading-aligned, the folder icon drifts with the name's width and
/// short names look like the whole row slid to the middle of the sidebar.
#[gpui_kit::test]
fn group_rows_share_one_leading_spine(cx: &mut TestAppContext) {
    init_ui_test(cx);
    let directory = tempfile::tempdir().unwrap();
    let services = Arc::new(AppServices::open(directory.path()).unwrap());
    let mut roots = Vec::new();
    for name in ["AI", "深度学习与智能体实验记录"] {
        let created = services.runtime().block_on(async {
            let name = name.to_string();
            services
                .spawn_library_projected(move |library| library.create_group(&name, None))
                .await
                .unwrap()
                .unwrap()
        });
        roots.push(created.value.id);
    }
    let parent = roots[0].clone();
    let created = services.runtime().block_on(async {
        services
            .spawn_library_projected(move |library| library.create_group("Infra", Some(&parent)))
            .await
            .unwrap()
            .unwrap()
    });
    let child = created.value.id.clone();
    // 最后一次分组变更的快照已经包含上面创建的全部分组。
    let snapshot = created.snapshot;

    let mut view = None;
    let (_, visual) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| EpubReaderApp::new(snapshot, Arc::clone(&services), window, cx));
        view = Some(app.clone());
        Root::new(app, window, cx)
    });
    let _app = view.expect("the library window is built with the fixture");
    redraw(visual);

    let short = group_name_left(visual, &roots[0]);
    let long = group_name_left(visual, &roots[1]);
    assert_eq!(
        short, long,
        "a longer group name must not move the text column: {short:?} {long:?}"
    );

    let child = group_name_left(visual, &child);
    assert_eq!(
        child - short,
        px(render_sidebar::GROUP_LEVEL_INDENT),
        "a nested group only adds one indent step: {child:?} {short:?}"
    );
}
