//! Shell paths handed to the native directory picker.

use super::*;

/// 文件 API 接受标准化的 `\\?\` 路径，而 native-dialog 使用的 Shell 解析会拒绝它。
/// 只转换对话框里显示的位置，存储路径保持原样。
pub fn shell_dialog_directory(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            let mut directory = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
                Prefix::VerbatimUNC(server, share) => PathBuf::from(r"\\").join(server).join(share),
                _ => return path.to_path_buf(),
            };
            directory.extend(components);
            return directory;
        }
    }
    path.to_path_buf()
}

/// 系统目录选择器的初始位置。
///
/// native-dialog 把这里的路径交给 Windows Shell 解析（`SHCreateItemFromParsingName`），
/// 路径不存在时会直接失败并让对话框打不开 —— 而首次启动时推荐的数据目录恰恰还没建出来。
/// 所以只交出确实存在的目录：输入路径存在就用它，否则沿父目录上溯到最近的存在目录；
/// 都找不到（相对路径、含非法字符）时返回 `None`，让对话框用系统默认位置。
pub fn dialog_start_directory(path: Option<&Path>) -> Option<PathBuf> {
    let mut candidate = path?;
    loop {
        if candidate.is_absolute() && candidate.is_dir() {
            return Some(shell_dialog_directory(candidate));
        }
        candidate = candidate.parent()?;
    }
}
