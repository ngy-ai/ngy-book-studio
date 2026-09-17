//! Tests for launch planning, the bootstrap config and the directory move.

use super::launch::plan_launch_with;
use super::*;

fn write_config(path: &Path, data_dir: &Path) {
    save_remembered_data_dir(path, data_dir).expect("写入引导配置");
}

fn ready(plan: LaunchPlan) -> DataDirSelection {
    match plan {
        LaunchPlan::Ready(selection) => selection,
        LaunchPlan::NeedsSetup { reason, .. } => panic!("预期无需设置，实际：{reason:?}"),
    }
}

fn needs_setup(plan: LaunchPlan) -> (ReselectReason, PathBuf) {
    match plan {
        LaunchPlan::NeedsSetup { reason, suggestion } => (reason, suggestion),
        LaunchPlan::Ready(selection) => {
            panic!("预期需要设置，实际用了 {}", selection.data_dir.display())
        }
    }
}

#[test]
fn ensure_rejects_a_path_owned_by_a_file() {
    let temp = tempfile::tempdir().expect("临时目录");
    let file = temp.path().join("不是目录");
    fs::write(&file, b"x").expect("写文件");

    let error = ensure_data_dir_usable(&file).expect_err("文件不能当数据目录");
    assert!(
        format!("{error:#}").contains("无法创建数据目录"),
        "{error:#}"
    );
}

#[test]
fn ensure_leaves_no_probe_behind() {
    let temp = tempfile::tempdir().expect("临时目录");
    let data_dir = temp.path().join("书库");

    ensure_data_dir_usable(&data_dir).expect("目录可用");

    assert!(data_dir.is_dir());
    let entries: Vec<_> = fs::read_dir(&data_dir)
        .expect("读取目录")
        .map(|entry| entry.expect("目录项").file_name())
        .collect();
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn environment_override_wins_and_does_not_touch_the_config() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let remembered = temp.path().join("记住的");
    write_config(&config, &remembered);
    let overridden = temp.path().join("环境变量");

    let selection = ready(
        plan_launch_with(&config, Some(overridden.clone()), temp.path().join("建议"))
            .expect("环境变量优先"),
    );

    assert_eq!(selection.data_dir, overridden);
    assert_eq!(selection.source, DataDirSource::Environment);
    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读配置")
            .as_deref(),
        Some(remembered.as_path()),
        "环境变量不改写配置"
    );
}

#[test]
fn environment_override_must_be_usable() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let file = temp.path().join("文件");
    fs::write(&file, b"x").expect("写文件");

    let error = plan_launch_with(&config, Some(file), temp.path().join("建议"))
        .expect_err("环境变量指向文件时失败");
    assert!(format!("{error:#}").contains(DATA_DIR_ENV), "{error:#}");
}

#[test]
fn a_remembered_directory_needs_no_setup() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let remembered = temp.path().join("书库");
    fs::create_dir_all(&remembered).expect("创建书库");
    write_config(&config, &remembered);

    let selection =
        ready(plan_launch_with(&config, None, temp.path().join("建议")).expect("使用记住的目录"));

    assert_eq!(selection.data_dir, remembered);
    assert_eq!(selection.source, DataDirSource::Remembered);
}

#[test]
fn the_first_run_asks_for_setup_with_the_suggested_directory() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let suggested = temp.path().join("建议");

    let (reason, suggestion) =
        needs_setup(plan_launch_with(&config, None, suggested.clone()).expect("首次启动"));

    assert_eq!(reason, ReselectReason::FirstRun);
    assert_eq!(suggestion, suggested);
}

#[test]
fn an_unusable_remembered_directory_is_offered_back_for_editing() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let gone = temp.path().join("已拔出的移动盘");
    // 记住的是文件而不是目录：与“移动盘拔掉”在可用性上等价。
    fs::write(&gone, b"x").expect("写文件");
    write_config(&config, &gone);

    let (reason, suggestion) =
        needs_setup(plan_launch_with(&config, None, temp.path().join("建议")).expect("重新选择"));

    assert!(
        matches!(&reason, ReselectReason::Unusable(message) if message.contains("无法创建数据目录")),
        "{reason:?}"
    );
    assert_eq!(suggestion, gone, "上次的选择要作为输入框初值");
}

