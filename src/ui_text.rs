//! User-facing text in English, Simplified Chinese, and Japanese.
//!
//! Only app-authored wording lives here. File names, paths, format names, key
//! names, and raw technical diagnostics stay untranslated.

use crate::settings::Language;

pub struct Catalog {
    ui_language: Language,

    pub open: &'static str,
    pub open_tooltip: &'static str,
    pub fullscreen: &'static str,
    pub fullscreen_tooltip: &'static str,
    pub settings: &'static str,
    pub settings_tooltip: &'static str,
    pub previews_show: &'static str,
    pub previews_hide: &'static str,
    pub previews_tooltip: &'static str,

    pub open_image: &'static str,
    pub drop_hint: &'static str,
    pub formats: &'static str,
    pub opening_image: &'static str,
    pub reading_file: &'static str,
    pub rotating: &'static str,

    pub settings_title: &'static str,
    pub settings_desc: &'static str,
    pub before: &'static str,
    pub after: &'static str,
    pub memory: &'static str,
    pub preview_strip: &'static str,
    pub shown: &'static str,
    pub hidden: &'static str,
    pub preview_tooltip: &'static str,
    pub reading_mode: &'static str,
    pub single_image: &'static str,
    pub book_rtl: &'static str,
    pub reading_tooltip: &'static str,
    pub book_next_spread: &'static str,
    pub right_arrow: &'static str,
    pub left_arrow: &'static str,
    pub arrow_tooltip: &'static str,
    pub language: &'static str,
    pub automatic: &'static str,
    pub settings_help: &'static str,
    pub restore_defaults: &'static str,
    pub restore_tooltip: &'static str,
    pub apply: &'static str,
    pub apply_tooltip: &'static str,
    pub cancel: &'static str,
    pub cancel_tooltip: &'static str,
    pub dismiss: &'static str,
    pub dismiss_tooltip: &'static str,
    pub increase: &'static str,
    pub decrease: &'static str,

    pub prev: &'static str,
    pub prev_tooltip: &'static str,
    pub next: &'static str,
    pub next_tooltip: &'static str,
    pub zoom_out_tooltip: &'static str,
    pub zoom_in_tooltip: &'static str,
    pub fit: &'static str,
    pub fit_tooltip: &'static str,
    pub actual: &'static str,
    pub actual_tooltip: &'static str,
    pub pause: &'static str,
    pub replay: &'static str,
    pub play: &'static str,
    pub playback_tooltip: &'static str,
    pub rotate_right: &'static str,
    pub rotate_right_tooltip: &'static str,
    pub rotate_left: &'static str,
    pub rotate_left_tooltip: &'static str,

    pub prefetch: &'static str,
    pub loading: &'static str,
    pub queued: &'static str,
    pub no_previews: &'static str,
    pub image_viewer: &'static str,
    pub book: &'static str,
    pub gif: &'static str,
    pub playing: &'static str,
    pub paused: &'static str,
    pub finished: &'static str,
    pub page: &'static str,
    pub pages: &'static str,
    pub pixel_read_only: &'static str,
    pub idle_read_only: &'static str,
    pub name_separator: &'static str,

    pub error_title: &'static str,
    pub path_failed: &'static str,
    pub could_not_open: &'static str,
    pub showing_last: &'static str,
    pub settings_warning: &'static str,
    pub settings_saved: &'static str,
    pub copied_image: &'static str,
    pub copied_selected_page: &'static str,
    pub applying_saving: &'static str,
    pub reading_mode_saving: &'static str,
    pub previews_showing: &'static str,
    pub previews_hiding: &'static str,
    pub saving_before_close: &'static str,
    pub save_failed: &'static str,
    pub close_again: &'static str,
    pub open_picker_title: &'static str,
}

