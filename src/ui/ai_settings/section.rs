//! The sections of the AI settings window.

use super::*;

/// 五个设置窗口的身份。菜单项、窗口标题、应用级单例登记和调试选择器都以它为准。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiSettingsSection {
    Endpoint,
    Models,
    WebSearch,
    BackgroundJobs,
    System,
}

impl AiSettingsSection {
    /// 菜单里的展示顺序。
    pub(crate) const ALL: [Self; 5] = [
        Self::Endpoint,
        Self::Models,
        Self::WebSearch,
        Self::BackgroundJobs,
        Self::System,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint",
            Self::Models => "模型设置",
            Self::WebSearch => "联网搜索",
            Self::BackgroundJobs => "后台任务",
            Self::System => "系统配置",
        }
    }

    /// 单例键、app_id 与调试选择器串里用的短名。
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::Endpoint => "endpoint",
            Self::Models => "models",
            Self::WebSearch => "web-search",
            Self::BackgroundJobs => "background-jobs",
            Self::System => "system",
        }
    }

    /// 窗口副标题：一句话说明这个窗口负责什么。
    pub(crate) fn subtitle(self) -> &'static str {
        match self {
            Self::Endpoint => "连接、密钥与远程授权；每个 Endpoint 独立保存。",
            Self::Models => {
                "问答、翻译、向量与视觉模型各自选择 Endpoint 与模型；问答模型另带生成参数。"
            }
            Self::WebSearch => "书中检索不到时的可选联网补充。",
            Self::BackgroundJobs => "新任务的运行方式与调度节奏。",
            Self::System => "界面外观、阅读偏好、翻译默认与存储位置。",
        }
    }

    /// 标题栏右侧的一句边界说明。
    pub(crate) fn header_note(self) -> &'static str {
        match self {
            Self::Endpoint => "墨页不会启动、停止或下载 Ollama 模型",
            Self::Models => "检测只读取模型列表，不改动服务端",
            Self::WebSearch => "默认关闭，关闭时不联网",
            Self::BackgroundJobs => "调度在这里；运行明细在图书库工具栏的「后台任务」窗口",
            Self::System => "只影响本机体验，不改动图书内容",
        }
    }

    /// 提交成功后的提示。每个窗口保存的只是它自己那一块。
    pub(crate) fn success_notice(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint 设置已保存；新的请求会使用新的连接与密钥。",
            Self::Models => "模型设置已保存；新的问答、翻译与检索请求会使用它们。",
            Self::WebSearch => "联网搜索设置已保存；新的搜索请求会使用它们。",
            Self::BackgroundJobs => {
                "后台任务设置已保存；调度立即对新任务生效，正在运行的任务不受影响。"
            }
            Self::System => "系统配置已保存，并已应用到已经打开的窗口。",
        }
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            Self::Endpoint => IconName::Settings2,
            Self::Models => IconName::Bot,
            Self::WebSearch => IconName::Globe,
            Self::BackgroundJobs => IconName::LoaderCircle,
            Self::System => IconName::Palette,
        }
    }

    pub(crate) fn window_title(self) -> String {
        format!("墨页 · AI 设置 · {}", self.label())
    }

    pub(crate) fn app_id(self) -> String {
        format!("dev.ngy.book-studio.ai-settings.{}", self.slug())
    }

    pub(crate) fn window_size(self) -> gpui_kit::Size<gpui_kit::Pixels> {
        match self {
            Self::Endpoint => size(px(760.), px(820.)),
            Self::Models => size(px(820.), px(880.)),
            Self::WebSearch => size(px(760.), px(860.)),
            Self::BackgroundJobs => size(px(720.), px(620.)),
            Self::System => size(px(800.), px(900.)),
        }
    }

    pub(crate) fn window_min_size(self) -> gpui_kit::Size<gpui_kit::Pixels> {
        size(px(560.), px(440.))
    }

    /// 本窗口内的调试选择器：`ai-<窗口>-<name>`。
    pub(crate) fn selector(self, name: &str) -> String {
        format!("ai-{}-{name}", self.slug())
    }

    /// [`Self::selector`] 的 `debug_selector` 形态。
    pub(crate) fn debug_selector(self, name: &str) -> impl Fn() -> String + 'static {
        let selector = self.selector(name);
        move || selector.clone()
    }
}