#[test]
fn an_unreadable_config_asks_again_with_the_reason() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    fs::write(&config, b"{ not json").expect("写坏配置");
    let suggested = temp.path().join("建议");

    let (reason, suggestion) =
        needs_setup(plan_launch_with(&config, None, suggested.clone()).expect("坏配置后重选"));

    assert!(
        matches!(&reason, ReselectReason::Unreadable(message) if message.contains("无法解析")),
        "{reason:?}"
    );
    assert_eq!(suggestion, suggested);
}

#[test]
fn confirming_a_directory_saves_it() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("nested").join("bootstrap.json");
    let chosen = temp.path().join("新书库");

    let selection = apply_data_dir(&config, &chosen).expect("确认新目录");

    assert_eq!(selection.data_dir, chosen);
    assert_eq!(selection.source, DataDirSource::Chosen);
    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读配置")
            .as_deref(),
        Some(chosen.as_path())
    );
}

#[test]
fn confirming_an_unusable_directory_reports_and_saves_nothing() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let blocked = temp.path().join("文件");
    fs::write(&blocked, b"x").expect("写文件");

    let error = apply_data_dir(&config, &blocked).expect_err("文件不能当数据目录");

    assert!(
        format!("{error:#}").contains("无法创建数据目录"),
        "{error:#}"
    );
    assert_eq!(load_remembered_data_dir(&config).expect("读配置"), None);
}

#[test]
fn forgetting_the_selection_removes_the_config() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    write_config(&config, &temp.path().join("书库"));

    forget_remembered_data_dir(&config).expect("清除记录");
    assert!(!config.exists());
    assert_eq!(load_remembered_data_dir(&config).expect("读配置"), None);
    forget_remembered_data_dir(&config).expect("重复清除不报错");
}

#[test]
fn saving_replaces_the_previous_selection_atomically() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    write_config(&config, &temp.path().join("旧书库"));
    let new_dir = temp.path().join("新书库");
    write_config(&config, &new_dir);

    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读配置")
            .as_deref(),
        Some(new_dir.as_path())
    );
    assert!(!config.with_extension("json.tmp").exists());
}

#[test]
fn input_text_is_trimmed_and_unquoted() {
    for (input, expected) in [
        (
            r#"  "C:\Users\学习资料\archives"  "#,
            r"C:\Users\学习资料\archives",
        ),
        (r#"“D:\墨页书库”"#, r"D:\墨页书库"),
        (r#"C:\墨页书库"#, r"C:\墨页书库"),
        ("  C:\\带 空格 的目录  ", "C:\\带 空格 的目录"),
    ] {
        assert_eq!(
            normalize_input_path(input),
            Some(PathBuf::from(expected)),
            "{input}"
        );
    }
    for blank in ["", "   ", "\"\"", "“”"] {
        assert_eq!(normalize_input_path(blank), None, "{blank}");
    }
}

// 文件 API 接受标准化的 `\\?\` 路径，native-dialog 的 Shell 解析会拒绝它，
// 所以只在对话框位置里转换前缀。
#[cfg(target_os = "windows")]
#[test]
fn shell_dialog_directory_converts_drive_and_unc_prefixes_only() {
    for (input, expected) in [
        (
            r"\\?\C:\Users\学习资料\archives",
            r"C:\Users\学习资料\archives",
        ),
        (
            r"\\?\UNC\server\share\learning\archives",
            r"\\server\share\learning\archives",
        ),
        (r"C:\Users\学习资料\archives", r"C:\Users\学习资料\archives"),
    ] {
        assert_eq!(
            shell_dialog_directory(Path::new(input)),
            PathBuf::from(expected)
        );
    }
}

// 首次启动时推荐的数据目录还没建出来，Shell 解析不存在的路径会让对话框直接打不开，
// 所以初始位置只取存在的目录，取不到就不设置。
#[test]
fn dialog_start_directory_uses_the_closest_existing_directory() {
    let temp = tempfile::tempdir().expect("临时目录");
    let missing = temp.path().join("还没建的推荐位置").join("data");

    assert_eq!(
        dialog_start_directory(Some(missing.as_path())),
        Some(temp.path().to_path_buf())
    );
    assert_eq!(
        dialog_start_directory(Some(temp.path())),
        Some(temp.path().to_path_buf())
    );
    assert_eq!(dialog_start_directory(Some(Path::new("相对/路径"))), None);
    assert_eq!(dialog_start_directory(None), None);
}

/// 旧书库的样子：库文件加对象存储。
fn seed_library(data_dir: &Path, marker: &[u8]) {
    fs::create_dir_all(data_dir.join("objects")).expect("建对象目录");
    fs::write(data_dir.join(crate::db::DATABASE_FILE), marker).expect("写库文件");
    fs::write(data_dir.join("objects").join("blob"), marker).expect("写对象");
}

#[test]
fn changing_the_data_directory_moves_the_old_contents_on_the_next_launch() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let new = temp.path().join("新书库");
    seed_library(&old, "旧库".as_bytes());
    write_config(&config, &old);

    apply_data_dir_change(&config, &new, &old, false).expect("记录更改");
    // 记录更改本身不搬数据：当前进程还占着旧库。
    assert!(old.join(crate::db::DATABASE_FILE).is_file());
    assert_eq!(
        load_pending_move(&config).expect("读待办"),
        Some(PendingMove {
            from: old.clone(),
            overwrite: false,
        })
    );

    assert_eq!(
        complete_pending_move(&config, &new).expect("搬迁"),
        Some(old.clone())
    );
    assert_eq!(
        fs::read(new.join(crate::db::DATABASE_FILE)).expect("读新库"),
        "旧库".as_bytes()
    );
    assert!(new.join("objects").join("blob").is_file());
    assert!(!old.exists(), "旧目录应被清空");
    assert_eq!(load_pending_move(&config).expect("读待办"), None);
    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读配置")
            .as_deref(),
        Some(new.as_path())
    );

    // 搬完再启动不该重复搬迁。
    assert_eq!(
        complete_pending_move(&config, &new).expect("再搬一次"),
        None
    );
}

