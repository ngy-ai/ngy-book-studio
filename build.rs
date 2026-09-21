//! 把 `assets/icon.ico` 嵌进可执行文件（模板，复制到项目根目录用）。
//!
//! 需要先在 `Cargo.toml` 里加：
//!
//! ```toml
//! [build-dependencies]
//! winresource = "0.1"
//! ```
//!
//! 要改的只有 `VERSIONINFO` 那几个字符串和图标路径。
//!
//! Windows 的窗口与任务栏图标取自 exe 的资源段——gpui 的 `WindowOptions::icon`
//! 只对 X11 生效，所以只能在构建这一层嵌。
//!
//! 嵌入失败只发 warning、不中断构建：图标是装饰，不该因为机器上找不到 `rc.exe`
//! 就让整个项目编译不过。但也不能装作没事，warning 会出现在构建输出里。

// 只在 Windows 上用到；放到外面会在别的平台变成未使用导入。
#[cfg(windows)]
use std::path::{Path, PathBuf};

fn main() {
    // 监视整个 assets 目录，**不要**只写 assets/icon.ico。
    //
    // 踩过的坑：监视一个当时不存在的路径时，cargo 把「文件缺失」记进指纹，
    // 之后图标生成出来了也不会重跑这个脚本——表现是图标永远不出现，而构建输出里
    // 只有那条旧警告（cargo 重放上次的 warning），`cargo build` 一秒内 Finished。
    // 改成监视目录后，目录里新增文件也会触发重跑。
    println!("cargo:rerun-if-changed=assets");

    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    let icon = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("icon.ico");

    if !icon.is_file() {
        println!(
            "cargo:warning=找不到 {}，跳过嵌入图标（先跑 tools/make_icon.py 生成）",
            icon.display()
        );
        return;
    }

    // 版本号与描述都跟着 Cargo.toml 走，别在这儿写死一份会过期的东西。
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());
    let name = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "app".to_string());
    let description =
        std::env::var("CARGO_PKG_DESCRIPTION").unwrap_or_else(|_| name.clone());

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(&icon.to_string_lossy());
    // 这几个字符串会出现在「属性 → 详细信息」里。中文没问题：winresource 生成的
    // .rc 自带 `#pragma code_page(65001)`，不用为了保险退回英文。
    resource.set("FileDescription", &description);
    resource.set("ProductName", &name);
    resource.set("FileVersion", &format!("{version}.0"));
    resource.set("ProductVersion", &format!("{version}.0"));

    let Some(rc_dir) = find_rc_dir() else {
        println!(
            "cargo:warning=没找到 Windows SDK 里的 rc.exe，跳过嵌入图标；\
             设 RC_PATH 指向 rc.exe 可以绕过自动查找"
        );
        return;
    };
    resource.set_toolkit_path(&rc_dir.to_string_lossy());

    if let Err(error) = resource.compile() {
        println!("cargo:warning=嵌入图标失败，可执行文件将没有图标：{error}");
    }
}

/// 找到装着 `rc.exe` 的那个目录。
///
/// 不走 winresource 默认那条路（它调 `reg.exe` 查注册表拿 SDK 根目录）：
/// 直接扫 `Windows Kits\10\bin` 下的版本目录，取版本号最大的一个。
/// 这样不依赖注册表，也不会碰上被安全策略拦下的程序。
#[cfg(windows)]
fn find_rc_dir() -> Option<PathBuf> {
    // 用户显式指定了就照办。
    if let Some(rc) = std::env::var_os("RC_PATH") {
        return PathBuf::from(rc).parent().map(Path::to_path_buf);
    }

    const ROOTS: [&str; 2] = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];

    for root in ROOTS {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        // 目录名形如 10.0.26100.0，字典序就是版本序。
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| dir.join("x64").join("rc.exe").is_file())
            .collect();
        versions.sort();
        if let Some(latest) = versions.pop() {
            return Some(latest.join("x64"));
        }
    }

    None
}
