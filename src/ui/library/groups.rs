//! Group mutations and deleting a book.

use super::*;

impl EpubReaderApp {
    pub(super) fn select_group(&mut self, filter: GroupFilter, cx: &mut Context<Self>) {
        if self.group_filter == filter {
            return;
        }
        let path = match &filter {
            GroupFilter::Group(id) => self.library.group_path(id),
            _ => Vec::new(),
        };
        self.group_filter = filter;
        self.group_filter_generation = self.group_filter_generation.wrapping_add(1);
        for group in path {
            self.collapsed_groups.remove(&group.id);
        }
        self.notice = None;
        self.library_notice_request = None;
        self.sync_ai_scope(cx);
    }

    pub(super) fn toggle_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if !self.collapsed_groups.remove(&group_id) {
            self.collapsed_groups.insert(group_id);
        }
        cx.notify();
    }

    pub(super) fn create_group(
        &mut self,
        parent_id: Option<String>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let filter_generation = self.group_filter_generation;
        // A group created from the book group picker must not also switch the
        // shelf behind it; the picker keeps the user's current shelf intact.
        let returns_to_picker = matches!(
            self.group_modal.as_ref(),
            Some(GroupModal::NameInput {
                return_to: Some(_),
                ..
            })
        );
        let Some(request_id) = self.begin_library_request("正在创建分组…", cx) else {
            return;
        };
        if let Some(GroupModal::NameInput {
            pending_request,
            error,
            ..
        }) = self.group_modal.as_mut()
        {
            *pending_request = Some(request_id);
            *error = None;
        }
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| {
            library.create_group(&name, parent_id.as_deref())
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        let group = mutation.value;
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            if !returns_to_picker
                                && request_id == this.library_request_generation
                                && filter_generation == this.group_filter_generation
                            {
                                this.group_filter = GroupFilter::Group(group.id.clone());
                                this.group_filter_generation =
                                    this.group_filter_generation.wrapping_add(1);
                            }
                            this.sync_ai_scope(cx);
                        }
                        let created_id = group.id.clone();
                        let created_parent = group.parent_id.clone();
                        this.finish_name_modal_request(request_id, None, cx);
                        // Select the new group in the restored picker so the
                        // user only has to confirm.
                        if let Some(GroupModal::BookGroupPicker {
                            selected, expanded, ..
                        }) = this.group_modal.as_mut()
                        {
                            if let Some(parent_id) = created_parent {
                                expanded.insert(parent_id);
                            }
                            *selected = Some(created_id);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("已创建分组「{}」", group.name),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => {
                        let message = format!("创建分组失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("创建分组任务异常停止：{error}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn rename_group(&mut self, group_id: String, name: String, cx: &mut Context<Self>) {
        let Some(request_id) = self.begin_library_request("正在重命名分组…", cx) else {
            return;
        };
        if let Some(GroupModal::NameInput {
            pending_request,
            error,
            ..
        }) = self.group_modal.as_mut()
        {
            *pending_request = Some(request_id);
            *error = None;
        }
        let services = Arc::clone(&self.services);
        let completed_name = name.clone();
        let task = services.spawn_library_projected(move |library| {
            library.rename_group(&group_id, &name)?;
            Ok(())
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        this.finish_name_modal_request(request_id, None, cx);
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("分组已重命名为「{completed_name}」"),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => {
                        let message = format!("重命名分组失败：{error:#}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                    Err(error) => {
                        let message = format!("重命名分组任务异常停止：{error}");
                        this.finish_name_modal_request(request_id, Some(message.clone()), cx);
                        this.set_library_request_error(request_id, message, cx);
                    }
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn delete_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        let name = self
            .library
            .group(&group_id)
            .map(|group| group.name.clone())
            .unwrap_or_default();
        let Some(request_id) = self.begin_library_request("正在删除分组…", cx) else {
            return;
        };
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| library.delete_group(&group_id));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: if name.is_empty() {
                                    "分组已删除".to_string()
                                } else {
                                    format!("分组「{name}」已删除，其中的图书已移出分组")
                                },
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("删除分组失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("删除分组任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn move_book_to_group(
        &mut self,
        book_id: String,
        group_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let target = group_id
            .as_deref()
            .and_then(|group_id| self.library.group(group_id))
            .map(|group| group.name.clone());
        let Some(request_id) = self.begin_library_request("正在移动图书…", cx) else {
            return;
        };
        let services = Arc::clone(&self.services);
        let task = services.spawn_library_projected(move |library| {
            library.set_book_group(&book_id, group_id.as_deref())
        });
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: match target {
                                    Some(name) => format!("已移动到「{name}」"),
                                    None => "已移出分组".to_string(),
                                },
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("移动图书失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("移动图书任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    /// Opens the group tree picker for a single book. The tree replaces the
    /// flat group list that used to be inlined into the book context menu; a
    /// menu item per group does not scale once a library has nested groups.
    pub(super) fn open_book_group_picker(&mut self, book_id: String, cx: &mut Context<Self>) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        let selected = book.group_id.clone();
        let expanded = match selected.as_deref() {
            // Expand the ancestors so the book's current group is visible.
            Some(group_id) => picker_expanded_groups(&self.library.group_path(group_id)),
            None => HashSet::new(),
        };
        self.group_modal = Some(GroupModal::BookGroupPicker {
            book_id: book.id,
            book_title: book.title,
            selected,
            expanded,
        });
        cx.notify();
    }

    pub(super) fn select_picker_group(&mut self, group_id: Option<String>, cx: &mut Context<Self>) {
        if let Some(GroupModal::BookGroupPicker { selected, .. }) = self.group_modal.as_mut() {
            *selected = group_id;
            cx.notify();
        }
    }

    pub(super) fn toggle_picker_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if let Some(GroupModal::BookGroupPicker { expanded, .. }) = self.group_modal.as_mut() {
            if !expanded.remove(&group_id) {
                expanded.insert(group_id);
            }
            cx.notify();
        }
    }

    pub(super) fn open_create_book_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_importing {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("请输入图书名称")
                .default_value(String::new())
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self.group_modal = Some(GroupModal::NameInput {
            title: "新建图书".to_string(),
            confirm_label: "创建".to_string(),
            action: NameInputAction::CreateBook,
            input,
            pending_request: None,
            error: None,
            return_to: None,
        });
        cx.notify();
    }

    pub(super) fn open_create_group_dialog(
        &mut self,
        parent_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(parent_id) = parent_id.as_deref() {
            if self.library.group(parent_id).is_none() {
                return;
            }
        }
        let title = match parent_id.as_deref().and_then(|id| self.library.group(id)) {
            Some(parent) => format!("在「{}」下新建子分组", parent.name),
            None => "新建分组".to_string(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(String::new()));
        input.update(cx, |state, cx| state.focus(window, cx));
        // Creating a group from inside the book group picker must not swallow
        // the picker: the name input hands control back once it completes.
        let return_to = match self.group_modal.as_ref() {
            Some(GroupModal::BookGroupPicker { .. }) => self.group_modal.clone().map(Box::new),
            _ => None,
        };
        self.group_modal = Some(GroupModal::NameInput {
            title,
            confirm_label: "创建".to_string(),
            action: NameInputAction::CreateGroup(parent_id),
            input,
            pending_request: None,
            error: None,
            return_to,
        });
        cx.notify();
    }

    pub(super) fn open_rename_group_dialog(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.library.group(&group_id).cloned() else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(group.name.clone()));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.group_modal = Some(GroupModal::NameInput {
            title: format!("重命名「{}」", group.name),
            confirm_label: "保存".to_string(),
            action: NameInputAction::RenameGroup(group_id),
            input,
            pending_request: None,
            error: None,
            return_to: None,
        });
        cx.notify();
    }

    pub(super) fn open_delete_group_dialog(
        &mut self,
        group_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.library.group(&group_id).cloned() else {
            return;
        };
        let descendants = self.library.group_subtree_ids(&group_id).len() - 1;
        let books = self.library.group_subtree_book_count(&group_id);
        let description = if descendants == 0 {
            format!(
                "将删除分组「{}」。其中 {} 本图书会变为未分组，图书本身不会被移除。",
                group.name, books
            )
        } else {
            format!(
                "将删除分组「{}」及其 {} 个子分组。其中 {} 本图书会变为未分组，图书本身不会被移除。",
                group.name, descendants, books
            )
        };
        self.group_modal = Some(GroupModal::DeleteConfirm {
            group_id,
            name: group.name.clone(),
            description,
        });
        cx.notify();
    }

    pub(super) fn open_delete_book_dialog(
        &mut self,
        book_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        self.group_modal = Some(GroupModal::DeleteBook {
            book_id,
            title: book.title.clone(),
            description: format!(
                "将删除图书「{}」及书库中的副本与封面，源文件不会被移除。该书已打开的阅读、预览与编辑窗口会一并关闭，其中未保存的修改将丢失。此操作不可撤销。",
                book.title
            ),
        });
        cx.notify();
    }

    pub(super) fn open_office_trust_dialog(&mut self, book_id: String, cx: &mut Context<Self>) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            self.set_error("图书已经不存在。".to_string(), cx);
            return;
        };
        if !is_office_format_name(&book.format) {
            self.set_error(
                "该图书格式不支持 Microsoft Office 增强预览。".to_string(),
                cx,
            );
            return;
        }
        self.group_modal = Some(GroupModal::OfficeTrust {
            book_id,
            title: book.title.clone(),
            description: format!(
                "仅当你信任「{}」的原始文件时继续。墨页会在只读模式下调用本机 Microsoft Office，禁用宏并禁止更新外部链接，但 Office 自动化不是安全沙箱；结构化预览无需执行此步骤。",
                book.title
            ),
        });
        cx.notify();
    }

    pub(super) fn close_group_modal(&mut self, cx: &mut Context<Self>) {
        if let Some(GroupModal::NameInput {
            pending_request, ..
        }) = self.group_modal.as_ref()
            && !name_input_modal_can_close(*pending_request)
        {
            return;
        }
        self.group_modal = None;
        cx.notify();
    }

    pub(super) fn confirm_group_modal(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.group_modal.clone() else {
            return;
        };
        match modal {
            GroupModal::NameInput {
                action,
                input,
                pending_request,
                ..
            } => {
                if pending_request.is_some() {
                    return;
                }
                let name = input.read(cx).value().trim().to_string();
                if name.is_empty() {
                    if let Some(GroupModal::NameInput { error, .. }) = self.group_modal.as_mut() {
                        *error = Some(
                            match action {
                                NameInputAction::CreateBook => "图书名称不能为空",
                                NameInputAction::CreateGroup(_)
                                | NameInputAction::RenameGroup(_) => "分组名称不能为空",
                            }
                            .to_string(),
                        );
                    }
                    cx.notify();
                    return;
                }
                match action {
                    NameInputAction::CreateBook => self.create_book(name.clone(), cx),
                    NameInputAction::CreateGroup(parent_id) => {
                        self.create_group(parent_id.clone(), name.clone(), cx);
                    }
                    NameInputAction::RenameGroup(group_id) => {
                        self.rename_group(group_id.clone(), name.clone(), cx);
                    }
                }
            }
            GroupModal::DeleteConfirm { group_id, .. } => {
                self.delete_group(group_id.clone(), cx);
                self.group_modal = None;
                cx.notify();
            }
            GroupModal::DeleteBook { book_id, .. } => {
                self.delete_book(book_id.clone(), cx);
                self.group_modal = None;
                cx.notify();
            }
            GroupModal::OfficeTrust { .. } => {
                // Office trust confirmation is handled by its button callback
                // because starting the preview also needs the active Window.
            }
            GroupModal::BookGroupPicker {
                book_id, selected, ..
            } => {
                let book_id = book_id.clone();
                // A group can disappear while the picker is open.
                let target = selected.filter(|group_id| self.library.group(group_id).is_some());
                let current = self
                    .library
                    .books()
                    .iter()
                    .find(|book| book.id == book_id)
                    .and_then(|book| book.group_id.clone());
                self.group_modal = None;
                cx.notify();
                if target != current {
                    self.move_book_to_group(book_id, target, cx);
                }
            }
        }
    }

    pub(super) fn delete_book(&mut self, book_id: String, cx: &mut Context<Self>) {
        let Some(book) = self
            .library
            .books()
            .iter()
            .find(|book| book.id == book_id)
            .cloned()
        else {
            return;
        };
        self.cancel_office_preview(Some(&book_id), false, cx);
        let Some(request_id) = self.begin_library_request("正在删除图书…", cx) else {
            return;
        };
        let services = Arc::clone(&self.services);
        let close_book_id = book_id.clone();
        let task = services.spawn_library_projected(move |library| library.remove_book(&book_id));
        cx.spawn(async move |view, cx| {
            let outcome = task.await;
            let deleted = outcome.as_ref().is_ok_and(|inner| inner.is_ok());
            let close_window = view.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(mutation)) => {
                        if this
                            .apply_shared_library_projection(mutation.generation, mutation.snapshot)
                        {
                            this.sync_ai_scope(cx);
                        }
                        if this.library_notice_request == Some(request_id) {
                            this.notice = Some(Notice {
                                text: format!("已删除图书「{}」", book.title),
                                error: false,
                            });
                        }
                        cx.notify();
                    }
                    Ok(Err(error)) => this.set_library_request_error(
                        request_id,
                        format!("删除图书失败：{error:#}"),
                        cx,
                    ),
                    Err(error) => this.set_library_request_error(
                        request_id,
                        format!("删除图书任务异常停止：{error}"),
                        cx,
                    ),
                }
                this.finish_library_request(cx)
            });
            if deleted {
                // Reader, PDF/Office preview and editor windows keep their own
                // copy of the store and their own child WebView, so removing a
                // book has to take those windows with it.
                let _ = cx.update(|app| close_book_windows(&close_book_id, app));
            }
            if let Ok(Some(window)) = close_window {
                schedule_library_window_removal(window, cx);
            }
        })
        .detach();
        cx.notify();
    }

    pub(super) fn sync_ai_scope(&mut self, cx: &mut Context<Self>) {
        if !should_sync_library_scope(self.library_mutations.close_requested()) {
            cx.notify();
            return;
        }
        let scope = library_ai_scope(&self.library, &self.group_filter);
        self.ai_sidebar
            .update(cx, |sidebar, cx| sidebar.set_scope(scope, cx));
        if should_start_library_search(self.library_mutations.close_requested(), &self.search_query)
        {
            self.start_search(cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn cancel_ai_for_window_close(&mut self, cx: &mut Context<Self>) {
        self.cancel_office_preview(None, false, cx);
        self.ai_controller.close();
        self.ai_sidebar.update(cx, |sidebar, cx| {
            sidebar.cancel_for_window_close(cx);
        });
    }
}