pub static EN: Catalog = Catalog {
    ui_language: Language::English,

    open: "Open",
    open_tooltip: "Open image · Ctrl/Cmd+O",
    fullscreen: "Full screen",
    fullscreen_tooltip: "Toggle full screen · F11",
    settings: "Settings",
    settings_tooltip: "Settings · Ctrl/Cmd+,",
    previews_show: "Show previews",
    previews_hide: "Hide previews",
    previews_tooltip: "Show or hide the preview strip · saved automatically",

    open_image: "Open an image",
    drop_hint: "Drop a file here, or choose Open above.",
    formats: "PNG · JPEG · WebP · GIF · BMP · ICO · TIFF",
    opening_image: "Opening image…",
    reading_file: "Reading the file",
    rotating: "Rotating…",

    settings_title: "Settings",
    settings_desc: "Reading and previews. Changes apply together.",
    before: "Before",
    after: "After",
    memory: "Prefetch memory (MiB)",
    preview_strip: "Preview strip",
    shown: "Shown",
    hidden: "Hidden",
    preview_tooltip: "Toggle the bottom preview strip",
    reading_mode: "Reading mode",
    single_image: "Single image",
    book_rtl: "Book · right to left",
    reading_tooltip: "Switch between one image and right-to-left spreads",
    book_next_spread: "Book: next spread",
    right_arrow: "Right arrow",
    left_arrow: "Left arrow",
    arrow_tooltip: "Choose which arrow advances to the next spread",
    language: "Language",
    automatic: "Automatic",
    settings_help: "Tab moves · Arrow keys adjust · Esc cancels",
    restore_defaults: "Restore defaults",
    restore_tooltip: "Reset all settings in the draft",
    apply: "Apply",
    apply_tooltip: "Save and apply",
    cancel: "Cancel",
    cancel_tooltip: "Discard changes",
    dismiss: "Dismiss",
    dismiss_tooltip: "Hide this message",
    increase: "Increase by",
    decrease: "Decrease by",

    prev: "Prev",
    prev_tooltip: "Previous image · Page Up",
    next: "Next",
    next_tooltip: "Next image · Page Down",
    zoom_out_tooltip: "Zoom out · -",
    zoom_in_tooltip: "Zoom in · +",
    fit: "Fit",
    fit_tooltip: "Fit to window · 1 / 9",
    actual: "1:1",
    actual_tooltip: "Actual size · 0 · one image pixel per display pixel",
    pause: "Pause",
    replay: "Replay",
    play: "Play",
    playback_tooltip: "Pause, resume, or replay the animation · P",
    rotate_right: "Rotate R",
    rotate_right_tooltip: "Rotate clockwise · Ctrl/Cmd+R",
    rotate_left: "Rotate L",
    rotate_left_tooltip: "Rotate counterclockwise · Ctrl/Cmd+L",

    prefetch: "Prefetch",
    loading: "loading…",
    queued: "queued",
    no_previews: "No previews",
    image_viewer: "Image viewer",
    book: "Book",
    gif: "GIF",
    playing: "Playing",
    paused: "Paused",
    finished: "Finished",
    page: "page",
    pages: "pages",
    pixel_read_only: "px · read-only",
    idle_read_only: "Read-only · your files stay unchanged",
    name_separator: " + ",

    error_title: "Could not open this image",
    path_failed: "Could not resolve the selected path:",
    could_not_open: "Could not open",
    showing_last: "showing last image",
    settings_warning: "Could not load settings; using defaults.",
    settings_saved: "Settings saved.",
    copied_image: "Copied image",
    copied_selected_page: "Copied selected page (spread too large to combine)",
    applying_saving: "Applied · saving settings…",
    reading_mode_saving: "Reading mode changed · saving…",
    previews_showing: "Showing previews · saving…",
    previews_hiding: "Hiding previews · saving…",
    saving_before_close: "Saving settings before closing…",
    save_failed: "Applied, but not saved:",
    close_again: "Close again to exit without saving.",
    open_picker_title: "Open image",
};

