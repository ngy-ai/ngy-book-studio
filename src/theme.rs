//! 全局界面外观：浅色与深色两套皮肤。
//!
//! 这里只放"颜色是什么"和"当前用哪一套"，不碰任何布局或窗口逻辑。界面代码读
//! [`palette`] 取色，而不是各自写死 `rgb(0x...)`：两套皮肤的可读性由这一份数据
//! 负责，各窗口只管用语义名（`paper` / `ink` / `border`…）表达意图。
//!
//! 三件事必须同时成立，否则界面会出现"一半深一半浅"：
//!
//! 1. 项目自己的调色板（本模块的 [`Palette`]）——所有自绘元素读它；
//! 2. `gpui-component` 的语义色（按钮、输入框、Tab、滚动条、菜单）——由
//!    [`set_appearance`] 在切换后按同一调色板写入，所以第三方组件与自绘元素同色系；
//! 3. WebView 里的正文（阅读器章节、富文本编辑器、PDF 阅读器）——通过
//!    [`Palette::css_variables`] 把同一份颜色转成 CSS 自定义属性下发。
//!
//! WebView 不能读 Rust 常量，所以第 3 条是"同一份数据、两种表达"，不是第二套配色。
//!
//! 外观偏好存在图书库的 `settings` 表里（见 `services::ProviderSettings::appearance`），
//! 所以它是**应用级**偏好而不是某个窗口的：`set_appearance` 结束时刷新全部窗口。

use gpui::{App, Global, rgb};
use serde::{Deserialize, Serialize};

/// 界面皮肤。
///
/// 只提供明确的两套，不提供"跟随系统"：GPUI 在窗口创建时读取一次系统外观，之后
/// 不再通知，跟随系统会在用户改系统设置后停留在旧皮肤上而不自知。写死枚举让
/// "现在是什么皮肤"永远有一个确定答案。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

impl Appearance {
    /// 设置窗口里按这个顺序展示。
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// 存进设置表、写进 WebView 文档标识的稳定名字。
    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// 界面上给用户看的中文名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }

    /// CSS `color-scheme` 的取值，决定 WebView 里原生控件（滚动条、表单控件、
    /// 未指定颜色的正文）走哪套浏览器默认样式。
    pub fn color_scheme(self) -> &'static str {
        self.name()
    }
}

/// 一套皮肤的全部颜色。
///
/// 字段用 `u32` 而不是 `Hsla`：同一份颜色还要以 `#rrggbb` 的形式写进 CSS 变量，
/// 十六进制是 WebView 与 GPUI 都无损接受的公共表示，`rgb(u32)` 直接可用。
///
/// 语义命名对应的是"用在哪里"，不是"长什么样"，所以两套皮肤可以给出完全不同的
/// 色相而调用点一行都不用改。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// 窗口与页面底色，界面里面积最大的一层。
    pub paper: u32,
    /// 抬升的表面：卡片、浮层、输入框、状态栏。
    pub surface: u32,
    /// 侧栏与次级面板底色，用于和正文区拉开层次。
    pub sidebar: u32,
    /// 正文文字。
    pub ink: u32,
    /// 次要文字：说明、元信息、时间戳、禁用态。
    pub muted: u32,
    /// 分隔线与边框。
    pub border: u32,
    /// 强调色本身：图标、边框、进度、选中态的描边。
    pub accent: u32,
    /// 强调色上的文字与图标。它是 `accent_soft` 的背景上的前景，必须比
    /// [`Palette::accent`] 更靠近文字色一端，否则浅底浅字或深底深字都读不出来。
    pub accent_strong: u32,
    /// 强调色的柔和背景：选中行、标签、提示条。
    pub accent_soft: u32,
    /// 失败与危险操作。
    pub danger: u32,
    /// 失败提示的柔和背景。
    pub danger_soft: u32,
    /// 需要用户注意但不算失败（暂停、警告、覆盖确认）。
    pub warning: u32,
    pub warning_soft: u32,
    /// 完成与成功。
    pub success: u32,
    pub success_soft: u32,
    /// 中性状态：排队中、未开始、无归属。
    pub neutral: u32,
    pub neutral_soft: u32,
    /// 信息提示。
    pub info: u32,
    pub info_soft: u32,
    /// 浮层遮罩的基色，调用方自己叠 alpha（见 [`Palette::scrim_with_alpha`]）。
    pub scrim: u32,
}

