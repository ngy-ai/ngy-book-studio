//! Opening or focusing the notes window.

use super::*;

pub(crate) fn open_notes_window(
    services: Arc<AppServices>,
    library_view: Entity<EpubReaderApp>,
    book: Option<(String, String)>,
    cx: &mut App,
) -> Result<()> {
    if application_is_exiting(cx) {
        return Ok(());
    }
    let title = book
        .as_ref()
        .map(|(_, title)| format!("《{title}》 · 本书笔记"))
        .unwrap_or_else(|| "墨页 · 全部笔记".into());
    let book_id = book.as_ref().map(|(id, _)| id.clone());
    // One notes window per scope: "全部笔记" is global, "本书笔记" is per book.
    let key = book_id
        .as_ref()
        .map(|id| singleton_window_key("notes", id))
        .unwrap_or_else(|| singleton_window_key("notes", "all"));
    match reserve_singleton_window(&key, cx) {
        SingletonWindowReservation::Activate(handle) => {
            activate_singleton_window(handle, cx);
            return Ok(());
        }
        SingletonWindowReservation::InFlight => return Ok(()),
        SingletonWindowReservation::Reserved => {}
    }
    let bounds = Bounds::centered(None, size(px(860.), px(760.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(680.), px(500.))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            app_id: Some("dev.ngy.book-studio.notes".into()),
            ..Default::default()
        },
        move |window, cx| {
            let notes = cx.new(|cx| NotesWindow::new(services, library_view, book, window, cx));
            notes.update(cx, |this, cx| this.refresh(window, cx));
            on_window_close(window, cx, |_, _| true);
            if let Some(book_id) = book_id {
                register_book_window(
                    book_id,
                    window,
                    |window, cx| remove_window_after_current_frame(window, cx, None),
                    cx,
                );
            }
            cx.new(|cx| Root::new(notes, window, cx))
        },
    );
    match opened {
        Ok(handle) => {
            complete_singleton_window(&key, handle.into(), cx);
            Ok(())
        }
        Err(error) => {
            release_singleton_window(&key, cx);
            Err(error).context("无法创建笔记窗口")
        }
    }
}