pub static ZH: Catalog = Catalog {
    ui_language: Language::SimplifiedChinese,

    open: "打开",
    open_tooltip: "打开图片 · Ctrl/Cmd+O",
    fullscreen: "全屏",
    fullscreen_tooltip: "切换全屏 · F11",
    settings: "设置",
    settings_tooltip: "设置 · Ctrl/Cmd+,",
    previews_show: "显示预览",
    previews_hide: "隐藏预览",
    previews_tooltip: "显示或隐藏预览栏 · 自动保存",

    open_image: "打开图片",
    drop_hint: "可将文件拖到此处，或点击上方“打开”。",
    formats: "PNG · JPEG · WebP · GIF · BMP · ICO · TIFF",
    opening_image: "正在打开图片…",
    reading_file: "正在读取文件",
    rotating: "正在旋转…",

    settings_title: "设置",
    settings_desc: "阅读方式与预览。更改会一起应用。",
    before: "之前",
    after: "之后",
    memory: "预取内存（MiB）",
    preview_strip: "预览栏",
    shown: "显示",
    hidden: "隐藏",
    preview_tooltip: "切换底部预览栏",
    reading_mode: "阅读模式",
    single_image: "单图",
    book_rtl: "漫画 · 从右到左",
    reading_tooltip: "在单图与从右到左的双页漫画之间切换",
    book_next_spread: "漫画：下一跨页",
    right_arrow: "右方向键",
    left_arrow: "左方向键",
    arrow_tooltip: "选择用哪个方向键进入下一跨页",
    language: "语言",
    automatic: "自动（跟随系统）",
    settings_help: "Tab 移动 · 方向键调整 · Esc 取消",
    restore_defaults: "恢复默认值",
    restore_tooltip: "在草稿中重置所有设置",
    apply: "应用",
    apply_tooltip: "保存并应用",
    cancel: "取消",
    cancel_tooltip: "放弃更改",
    dismiss: "关闭",
    dismiss_tooltip: "隐藏此消息",
    increase: "增加",
    decrease: "减少",

    prev: "上一张",
    prev_tooltip: "上一张图片 · Page Up",
    next: "下一张",
    next_tooltip: "下一张图片 · Page Down",
    zoom_out_tooltip: "缩小 · -",
    zoom_in_tooltip: "放大 · +",
    fit: "适应",
    fit_tooltip: "适应窗口 · 1 / 9",
    actual: "1:1",
    actual_tooltip: "实际大小 · 0 · 一个图像像素对应一个显示像素",
    pause: "暂停",
    replay: "重播",
    play: "播放",
    playback_tooltip: "暂停、继续或重播动画 · P",
    rotate_right: "右转",
    rotate_right_tooltip: "顺时针旋转 · Ctrl/Cmd+R",
    rotate_left: "左转",
    rotate_left_tooltip: "逆时针旋转 · Ctrl/Cmd+L",

    prefetch: "预取",
    loading: "正在加载…",
    queued: "排队",
    no_previews: "没有预览",
    image_viewer: "图片查看器",
    book: "漫画",
    gif: "GIF",
    playing: "播放中",
    paused: "已暂停",
    finished: "已结束",
    page: "页",
    pages: "页",
    pixel_read_only: "像素 · 只读",
    idle_read_only: "只读 · 文件不会被更改",
    name_separator: " + ",

    error_title: "无法打开此图片",
    path_failed: "无法解析所选路径：",
    could_not_open: "无法打开",
    showing_last: "正在显示上一张",
    settings_warning: "无法加载设置，已使用默认值。",
    settings_saved: "设置已保存。",
    copied_image: "已复制图片",
    copied_selected_page: "已复制所选页面（跨页过大，无法合并）",
    applying_saving: "已应用 · 正在保存设置…",
    reading_mode_saving: "阅读模式已更改 · 正在保存…",
    previews_showing: "正在显示预览 · 保存中…",
    previews_hiding: "正在隐藏预览 · 保存中…",
    saving_before_close: "关闭前正在保存设置…",
    save_failed: "已应用，但未能保存：",
    close_again: "再次关闭即可不保存退出。",
    open_picker_title: "打开图片",
};