impl Palette {
    /// 浅色：米白纸感，与“墨页”的阅读气质一致。
    ///
    /// 所有会承载文字的颜色都按 WCAG AA 的 4.5:1 校过（`theme::tests` 逐个断言）：
    /// `ink` 在 `paper` 上是 13:1，`muted` 是 5.1:1。`accent` 比初版略深，是因为
    /// 它同时是链接色与图标色——偏亮一档就会让正文里的链接掉到 4.1:1。
    pub const LIGHT: Self = Self {
        paper: 0xf7f5f0,
        surface: 0xfffefa,
        sidebar: 0xf1eee8,
        ink: 0x292621,
        muted: 0x6f6860,
        border: 0xe1ddd5,
        accent: 0xb05538,
        accent_strong: 0x9a4a30,
        accent_soft: 0xf1ddd5,
        danger: 0xb33a2b,
        danger_soft: 0xf8e2de,
        warning: 0x8a5a20,
        warning_soft: 0xf4e8d5,
        success: 0x376441,
        success_soft: 0xe3efe5,
        neutral: 0x5e6572,
        neutral_soft: 0xe7e9ed,
        info: 0x2f5b85,
        info_soft: 0xe9f1fa,
        scrim: 0x1c1a17,
    };

    /// 深色：暖黑纸感，保留浅色的色相关系（强调色仍是砖红，不是纯蓝）。
    ///
    /// 深色不是浅色的取反：`accent_strong` 必须变亮而不是变暗，因为它的职责是
    /// "在柔和强调底上可读"，底变暗后前景就得往亮处走。正文对比度约 14:1，
    /// 次要文字约 6.4:1。
    pub const DARK: Self = Self {
        paper: 0x1c1a17,
        surface: 0x252220,
        sidebar: 0x181613,
        ink: 0xe9e4db,
        muted: 0xa69d92,
        border: 0x3b3630,
        accent: 0xd07a57,
        accent_strong: 0xe9a183,
        accent_soft: 0x3b2a23,
        danger: 0xe0705f,
        // 比"看着合适"更深一档：失败色压在柔和底上必须过 4.5:1，否则失败标签
        // 在深色皮肤里会糊掉。
        danger_soft: 0x2e1a17,
        warning: 0xd9a55c,
        warning_soft: 0x3a2e1c,
        success: 0x7fb08a,
        success_soft: 0x223026,
        neutral: 0xb3b9c4,
        neutral_soft: 0x2c2f36,
        info: 0x8fb6e0,
        info_soft: 0x222c38,
        scrim: 0x000000,
    };

    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::LIGHT,
            Appearance::Dark => Self::DARK,
        }
    }

    /// `#rrggbb`，供 CSS 使用。
    pub fn hex(value: u32) -> String {
        format!("#{value:06x}")
    }

    /// 给任意皮肤色叠上透明度。
    ///
    /// 半透明色必须由皮肤给出基色：写死的 `rgba(0x2926210a)` 在深色皮肤下是"深色
    /// 叠深色"，hover 与遮罩会彻底看不见。基色取语义色（`ink`、`surface`、`danger`…），
    /// 透明度取调用方的意图，两套皮肤因此得到同一层关系而不是同一串数字。
    pub fn tint(value: u32, alpha: f32) -> gpui::Rgba {
        let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
        gpui::rgba((value << 8) | alpha)
    }

    /// 浮层遮罩的缩放：两套皮肤都要压暗背景，浅色下用深色压，深色下用纯黑压。
    pub fn scrim_with_alpha(self, alpha: f32) -> gpui::Rgba {
        Self::tint(self.scrim, alpha)
    }

    /// 文本选中的底色。
    ///
    /// 不能用不透明的皮肤色：`accent_soft` 在深色皮肤里和 `surface`（回答卡片、
    /// 输入框）只差一档明度，选中区域几乎看不出来；浅色皮肤里它又太浅，被选中
    /// 的文字仍像没选中。叠了 alpha 的强调色落在 `paper` / `surface` / `sidebar`
    /// 三种底色上都读得出来，而且选中不改文字颜色，所以文字仍与其自己的底色
    /// 保持原来的对比度。GPUI 在绘制文字之前先填这个矩形，透明度会与卡片底色
    /// 正常合成。
    pub fn selection(self) -> gpui::Rgba {
        Self::tint(self.accent, 0.4)
    }

    /// 皮肤色的 `(CSS 自定义属性名, 取值)` 列表。
    ///
    /// WebView 的下发有两条路径（阅读器注入样式表、编辑器与 PDF 阅读器的运行时
    /// setter），两条路径都读这份列表：变量名只在这里出现一次，改名不会只改到一半。
    pub fn variables(self) -> [(&'static str, u32); 19] {
        [
            ("--ngy-paper", self.paper),
            ("--ngy-surface", self.surface),
            ("--ngy-sidebar", self.sidebar),
            ("--ngy-text", self.ink),
            ("--ngy-muted", self.muted),
            ("--ngy-border", self.border),
            ("--ngy-accent", self.accent),
            ("--ngy-accent-strong", self.accent_strong),
            ("--ngy-accent-soft", self.accent_soft),
            ("--ngy-danger", self.danger),
            ("--ngy-danger-soft", self.danger_soft),
            ("--ngy-warning", self.warning),
            ("--ngy-warning-soft", self.warning_soft),
            ("--ngy-success", self.success),
            ("--ngy-success-soft", self.success_soft),
            ("--ngy-neutral", self.neutral),
            ("--ngy-neutral-soft", self.neutral_soft),
            ("--ngy-info", self.info),
            ("--ngy-info-soft", self.info_soft),
        ]
    }

    /// 下发给 WebView 的 CSS 自定义属性块（不含选择器与花括号）。
    pub fn css_variables(self) -> String {
        let mut css = String::new();
        for (name, value) in self.variables() {
            css.push_str(name);
            css.push_str(": ");
            css.push_str(&Self::hex(value));
            css.push_str("; ");
        }
        css
    }

    /// `(r, g, b, a)`。WebView 的原生底色要的是字节元组而不是 `Rgba`；窗口还没
    /// 画出第一帧时显示的就是它，所以它必须已经是对应皮肤的颜色。
    pub fn bytes(value: u32) -> (u8, u8, u8, u8) {
        (
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
            0xff,
        )
    }

    /// 写成一段把整份调色板写进 `root` 的 JS（`root` 由调用方在片段里绑定）。
    ///
    /// 运行时改色只能逐个 `setProperty`：WebView 的文档是第三方 EPUB 章节或我们
    /// 自己生成的 XHTML，替换整个 `style` 属性会连带清掉页面上别处写的内联样式。
    pub fn css_variable_setters(self) -> String {
        let mut script = String::new();
        for (name, value) in self.variables() {
            script.push_str("root.style.setProperty('");
            script.push_str(name);
            script.push_str("', '");
            script.push_str(&Self::hex(value));
            script.push_str("'); ");
        }
        script
    }
}