#[test]
fn a_target_holding_a_library_needs_the_overwrite_permission() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let new = temp.path().join("新书库");
    seed_library(&old, "旧库".as_bytes());
    seed_library(&new, "别人的库".as_bytes());

    apply_data_dir_change(&config, &new, &old, false).expect("记录更改");
    let error = complete_pending_move(&config, &new).expect_err("没有覆盖授权时必须失败");
    assert!(
        format!("{error:#}").contains("已经是一个图书库"),
        "{error:#}"
    );
    // 两边都原样保留，记录也还在，下次启动可以重试。
    assert_eq!(
        fs::read(new.join(crate::db::DATABASE_FILE)).expect("读目标库"),
        "别人的库".as_bytes()
    );
    assert_eq!(
        fs::read(old.join(crate::db::DATABASE_FILE)).expect("读旧库"),
        "旧库".as_bytes()
    );
    assert!(load_pending_move(&config).expect("读待办").is_some());

    // 用户确认覆盖后重试。
    apply_data_dir_change(&config, &new, &old, true).expect("重新记录");
    complete_pending_move(&config, &new).expect("搬迁");
    assert_eq!(
        fs::read(new.join(crate::db::DATABASE_FILE)).expect("读新库"),
        "旧库".as_bytes()
    );
    assert!(!old.exists());
}

#[test]
fn a_target_holding_other_files_is_refused_and_the_source_stays() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let new = temp.path().join("新书库");
    seed_library(&old, "旧库".as_bytes());
    fs::create_dir_all(&new).expect("建新目录");
    fs::write(new.join("别人的文件.txt"), b"x").expect("写别的文件");

    apply_data_dir_change(&config, &new, &old, false).expect("记录更改");
    let error = complete_pending_move(&config, &new).expect_err("非空目录必须拒绝");
    assert!(format!("{error:#}").contains("不是空目录"), "{error:#}");
    assert!(
        old.join(crate::db::DATABASE_FILE).is_file(),
        "源目录不能被搬走"
    );
    assert!(new.join("别人的文件.txt").is_file());
    assert!(load_pending_move(&config).expect("读待办").is_some());
}

#[test]
fn a_vanished_source_directory_finishes_the_move() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let new = temp.path().join("新书库");
    seed_library(&old, "旧库".as_bytes());
    write_config(&config, &old);
    apply_data_dir_change(&config, &new, &old, false).expect("记录更改");

    fs::remove_dir_all(&old).expect("用户自己删掉了旧目录");

    assert_eq!(
        complete_pending_move(&config, &new).expect("搬迁"),
        Some(old.clone())
    );
    assert_eq!(load_pending_move(&config).expect("读待办"), None);
}

#[test]
fn changing_the_directory_back_to_the_source_cancels_the_move() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let new = temp.path().join("新书库");
    seed_library(&old, "旧库".as_bytes());
    write_config(&config, &old);
    apply_data_dir_change(&config, &new, &old, false).expect("记录更改");

    // 改回原目录：没有东西要搬，待办要清掉，否则会要求目标目录为空。
    apply_data_dir(&config, &old).expect("改回原目录");
    assert_eq!(load_pending_move(&config).expect("读待办"), None);
    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读配置")
            .as_deref(),
        Some(old.as_path())
    );
}