pub static JA: Catalog = Catalog {
    ui_language: Language::Japanese,

    open: "開く",
    open_tooltip: "画像を開く・Ctrl/Cmd+O",
    fullscreen: "全画面",
    fullscreen_tooltip: "全画面を切り替え・F11",
    settings: "設定",
    settings_tooltip: "設定・Ctrl/Cmd+,",
    previews_show: "プレビューを表示",
    previews_hide: "プレビューを隠す",
    previews_tooltip: "下部のプレビュー欄を表示または非表示・自動保存",

    open_image: "画像を開く",
    drop_hint: "ここにファイルをドロップするか、上の「開く」を選んでください。",
    formats: "PNG · JPEG · WebP · GIF · BMP · ICO · TIFF",
    opening_image: "画像を開いています…",
    reading_file: "ファイルを読み込み中",
    rotating: "回転しています…",

    settings_title: "設定",
    settings_desc: "表示方法とプレビュー。変更はまとめて適用されます。",
    before: "前",
    after: "後",
    memory: "先読みメモリ（MiB）",
    preview_strip: "プレビュー欄",
    shown: "表示",
    hidden: "非表示",
    preview_tooltip: "下部のプレビュー欄を切り替え",
    reading_mode: "表示モード",
    single_image: "単一画像",
    book_rtl: "ブック・右から左",
    reading_tooltip: "単一画像と右開きの見開きを切り替え",
    book_next_spread: "ブック：次の見開き",
    right_arrow: "右矢印",
    left_arrow: "左矢印",
    arrow_tooltip: "次の見開きに進む矢印を選択",
    language: "言語",
    automatic: "自動（システム）",
    settings_help: "Tab で移動・矢印キーで調整・Esc でキャンセル",
    restore_defaults: "既定値に戻す",
    restore_tooltip: "下書き内のすべての設定をリセット",
    apply: "適用",
    apply_tooltip: "保存して適用",
    cancel: "キャンセル",
    cancel_tooltip: "変更を破棄",
    dismiss: "閉じる",
    dismiss_tooltip: "このメッセージを隠す",
    increase: "増やす",
    decrease: "減らす",

    prev: "前へ",
    prev_tooltip: "前の画像・Page Up",
    next: "次へ",
    next_tooltip: "次の画像・Page Down",
    zoom_out_tooltip: "縮小・-",
    zoom_in_tooltip: "拡大・+",
    fit: "合わせる",
    fit_tooltip: "ウィンドウに合わせる・1 / 9",
    actual: "1:1",
    actual_tooltip: "実際のサイズ・0・画像1ピクセルを表示1ピクセルで表示",
    pause: "一時停止",
    replay: "再生し直す",
    play: "再生",
    playback_tooltip: "アニメーションを一時停止、再開、または再生し直す・P",
    rotate_right: "右回転",
    rotate_right_tooltip: "時計回りに回転・Ctrl/Cmd+R",
    rotate_left: "左回転",
    rotate_left_tooltip: "反時計回りに回転・Ctrl/Cmd+L",

    prefetch: "先読み",
    loading: "読み込み中…",
    queued: "待機",
    no_previews: "プレビューなし",
    image_viewer: "画像ビューア",
    book: "ブック",
    gif: "GIF",
    playing: "再生中",
    paused: "一時停止中",
    finished: "終了",
    page: "ページ",
    pages: "ページ",
    pixel_read_only: "px・読み取り専用",
    idle_read_only: "読み取り専用・ファイルは変更されません",
    name_separator: " + ",

    error_title: "この画像を開けませんでした",
    path_failed: "選択したパスを解決できません：",
    could_not_open: "を開けません",
    showing_last: "前の画像を表示中",
    settings_warning: "設定を読み込めないため、既定値を使用します。",
    settings_saved: "設定を保存しました。",
    copied_image: "画像をコピーしました",
    copied_selected_page: "選択したページをコピーしました（見開きが大きすぎるため結合できません）",
    applying_saving: "適用しました・設定を保存中…",
    reading_mode_saving: "表示モードを変更しました・保存中…",
    previews_showing: "プレビューを表示・保存中…",
    previews_hiding: "プレビューを非表示・保存中…",
    saving_before_close: "終了前に設定を保存しています…",
    save_failed: "適用しましたが保存できませんでした：",
    close_again: "もう一度閉じると保存せず終了します。",
    open_picker_title: "画像を開く",
};