/// 当前皮肤。它是 [`Palette`] 的唯一来源，界面代码不要各存一份。
#[derive(Clone, Copy, Debug, Default)]
pub struct AppTheme {
    pub appearance: Appearance,
}

impl Global for AppTheme {}

/// 当前皮肤。没有全局状态时按浅色处理：GPUI 的 `TestAppContext` 与数据目录设置
/// 窗口都可能在任何窗口建立全局之前渲染，取色不能因此 panic。
pub fn appearance(cx: &App) -> Appearance {
    cx.try_global::<AppTheme>()
        .map(|theme| theme.appearance)
        .unwrap_or_default()
}

/// 当前皮肤的全部颜色。
pub fn palette(cx: &App) -> Palette {
    Palette::for_appearance(appearance(cx))
}

/// 切换皮肤并让所有窗口立即生效。
///
/// 三个副作用都必须做完：记住新皮肤（[`palette`] 的读数）、把同一套颜色写进
/// `gpui-component` 的语义色（否则按钮和滚动条会留在旧皮肤上）、刷新全部窗口
/// （否则已经画好的界面要等下一次重绘才变）。WebView 里的三个面不在这里刷新 ——
/// 它们由各自的窗口在自己的 `apply_theme` 里下发，因为只有窗口知道自己的 WebView
/// 是否已经建好。
pub fn set_appearance(appearance: Appearance, cx: &mut App) {
    if cx.has_global::<AppTheme>() {
        cx.global_mut::<AppTheme>().appearance = appearance;
    } else {
        cx.set_global(AppTheme { appearance });
    }
    sync_component_theme(appearance, cx);
    cx.refresh_windows();
}