#[test]
fn a_pending_move_follows_another_directory_change() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let blocked = temp.path().join("放不下");
    let another = temp.path().join("换个位置");
    seed_library(&old, "旧库".as_bytes());
    write_config(&config, &old);
    apply_data_dir_change(&config, &blocked, &old, false).expect("记录更改");

    // 搬迁还没做完用户又选了别处：源目录要跟着走，否则旧数据会失去线索。
    apply_data_dir(&config, &another).expect("换个目录");
    assert_eq!(
        load_pending_move(&config).expect("读待办"),
        Some(PendingMove {
            from: old,
            overwrite: false,
        })
    );
    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读配置")
            .as_deref(),
        Some(another.as_path())
    );
}

// 坏掉的配置里可能还留着未完成搬迁的线索，只是解析不出来。静默覆盖会把线索永久抹掉；
// 但为此让写配置失败会卡死启动（错误来自配置文件，用户改路径也修不好）。所以把原始
// 字节备份到旁边，再照常写新配置：证据留在磁盘上，用户能继续用。每次损坏各留一份。
#[test]
fn a_corrupt_config_is_archived_beside_the_new_one_instead_of_blocking_startup() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let blocked = temp.path().join("放不下");
    let another = temp.path().join("另一个目录");
    seed_library(&old, "旧库".as_bytes());
    apply_data_dir_change(&config, &blocked, &old, false).expect("记录更改");
    // 记录本来在配置里，随后文件坏掉（写一半被中断，或被手工改坏）。
    let mut first_broken = fs::read(&config).expect("读配置");
    first_broken.extend_from_slice("\n{ 被改坏了".as_bytes());
    fs::write(&config, &first_broken).expect("写坏配置");
    assert!(load_pending_move(&config).is_err(), "坏配置读不出待办");

    // 坏配置不再让保存失败：启动能继续，用户换个目录重试就能通过。
    save_remembered_data_dir(&config, &another).expect("坏配置要备份而不是阻塞启动");

    assert_eq!(
        load_remembered_data_dir(&config)
            .expect("读新配置")
            .as_deref(),
        Some(another.as_path())
    );
    assert_eq!(
        load_pending_move(&config).expect("读新配置里的待办"),
        None,
        "坏文件里的待办恢复不了，新配置没有待办"
    );
    let first_backup = config.with_file_name("bootstrap.json.corrupt");
    assert_eq!(
        fs::read(&first_backup).expect("读备份"),
        first_broken,
        "原始字节要逐字节留在备份里"
    );

    // 再坏一次：新配置被改坏后重存，前一份备份不能被盖掉。
    let mut second_broken = fs::read(&config).expect("读配置");
    second_broken.extend_from_slice("\n{ 又坏了".as_bytes());
    fs::write(&config, &second_broken).expect("再次写坏配置");
    save_remembered_data_dir(&config, &blocked).expect("第二次坏配置也要能继续");

    assert_eq!(
        fs::read(&first_backup).expect("读第一份备份"),
        first_broken,
        "第一次损坏的备份不能被第二次覆盖"
    );
    assert_eq!(
        fs::read(config.with_file_name("bootstrap.json.corrupt-2")).expect("读第二份备份"),
        second_broken,
        "第二次损坏的原始字节也要留一份"
    );
}

#[test]
fn a_move_is_skipped_when_this_launch_uses_another_directory() {
    let temp = tempfile::tempdir().expect("临时目录");
    let config = temp.path().join("bootstrap.json");
    let old = temp.path().join("旧书库");
    let new = temp.path().join("新书库");
    let isolated = temp.path().join("隔离目录");
    seed_library(&old, "旧库".as_bytes());
    write_config(&config, &old);
    apply_data_dir_change(&config, &new, &old, false).expect("记录更改");

    // 开发构建的 NGY_DATA_DIR 会用别的目录启动：不能把配置里的真实书库搬到那去。
    assert_eq!(
        complete_pending_move(&config, &isolated).expect("搬迁"),
        None
    );
    assert!(
        old.join(crate::db::DATABASE_FILE).is_file(),
        "源目录不能被搬走"
    );
    assert!(load_pending_move(&config).expect("读待办").is_some());
}