/// `Automatic` is resolved before this is called; mapping it to English keeps
/// the catalog total even if a caller forgets.
pub fn catalog(language: Language) -> &'static Catalog {
    match language {
        Language::English | Language::Automatic => &EN,
        Language::SimplifiedChinese => &ZH,
        Language::Japanese => &JA,
    }
}

impl Catalog {
    pub fn dimensions(&self, width: u32, height: u32) -> String {
        format!("{width} × {height} {}", self.pixel_read_only)
    }

    pub fn book_pages(&self, pages: usize) -> String {
        match self.ui_language {
            Language::Japanese => format!(" · {}・{}ページ", self.book, pages),
            Language::SimplifiedChinese => format!(" · {} · {} 页", self.book, pages),
            _ => format!(
                " · {} · {} {}",
                self.book,
                pages,
                if pages == 1 { self.page } else { self.pages }
            ),
        }
    }

    pub fn book_gif(&self, counters: &str, state: &str) -> String {
        match self.ui_language {
            Language::Japanese => format!(" · {} {}・{}", self.gif, counters, state),
            _ => format!(" · {} {} · {}", self.gif, counters, state),
        }
    }

    pub fn single_gif(&self, frame: usize, total: usize, state: &str) -> String {
        match self.ui_language {
            Language::Japanese => format!(" · {} {frame}/{total}・{state}", self.gif),
            _ => format!(" · {} {frame}/{total} · {state}", self.gif),
        }
    }

    pub fn prefetch_stats(
        &self,
        entries: usize,
        mebibytes: f64,
        loading: bool,
        queued: usize,
    ) -> String {
        match self.ui_language {
            Language::SimplifiedChinese => format!(
                "{}：{entries} · {mebibytes:.1} MiB{}{}",
                self.prefetch,
                if loading {
                    format!(" · {}", self.loading)
                } else {
                    String::new()
                },
                if queued > 0 {
                    format!(" · {} {queued}", self.queued)
                } else {
                    String::new()
                }
            ),
            Language::Japanese => format!(
                "{}: {entries}・{mebibytes:.1} MiB{}{}",
                self.prefetch,
                if loading {
                    format!("・{}", self.loading)
                } else {
                    String::new()
                },
                if queued > 0 {
                    format!("・{queued} {}", self.queued)
                } else {
                    String::new()
                }
            ),
            _ => format!(
                "{}: {entries} · {mebibytes:.1} MiB{}{}",
                self.prefetch,
                if loading {
                    format!(" · {}", self.loading)
                } else {
                    String::new()
                },
                if queued > 0 {
                    format!(" · {queued} {}", self.queued)
                } else {
                    String::new()
                }
            ),
        }
    }

    pub fn could_not_open(&self, name: &str, showing_last: bool) -> String {
        match self.ui_language {
            Language::Japanese => {
                let mut text = format!("{name}{}", self.could_not_open);
                if showing_last {
                    text.push('・');
                    text.push_str(self.showing_last);
                }
                text
            }
            Language::SimplifiedChinese => {
                let mut text = format!("{}{name}", self.could_not_open);
                if showing_last {
                    text.push_str(" · ");
                    text.push_str(self.showing_last);
                }
                text
            }
            _ => {
                let mut text = format!("{} {name}", self.could_not_open);
                if showing_last {
                    text.push_str(" · ");
                    text.push_str(self.showing_last);
                }
                text
            }
        }
    }

    pub fn opening_file(&self, name: &str) -> String {
        match self.ui_language {
            Language::Japanese => format!("{name} を開いています…"),
            Language::SimplifiedChinese => format!("正在打开 {name}…"),
            _ => format!("Opening {name}…"),
        }
    }

    pub fn settings_warning(&self, detail: &str) -> String {
        format!("{}\n{detail}", self.settings_warning)
    }

    pub fn path_failed_detail(&self, detail: &str) -> String {
        if self.ui_language == Language::English {
            format!("{} {detail}", self.path_failed)
        } else {
            format!("{}{detail}", self.path_failed)
        }
    }