/// 把当前皮肤写进 `gpui-component` 的语义色。
///
/// `Theme::change` 会先按内置主题全量覆盖 `ThemeColor`，所以必须在它之后写，顺序
/// 反了就被内置配色盖掉。`ThemeRegistry` 未初始化时（测试里没有调
/// `gpui_component::init`）整个同步跳过：那时 `Theme::change` 会自己去读 registry
/// 并 panic，而皮肤本身仍然可用。
fn sync_component_theme(appearance: Appearance, cx: &mut App) {
    if !cx.has_global::<gpui_component::ThemeRegistry>() {
        return;
    }
    let mode = match appearance {
        Appearance::Light => gpui_component::ThemeMode::Light,
        Appearance::Dark => gpui_component::ThemeMode::Dark,
    };
    gpui_component::Theme::change(mode, None, cx);

    let palette = Palette::for_appearance(appearance);
    let to_hsla = |value: u32| gpui::Hsla::from(rgb(value));
    let theme = gpui_component::Theme::global_mut(cx);
    theme.mode = mode;
    theme.background = to_hsla(palette.paper);
    theme.foreground = to_hsla(palette.ink);
    theme.border = to_hsla(palette.border);
    theme.input = to_hsla(palette.border);
    theme.ring = to_hsla(palette.accent);
    theme.caret = to_hsla(palette.accent);
    theme.overlay = to_hsla(palette.scrim);
    theme.popover = to_hsla(palette.surface);
    theme.popover_foreground = to_hsla(palette.ink);
    theme.primary = to_hsla(palette.accent);
    theme.primary_hover = to_hsla(palette.accent_strong);
    theme.primary_active = to_hsla(palette.accent_strong);
    // 实心按钮上的文字用 `paper` 而不是 `surface`：两套皮肤下它都比 `surface`
    // 离 `accent` 更远，浅色下 surface 只有 4.5:1 的边缘值。
    theme.primary_foreground = to_hsla(palette.paper);
    theme.accent = to_hsla(palette.accent_soft);
    theme.accent_foreground = to_hsla(palette.accent_strong);
    theme.secondary = to_hsla(palette.surface);
    theme.secondary_hover = to_hsla(palette.accent_soft);
    theme.secondary_active = to_hsla(palette.accent_soft);
    theme.secondary_foreground = to_hsla(palette.ink);
    theme.muted = to_hsla(palette.neutral_soft);
    theme.muted_foreground = to_hsla(palette.muted);
    theme.danger = to_hsla(palette.danger);
    theme.danger_foreground = to_hsla(palette.surface);
    theme.success = to_hsla(palette.success);
    theme.success_foreground = to_hsla(palette.surface);
    theme.warning = to_hsla(palette.warning);
    theme.warning_foreground = to_hsla(palette.surface);
    theme.info = to_hsla(palette.info);
    theme.info_foreground = to_hsla(palette.surface);
    theme.link = to_hsla(palette.accent_strong);
    theme.link_hover = to_hsla(palette.accent);
    // 选中底色是半透明的强调色，不是 `accent_soft`：不透明色在深色卡片上
    // （AI 回答、输入框）看不出选中范围，理由见 `Palette::selection`。
    theme.selection = palette.selection().into();
    theme.sidebar = to_hsla(palette.sidebar);
    theme.sidebar_foreground = to_hsla(palette.ink);
    theme.sidebar_border = to_hsla(palette.border);
    theme.sidebar_accent = to_hsla(palette.accent_soft);
    theme.sidebar_accent_foreground = to_hsla(palette.accent_strong);
    theme.sidebar_primary = to_hsla(palette.accent);
    theme.sidebar_primary_foreground = to_hsla(palette.surface);
    theme.list = to_hsla(palette.paper);
    theme.list_active = to_hsla(palette.accent_soft);
    theme.list_active_border = to_hsla(palette.accent);
    theme.list_hover = to_hsla(palette.sidebar);
    theme.list_even = to_hsla(palette.paper);
    theme.list_head = to_hsla(palette.sidebar);
    theme.tab = to_hsla(palette.paper);
    theme.tab_active = to_hsla(palette.accent_soft);
    theme.tab_active_foreground = to_hsla(palette.accent_strong);
    theme.tab_bar = to_hsla(palette.sidebar);
    theme.tab_bar_segmented = to_hsla(palette.sidebar);
    theme.tab_foreground = to_hsla(palette.muted);
    theme.table = to_hsla(palette.paper);
    theme.table_head = to_hsla(palette.sidebar);
    theme.table_head_foreground = to_hsla(palette.ink);
    theme.table_row_border = to_hsla(palette.border);
    theme.table_hover = to_hsla(palette.sidebar);
    theme.table_even = to_hsla(palette.paper);
    theme.group_box = to_hsla(palette.surface);
    theme.group_box_foreground = to_hsla(palette.ink);
    theme.description_list_label = to_hsla(palette.sidebar);
    theme.description_list_label_foreground = to_hsla(palette.muted);
    theme.scrollbar = to_hsla(palette.paper);
    theme.scrollbar_thumb = to_hsla(palette.border);
    theme.scrollbar_thumb_hover = to_hsla(palette.muted);
    theme.skeleton = to_hsla(palette.neutral_soft);
    theme.switch = to_hsla(palette.neutral_soft);
    theme.title_bar = to_hsla(palette.sidebar);
    theme.title_bar_border = to_hsla(palette.border);
    theme.tiles = to_hsla(palette.surface);
    theme.progress_bar = to_hsla(palette.accent_soft);
    theme.drop_target = to_hsla(palette.accent_soft);
    theme.drag_border = to_hsla(palette.accent);
    theme.accordion = to_hsla(palette.paper);
    theme.accordion_hover = to_hsla(palette.sidebar);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两套皮肤的字段集相同（由类型保证），但取值必须真的不同，否则"可切换"是假的。
    #[test]
    fn the_two_skins_actually_differ() {
        let light = Palette::LIGHT;
        let dark = Palette::DARK;
        assert_ne!(light.paper, dark.paper);
        assert_ne!(light.ink, dark.ink);
        assert_ne!(light.surface, dark.surface);
        assert_ne!(light.accent, dark.accent);
        assert_ne!(light.scrim, dark.scrim);
    }

    /// 深色下"强调文字"要比"柔和强调底"更亮，浅色下则相反：绑反了两套皮肤里
    /// 必有一套读不出来。
    #[test]
    fn accent_text_stays_on_the_readable_side_of_its_backdrop() {
        let light = Palette::LIGHT;
        let dark = Palette::DARK;
        assert!(luminance(light.accent_strong) < luminance(light.accent_soft));
        assert!(luminance(dark.accent_strong) > luminance(dark.accent_soft));
    }

    /// 正文与次要文字在两套皮肤下都要和底色拉开足够距离（WCAG AA 4.5:1）。
    #[test]
    fn body_and_muted_text_meet_the_contrast_floor_in_both_skins() {
        for palette in [Palette::LIGHT, Palette::DARK] {
            let paper = relative_luminance(palette.paper);
            assert!(
                contrast_ratio(relative_luminance(palette.ink), paper) >= 4.5,
                "正文与底色的对比度不足"
            );
            assert!(
                contrast_ratio(relative_luminance(palette.muted), paper) >= 4.5,
                "次要文字与底色的对比度不足"
            );
            // 侧栏与正文区共用次要文字，两处都要达标。
            assert!(
                contrast_ratio(
                    relative_luminance(palette.muted),
                    relative_luminance(palette.sidebar)
                ) >= 4.5,
                "次要文字在侧栏底色上的对比度不足"
            );
        }
    }

    /// 选中底色要同时成立两件事：在不透明的卡片底上看得出来，且被选中的文字仍然
    /// 读得出来。不透明的皮肤色做不到前者 —— 深色皮肤里 `accent_soft` 与 `surface`
    /// （AI 回答卡片、输入框）只差一档明度，选中范围几乎看不出来 —— 所以它是叠了
    /// alpha 的强调色，这里按 alpha 合成成实际显示的颜色再断言。
    #[test]
    fn the_selection_tint_is_visible_and_keeps_the_selected_text_readable() {
        for palette in [Palette::LIGHT, Palette::DARK] {
            let selection = palette.selection();
            assert!(
                selection.a > 0. && selection.a < 1.,
                "选中底色必须是半透明的强调色"
            );
            for backdrop in [palette.paper, palette.surface, palette.sidebar] {
                let selected = composited(selection, backdrop);
                assert!(
                    contrast_ratio(relative_luminance(selected), relative_luminance(backdrop))
                        >= 1.5,
                    "选中范围在 {backdrop:#08x} 上看不出来"
                );
                assert!(
                    contrast_ratio(
                        relative_luminance(palette.ink),
                        relative_luminance(selected)
                    ) >= 4.5,
                    "选中后的文字在 {backdrop:#08x} 上读不出来"
                );
            }
        }
    }

    /// 强调色同时是链接色、图标色与实心按钮底色，三个位置都得可读。
    #[test]
    fn the_accent_color_is_readable_as_text_and_as_a_button_backdrop() {
        for palette in [Palette::LIGHT, Palette::DARK] {
            assert!(
                contrast_ratio(
                    relative_luminance(palette.accent),
                    relative_luminance(palette.paper)
                ) >= 4.5,
                "强调色作为正文链接色时对比度不足"
            );
            assert!(
                contrast_ratio(
                    relative_luminance(palette.paper),
                    relative_luminance(palette.accent)
                ) >= 4.5,
                "实心按钮上的文字对比度不足"
            );
            // 选中标签与分组标题用 accent_strong / accent_soft 这一对。
            assert!(
                contrast_ratio(
                    relative_luminance(palette.accent_strong),
                    relative_luminance(palette.accent_soft)
                ) >= 4.5,
                "强调文字在柔和强调底上的对比度不足"
            );
        }
    }

    /// 状态色在各自柔和底上也要可读，否则"失败/成功"标签会糊成一片。
    #[test]
    fn status_colors_are_readable_on_their_soft_backdrops() {
        for palette in [Palette::LIGHT, Palette::DARK] {
            for (foreground, background, label) in [
                (palette.danger, palette.danger_soft, "失败"),
                (palette.warning, palette.warning_soft, "警告"),
                (palette.success, palette.success_soft, "成功"),
                (palette.info, palette.info_soft, "信息"),
            ] {
                assert!(
                    contrast_ratio(
                        relative_luminance(foreground),
                        relative_luminance(background)
                    ) >= 4.5,
                    "{label} 色在柔和底上的对比度不足"
                );
            }
        }
    }

    /// 变量名和下发的十六进制值必须在两套皮肤里同名不同值：阅读器与编辑器按名字取值，
    /// 少一个变量就会退回 CSS 里写死的浅色。
    #[test]
    fn css_variables_cover_both_skins_with_distinct_values() {
        let light = Palette::LIGHT.css_variables();
        let dark = Palette::DARK.css_variables();
        for (palette, css) in [(Palette::LIGHT, &light), (Palette::DARK, &dark)] {
            // 阅读器、编辑器与 PDF 阅读器按这些名字取值；少一个就会退回 CSS 里
            // 写死的那套颜色，于是两套皮肤混在一起。
            for name in [
                "--ngy-paper: ",
                "--ngy-text: ",
                "--ngy-muted: ",
                "--ngy-border: ",
                "--ngy-accent: ",
                "--ngy-accent-strong: ",
                "--ngy-accent-soft: ",
            ] {
                assert!(css.contains(name), "缺少 {name}");
            }
            assert!(css.contains(&format!("--ngy-paper: {}", Palette::hex(palette.paper))));
        }
        assert_ne!(light, dark);
    }

    #[test]
    fn appearance_names_matches_serialized_form() {
        assert_eq!(Appearance::Light.name(), "light");
        assert_eq!(Appearance::Dark.name(), "dark");
        assert_eq!(Appearance::Light.color_scheme(), "light");
        assert_eq!(Appearance::Dark.color_scheme(), "dark");
        assert!(Appearance::default() == Appearance::Light);
        assert_eq!(
            serde_json::from_str::<Appearance>("\"dark\"").unwrap(),
            Appearance::Dark
        );
    }

    fn luminance(value: u32) -> f32 {
        relative_luminance(value)
    }

    fn relative_luminance(value: u32) -> f32 {
        let channel = |shift: u32| {
            let raw = ((value >> shift) & 0xff) as f32 / 255.0;
            if raw <= 0.03928 {
                raw / 12.92
            } else {
                ((raw + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast_ratio(a: f32, b: f32) -> f32 {
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    /// 半透明色按 alpha 合成到不透明底色上，得到实际显示的颜色 —— 也就是 GPU 上
    /// 选中矩形压在卡片底色之后，用户真正看到的那一版。
    fn composited(foreground: gpui::Rgba, backdrop: u32) -> u32 {
        let channel = |shift: u32, value: f32| {
            let base = ((backdrop >> shift) & 0xff) as f32 / 255.0;
            value * foreground.a + base * (1.0 - foreground.a)
        };
        let to_byte = |value: f32| (value.clamp(0., 1.) * 255.0).round() as u32;
        (to_byte(channel(16, foreground.r)) << 16)
            | (to_byte(channel(8, foreground.g)) << 8)
            | to_byte(channel(0, foreground.b))
    }
}
