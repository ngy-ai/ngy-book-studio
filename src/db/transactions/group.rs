//! Group creation, renaming, tree deletion and book assignment.

use super::*;

pub(crate) fn create_group(
    conn: &mut Connection,
    group: &BookGroup,
    max_depth: usize,
) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分组创建事务")?;
    if let Some(parent_id) = group.parent_id.as_deref() {
        let depth = groups::depth(&tx, parent_id, max_depth)?.context("父分组不存在")?;
        if depth >= max_depth {
            bail!("分组最多支持 {max_depth} 级");
        }
    }
    if groups::sibling_name_exists(&tx, None, &group.name, group.parent_id.as_deref())? {
        bail!("同级下已存在同名分组");
    }
    groups::insert(&tx, group)?;
    tx.commit().context("无法提交分组创建事务")?;
    Ok(())
}

pub(crate) fn rename_group(conn: &mut Connection, group_id: &str, name: &str) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分组重命名事务")?;
    let parent_id = groups::parent_id(&tx, group_id)?.context("分组不存在")?;
    if groups::sibling_name_exists(&tx, Some(group_id), name, parent_id.as_deref())? {
        bail!("同级下已存在同名分组");
    }
    if groups::rename(&tx, group_id, name)? == 0 {
        bail!("分组不存在");
    }
    tx.commit().context("无法提交分组重命名事务")?;
    Ok(())
}

pub(crate) fn delete_group_tree(conn: &mut Connection, root_group_id: &str) -> Result<Vec<String>> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("无法启动分组删除事务")?;
    let removed = groups::subtree_ids(&tx, root_group_id)?;
    if removed.is_empty() {
        bail!("分组不存在");
    }
    books::clear_groups_in_subtree(&tx, root_group_id)?;
    groups::delete_subtree(&tx, root_group_id)?;
    tx.commit().context("无法提交分组删除事务")?;
    Ok(removed)
}

pub(crate) fn set_book_group(
    conn: &Connection,
    book_id: &str,
    group_id: Option<&str>,
) -> Result<()> {
    if books::set_group(conn, book_id, group_id)? != 0 {
        return Ok(());
    }
    if !books::exists(conn, book_id)? {
        bail!("图书不存在");
    }
    bail!("分组不存在");
}