    pub fn save_failed(&self, detail: &str, closing: bool) -> String {
        let mut text = if self.ui_language == Language::English {
            format!("{} {detail}", self.save_failed)
        } else {
            format!("{}{detail}", self.save_failed)
        };
        if closing {
            text.push('\n');
            text.push_str(self.close_again);
        }
        text
    }

    pub fn image_names(&self, names: &[String]) -> String {
        names.join(self.name_separator)
    }

    pub fn change_step(&self, step: u64, increase: bool) -> String {
        match self.ui_language {
            Language::Japanese => format!(
                "{step} {}",
                if increase {
                    self.increase
                } else {
                    self.decrease
                }
            ),
            Language::SimplifiedChinese => format!(
                "{} {step}",
                if increase {
                    self.increase
                } else {
                    self.decrease
                }
            ),
            _ => format!(
                "{} {step}",
                if increase {
                    self.increase
                } else {
                    self.decrease
                }
            ),
        }
    }

    pub fn language_name(&self, language: Language) -> &'static str {
        match language {
            Language::Automatic => self.automatic,
            Language::English => "English",
            Language::SimplifiedChinese => "简体中文",
            Language::Japanese => "日本語",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(catalog: &Catalog) -> Vec<(&'static str, &'static str)> {
        vec![
            ("open", catalog.open),
            ("open_tooltip", catalog.open_tooltip),
            ("fullscreen", catalog.fullscreen),
            ("fullscreen_tooltip", catalog.fullscreen_tooltip),
            ("settings", catalog.settings),
            ("settings_tooltip", catalog.settings_tooltip),
            ("previews_show", catalog.previews_show),
            ("previews_hide", catalog.previews_hide),
            ("previews_tooltip", catalog.previews_tooltip),
            ("open_image", catalog.open_image),
            ("drop_hint", catalog.drop_hint),
            ("formats", catalog.formats),
            ("opening_image", catalog.opening_image),
            ("reading_file", catalog.reading_file),
            ("rotating", catalog.rotating),
            ("settings_title", catalog.settings_title),
            ("settings_desc", catalog.settings_desc),
            ("before", catalog.before),
            ("after", catalog.after),
            ("memory", catalog.memory),
            ("preview_strip", catalog.preview_strip),
            ("shown", catalog.shown),
            ("hidden", catalog.hidden),
            ("preview_tooltip", catalog.preview_tooltip),
            ("reading_mode", catalog.reading_mode),
            ("single_image", catalog.single_image),
            ("book_rtl", catalog.book_rtl),
            ("reading_tooltip", catalog.reading_tooltip),
            ("book_next_spread", catalog.book_next_spread),
            ("right_arrow", catalog.right_arrow),
            ("left_arrow", catalog.left_arrow),
            ("arrow_tooltip", catalog.arrow_tooltip),
            ("language", catalog.language),
            ("automatic", catalog.automatic),
            ("settings_help", catalog.settings_help),
            ("restore_defaults", catalog.restore_defaults),
            ("restore_tooltip", catalog.restore_tooltip),
            ("apply", catalog.apply),
            ("apply_tooltip", catalog.apply_tooltip),
            ("cancel", catalog.cancel),
            ("cancel_tooltip", catalog.cancel_tooltip),
            ("dismiss", catalog.dismiss),
            ("dismiss_tooltip", catalog.dismiss_tooltip),
            ("increase", catalog.increase),
            ("decrease", catalog.decrease),
            ("prev", catalog.prev),
            ("prev_tooltip", catalog.prev_tooltip),
            ("next", catalog.next),
            ("next_tooltip", catalog.next_tooltip),
            ("zoom_out_tooltip", catalog.zoom_out_tooltip),
            ("zoom_in_tooltip", catalog.zoom_in_tooltip),
            ("fit", catalog.fit),
            ("fit_tooltip", catalog.fit_tooltip),
            ("actual", catalog.actual),
            ("actual_tooltip", catalog.actual_tooltip),
            ("pause", catalog.pause),
            ("replay", catalog.replay),
            ("play", catalog.play),
            ("playback_tooltip", catalog.playback_tooltip),
            ("rotate_right", catalog.rotate_right),
            ("rotate_right_tooltip", catalog.rotate_right_tooltip),
            ("rotate_left", catalog.rotate_left),
            ("rotate_left_tooltip", catalog.rotate_left_tooltip),
            ("prefetch", catalog.prefetch),
            ("loading", catalog.loading),
            ("queued", catalog.queued),
            ("no_previews", catalog.no_previews),
            ("image_viewer", catalog.image_viewer),
            ("book", catalog.book),
            ("gif", catalog.gif),
            ("playing", catalog.playing),
            ("paused", catalog.paused),
            ("finished", catalog.finished),
            ("page", catalog.page),
            ("pages", catalog.pages),
            ("pixel_read_only", catalog.pixel_read_only),
            ("idle_read_only", catalog.idle_read_only),
            ("name_separator", catalog.name_separator),
            ("error_title", catalog.error_title),
            ("path_failed", catalog.path_failed),
            ("could_not_open", catalog.could_not_open),
            ("showing_last", catalog.showing_last),
            ("settings_warning", catalog.settings_warning),
            ("settings_saved", catalog.settings_saved),
            ("copied_image", catalog.copied_image),
            ("copied_selected_page", catalog.copied_selected_page),
            ("applying_saving", catalog.applying_saving),
            ("reading_mode_saving", catalog.reading_mode_saving),
            ("previews_showing", catalog.previews_showing),
            ("previews_hiding", catalog.previews_hiding),
            ("saving_before_close", catalog.saving_before_close),
            ("save_failed", catalog.save_failed),
            ("close_again", catalog.close_again),
            ("open_picker_title", catalog.open_picker_title),
        ]
    }

    #[test]
    fn every_catalog_field_is_non_empty() {
        for catalog in [&EN, &ZH, &JA] {
            assert_eq!(fields(catalog).len(), 94, "catalog field list is stale");
            for (name, value) in fields(catalog) {
                assert!(!value.trim().is_empty(), "{name} is empty");
            }
        }
        assert_eq!(catalog(Language::Automatic).ui_language, Language::English);
    }

    #[test]
    fn dynamic_messages_keep_language_order_and_include_values() {
        for (catalog, marker) in [
            (&EN, "Could not open"),
            (&ZH, "无法打开"),
            (&JA, "を開けません"),
        ] {
            let text = catalog.could_not_open("photo.png", true);
            assert!(text.contains(marker));
            assert!(text.contains("photo.png"));
            assert!(catalog.dimensions(640, 480).contains("640 × 480"));
            assert!(catalog.book_pages(2).contains('2'));
            assert!(
                catalog
                    .book_gif("1/2 + 2/3", catalog.playing)
                    .contains("1/2 + 2/3")
            );
            assert!(catalog.single_gif(2, 9, catalog.paused).contains("2/9"));
            let stats = catalog.prefetch_stats(4, 12.5, true, 2);
            assert!(stats.contains('4') && stats.contains("12.5") && stats.contains('2'));
            assert!(catalog.opening_file("scan.jpg").contains("scan.jpg"));
            assert!(
                catalog
                    .settings_warning("raw detail")
                    .contains("raw detail")
            );
            assert!(catalog.save_failed("raw error", true).contains("raw error"));
        }
        assert_ne!(
            EN.could_not_open("x.png", false),
            ZH.could_not_open("x.png", false)
        );
        assert_ne!(
            EN.prefetch_stats(4, 12.5, true, 2),
            JA.prefetch_stats(4, 12.5, true, 2)
        );
        assert_ne!(EN.dimensions(10, 20), ZH.dimensions(10, 20));
        assert_ne!(EN.path_failed_detail("x"), JA.path_failed_detail("x"));
        assert!(EN.settings_warning("raw detail").contains('\n'));
        assert!(EN.save_failed("raw error", true).contains('\n'));
        assert_eq!(EN.language_name(Language::SimplifiedChinese), "简体中文");
        assert_eq!(JA.language_name(Language::Japanese), "日本語");
        assert_eq!(ZH.language_name(Language::Automatic), ZH.automatic);
    }
}
