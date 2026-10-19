//! Integration tests of the parallel walk: the in-flight block window and
//! reconfiguration.

use super::*;

use std::{
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

use crate::db;

/// 块级并行的主用例：一次整本翻译同时开 `任务并发` 个块请求。
///
/// 现场（2026-09-15，「任务并发」改成 8、只开一本翻译）：窗口里的「处理中」恒为 1，
/// 因为并发只决定几个任务在跑，任务内部仍逐块串行。这条用例同时固定三件事：模型真的
/// 被并发调用、窗口写进了持久游标（任务窗口就靠它显示处理中）、游标永不越过还在等的块。
mod chapter_jump_test;

mod endpoint_test;

mod execution_test;

mod failure_test;

mod narrowing_test;

mod window_test;
