//! User-facing text in English, Simplified Chinese, and Japanese.
//!
//! Only app-authored wording lives here. File names, paths, format names, key
//! names, and raw technical diagnostics stay untranslated.

use crate::progress::PageOverride;
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
    pub wide_pages: &'static str,
    pub auto_wide_auto: &'static str,
    pub auto_wide_off: &'static str,
    pub auto_wide_tooltip: &'static str,
    pub wide_auto: &'static str,
    pub wide_single: &'static str,
    pub wide_spread: &'static str,
    pub wide_tooltip: &'static str,
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

    pub editing: &'static str,
    pub editing_unsaved: &'static str,
    pub edit_unsupported_format: &'static str,
    pub edit_single_frame: &'static str,
    pub edit_source_mismatch: &'static str,
    pub edit_failed_prefix: &'static str,
    pub edit_empty: &'static str,
    pub edit_too_large: &'static str,
    pub edit_too_many_ops: &'static str,
    pub crop_empty: &'static str,
    pub edit_discarded: &'static str,
    pub crop: &'static str,
    pub crop_tooltip: &'static str,
    pub crop_hint: &'static str,
    pub crop_zoom_hint: &'static str,
    pub crop_aspect_tooltip: &'static str,
    pub crop_apply: &'static str,
    pub crop_apply_tooltip: &'static str,
    pub crop_cancel: &'static str,
    pub crop_cancel_tooltip: &'static str,
    pub aspect_free: &'static str,
    pub aspect_square: &'static str,
    pub aspect_four_three: &'static str,
    pub aspect_three_two: &'static str,
    pub aspect_sixteen_nine: &'static str,
    pub aspect_original: &'static str,
    pub flip_h: &'static str,
    pub flip_h_tooltip: &'static str,
    pub flip_v: &'static str,
    pub flip_v_tooltip: &'static str,
    pub undo: &'static str,
    pub undo_tooltip: &'static str,
    pub save: &'static str,
    pub save_tooltip: &'static str,
    pub save_as: &'static str,
    pub save_as_tooltip: &'static str,
    pub edit_exit: &'static str,
    pub edit_exit_tooltip: &'static str,
    pub edit_rotate_tooltip: &'static str,
    pub saving_image: &'static str,
    pub save_blocked_closing: &'static str,
    pub archive_cannot_overwrite: &'static str,
    pub saved: &'static str,
    pub saved_as_prefix: &'static str,
    pub reopen_folder: &'static str,
    pub export_failed_prefix: &'static str,
    pub unknown_save_format_prefix: &'static str,
    pub save_confirm_format_prefix: &'static str,
    pub save_confirm_warning: &'static str,
    pub save_confirm_irreversible: &'static str,
    pub save_confirm_atomic: &'static str,
    pub save_confirm_transparency: &'static str,
    pub save_confirm_webp: &'static str,
    pub save_confirm_quality: &'static str,
    pub save_confirm_replace: &'static str,
    pub save_confirm_replace_tooltip: &'static str,

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
    pub no_images: &'static str,
    pub archive_changed: &'static str,
    pub could_not_open: &'static str,
    pub showing_last: &'static str,
    pub settings_warning: &'static str,
    pub settings_saved: &'static str,
    pub progress_warning: &'static str,
    pub progress_save_failed: &'static str,
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
    open_tooltip: "Open image or archive · Ctrl/Cmd+O",
    fullscreen: "Full screen",
    fullscreen_tooltip: "Toggle full screen · F11",
    settings: "Settings",
    settings_tooltip: "Settings · Ctrl/Cmd+,",
    previews_show: "Show previews",
    previews_hide: "Hide previews",
    previews_tooltip: "Show or hide the preview strip · saved automatically",

    open_image: "Open an image or archive",
    drop_hint: "Drop a file here, or choose Open above.",
    formats: "PNG · JPEG · WebP · GIF · BMP · ICO · TIFF · CBZ · ZIP",
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
    wide_pages: "Wide pages",
    auto_wide_auto: "Auto",
    auto_wide_off: "Off",
    auto_wide_tooltip: "Automatically show wide pages alone in Book mode",
    wide_auto: "Wide: Auto",
    wide_single: "Wide: Single",
    wide_spread: "Wide: Spread",
    wide_tooltip: "Toggle wide page · W",
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

    editing: "Editing",
    editing_unsaved: "Editing · unsaved",
    edit_unsupported_format: "Editing supports only PNG, JPEG, WebP, and BMP. GIF, TIFF, and ICO stay read-only; archive members can be edited but only saved as a copy.",
    edit_single_frame: "Editing supports a single still image; animations stay read-only.",
    edit_source_mismatch: "The selected file does not match the displayed image; reopen it before editing.",
    edit_failed_prefix: "Could not edit: ",
    edit_empty: "The image has no pixels to edit.",
    edit_too_large: "Image too large to edit: ",
    edit_too_many_ops: "Edit history is full (16 steps); save or undo before adding more.",
    crop_empty: "The crop selection contains no pixels.",
    edit_discarded: "Edits discarded; the file was not changed.",
    crop: "Crop",
    crop_tooltip: "Select a crop area · C",
    crop_hint: "Drag to select · drag inside to move · double-click to reset · Enter applies · Esc cancels",
    crop_zoom_hint: "Zoom in for pixel-accurate cropping.",
    crop_aspect_tooltip: "Crop aspect ratio",
    crop_apply: "Apply crop",
    crop_apply_tooltip: "Apply the crop · Enter",
    crop_cancel: "Cancel crop",
    crop_cancel_tooltip: "Discard this crop selection · Esc",
    aspect_free: "Free",
    aspect_square: "1:1",
    aspect_four_three: "4:3",
    aspect_three_two: "3:2",
    aspect_sixteen_nine: "16:9",
    aspect_original: "Original",
    flip_h: "Flip H",
    flip_h_tooltip: "Mirror horizontally · X",
    flip_v: "Flip V",
    flip_v_tooltip: "Mirror vertically · Y",
    undo: "Undo",
    undo_tooltip: "Undo the last edit · Ctrl/Cmd+Z",
    save: "Save",
    save_tooltip: "Overwrite the original after confirmation · Ctrl/Cmd+S",
    save_as: "Save As…",
    save_as_tooltip: "Write a copy · Ctrl/Cmd+Shift+S",
    edit_exit: "Exit",
    edit_exit_tooltip: "Leave editing; edits are discarded",
    edit_rotate_tooltip: "Rotate the image · Ctrl/Cmd+R / L",
    saving_image: "Saving image…",
    save_blocked_closing: "The window is closing; the image was not saved.",
    archive_cannot_overwrite: "Files inside archives cannot be overwritten. Use Save As.",
    saved: "Saved.",
    saved_as_prefix: "Saved as ",
    reopen_folder: " · reopen the folder to see the new file.",
    export_failed_prefix: "Save failed: ",
    unknown_save_format_prefix: "Unknown save format for ",
    save_confirm_format_prefix: "Format: ",
    save_confirm_warning: "EXIF, ICC profiles, and animation are not kept.",
    save_confirm_irreversible: "Once saved, an overwrite cannot be undone in Oxeye.",
    save_confirm_atomic: "Writes a temporary file, then swaps it in for the target.",
    save_confirm_transparency: "This JPEG has transparency; alpha will be lost.",
    save_confirm_webp: "WebP is written lossless; the file may get larger.",
    save_confirm_quality: "JPEG quality",
    save_confirm_replace: "Replace",
    save_confirm_replace_tooltip: "Replace the file on disk",

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
    pixel_read_only: "px",
    idle_read_only: "Files stay unchanged until you save.",
    name_separator: " + ",

    error_title: "Could not open this image or archive",
    no_images: "No supported images found.",
    archive_changed: "Archive changed; reopen it to continue.",
    could_not_open: "Could not open",
    showing_last: "showing last image",
    settings_warning: "Could not load settings; using defaults.",
    settings_saved: "Settings saved.",
    progress_warning: "Could not load reading progress; starting fresh.",
    progress_save_failed: "Reading progress not saved:",
    copied_image: "Copied image",
    copied_selected_page: "Copied selected page (spread too large to combine)",
    applying_saving: "Applied · saving settings…",
    reading_mode_saving: "Reading mode changed · saving…",
    previews_showing: "Showing previews · saving…",
    previews_hiding: "Hiding previews · saving…",
    saving_before_close: "Saving settings before closing…",
    save_failed: "Applied, but not saved:",
    close_again: "Close again to exit without saving.",
    open_picker_title: "Open image or archive",
};

pub static ZH: Catalog = Catalog {
    ui_language: Language::SimplifiedChinese,

    open: "打开",
    open_tooltip: "打开图片或压缩包 · Ctrl/Cmd+O",
    fullscreen: "全屏",
    fullscreen_tooltip: "切换全屏 · F11",
    settings: "设置",
    settings_tooltip: "设置 · Ctrl/Cmd+,",
    previews_show: "显示预览",
    previews_hide: "隐藏预览",
    previews_tooltip: "显示或隐藏预览栏 · 自动保存",

    open_image: "打开图片或压缩包",
    drop_hint: "可将文件拖到此处，或点击上方“打开”。",
    formats: "PNG · JPEG · WebP · GIF · BMP · ICO · TIFF · CBZ · ZIP",
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
    wide_pages: "宽页",
    auto_wide_auto: "自动",
    auto_wide_off: "关闭",
    auto_wide_tooltip: "在漫画模式中自动单独显示宽页",
    wide_auto: "宽页：自动",
    wide_single: "宽页：单页",
    wide_spread: "宽页：双页",
    wide_tooltip: "切换宽页显示方式 · W",
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

    editing: "编辑中",
    editing_unsaved: "编辑中 · 未保存",
    edit_unsupported_format: "仅支持编辑 PNG、JPEG、WebP、BMP。GIF、TIFF、ICO 保持只读；压缩包成员可编辑，但只能另存为。",
    edit_single_frame: "仅支持编辑单张静态图片；动画保持只读。",
    edit_source_mismatch: "所选文件与当前显示的图片不一致，请重新打开后再编辑。",
    edit_failed_prefix: "无法编辑：",
    edit_empty: "这张图片没有可编辑的像素。",
    edit_too_large: "图片过大，无法编辑：",
    edit_too_many_ops: "编辑历史上限（16 步），请先保存或撤销。",
    crop_empty: "裁剪区域不包含任何像素。",
    edit_discarded: "已丢弃编辑，文件未被修改。",
    crop: "裁剪",
    crop_tooltip: "选择裁剪区域 · C",
    crop_hint: "拖动选择 · 框内拖动移动 · 双击复位 · Enter 应用 · Esc 取消",
    crop_zoom_hint: "放大后可精确到像素。",
    crop_aspect_tooltip: "裁剪比例",
    crop_apply: "应用裁剪",
    crop_apply_tooltip: "应用裁剪 · Enter",
    crop_cancel: "取消裁剪",
    crop_cancel_tooltip: "放弃本次裁剪 · Esc",
    aspect_free: "自由",
    aspect_square: "1:1",
    aspect_four_three: "4:3",
    aspect_three_two: "3:2",
    aspect_sixteen_nine: "16:9",
    aspect_original: "原图",
    flip_h: "水平翻转",
    flip_h_tooltip: "水平镜像 · X",
    flip_v: "垂直翻转",
    flip_v_tooltip: "垂直镜像 · Y",
    undo: "撤销",
    undo_tooltip: "撤销上一步编辑 · Ctrl/Cmd+Z",
    save: "保存",
    save_tooltip: "确认后覆盖原文件 · Ctrl/Cmd+S",
    save_as: "另存为…",
    save_as_tooltip: "写出副本 · Ctrl/Cmd+Shift+S",
    edit_exit: "退出",
    edit_exit_tooltip: "退出编辑；未保存的修改会被丢弃",
    edit_rotate_tooltip: "旋转图片 · Ctrl/Cmd+R / L",
    saving_image: "正在保存图片…",
    save_blocked_closing: "窗口正在关闭，图片未保存。",
    archive_cannot_overwrite: "压缩包内文件不能覆盖，请使用另存为。",
    saved: "已保存。",
    saved_as_prefix: "已另存为 ",
    reopen_folder: " · 重新打开文件夹可见新文件。",
    export_failed_prefix: "保存失败：",
    unknown_save_format_prefix: "无法识别的保存格式：",
    save_confirm_format_prefix: "格式：",
    save_confirm_warning: "EXIF、ICC 与动画不会保留。",
    save_confirm_irreversible: "覆盖保存后无法在 Oxeye 中撤销。",
    save_confirm_atomic: "先写入临时文件，再原子替换目标。",
    save_confirm_transparency: "该 JPEG 含透明通道，保存后透明信息会丢失。",
    save_confirm_webp: "WebP 使用无损编码，文件可能变大。",
    save_confirm_quality: "JPEG 质量",
    save_confirm_replace: "替换",
    save_confirm_replace_tooltip: "替换磁盘上的文件",

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
    pixel_read_only: "像素",
    idle_read_only: "在你保存之前，文件不会被修改。",
    name_separator: " + ",

    error_title: "无法打开此图片或压缩包",
    no_images: "未找到支持的图片。",
    archive_changed: "压缩包已更改，请重新打开后继续。",
    could_not_open: "无法打开",
    showing_last: "正在显示上一张",
    settings_warning: "无法加载设置，已使用默认值。",
    settings_saved: "设置已保存。",
    progress_warning: "无法加载阅读进度，已从头开始。",
    progress_save_failed: "阅读进度未能保存：",
    copied_image: "已复制图片",
    copied_selected_page: "已复制所选页面（跨页过大，无法合并）",
    applying_saving: "已应用 · 正在保存设置…",
    reading_mode_saving: "阅读模式已更改 · 正在保存…",
    previews_showing: "正在显示预览 · 保存中…",
    previews_hiding: "正在隐藏预览 · 保存中…",
    saving_before_close: "关闭前正在保存设置…",
    save_failed: "已应用，但未能保存：",
    close_again: "再次关闭即可不保存退出。",
    open_picker_title: "打开图片或压缩包",
};

pub static JA: Catalog = Catalog {
    ui_language: Language::Japanese,

    open: "開く",
    open_tooltip: "画像またはアーカイブを開く・Ctrl/Cmd+O",
    fullscreen: "全画面",
    fullscreen_tooltip: "全画面を切り替え・F11",
    settings: "設定",
    settings_tooltip: "設定・Ctrl/Cmd+,",
    previews_show: "プレビューを表示",
    previews_hide: "プレビューを隠す",
    previews_tooltip: "下部のプレビュー欄を表示または非表示・自動保存",

    open_image: "画像またはアーカイブを開く",
    drop_hint: "ここにファイルをドロップするか、上の「開く」を選んでください。",
    formats: "PNG · JPEG · WebP · GIF · BMP · ICO · TIFF · CBZ · ZIP",
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
    wide_pages: "横長のページ",
    auto_wide_auto: "自動",
    auto_wide_off: "オフ",
    auto_wide_tooltip: "ブックモードで横長のページを自動で単独表示",
    wide_auto: "横長：自動",
    wide_single: "横長：単独",
    wide_spread: "横長：見開き",
    wide_tooltip: "横長ページの表示を切り替え・W",
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

    editing: "編集中",
    editing_unsaved: "編集中・未保存",
    edit_unsupported_format: "編集できるのは PNG、JPEG、WebP、BMP のみです。GIF、TIFF、ICO は読み取り専用で、書庫内ファイルは編集できますが「名前を付けて保存」のみです。",
    edit_single_frame: "編集できるのは静止画像 1 枚のみです。アニメーションは読み取り専用です。",
    edit_source_mismatch: "選択中のファイルと表示中の画像が一致しません。開き直してから編集してください。",
    edit_failed_prefix: "編集できません：",
    edit_empty: "編集できるピクセルがありません。",
    edit_too_large: "画像が大きすぎて編集できません：",
    edit_too_many_ops: "編集履歴が上限（16 手）です。保存か取り消しをしてください。",
    crop_empty: "切り抜き範囲にピクセルがありません。",
    edit_discarded: "編集を破棄しました。ファイルは変更されていません。",
    crop: "切り抜き",
    crop_tooltip: "切り抜き範囲を選択・C",
    crop_hint: "ドラッグで選択・内側をドラッグで移動・ダブルクリックで初期化・Enter で適用・Esc で取消",
    crop_zoom_hint: "拡大するとピクセル単位で切り抜けます。",
    crop_aspect_tooltip: "切り抜きの縦横比",
    crop_apply: "切り抜きを適用",
    crop_apply_tooltip: "切り抜きを適用・Enter",
    crop_cancel: "切り抜きを取消",
    crop_cancel_tooltip: "この切り抜きを破棄・Esc",
    aspect_free: "自由",
    aspect_square: "1:1",
    aspect_four_three: "4:3",
    aspect_three_two: "3:2",
    aspect_sixteen_nine: "16:9",
    aspect_original: "元画像",
    flip_h: "左右反転",
    flip_h_tooltip: "左右に鏡像・X",
    flip_v: "上下反転",
    flip_v_tooltip: "上下に鏡像・Y",
    undo: "取り消す",
    undo_tooltip: "直前の編集を取り消す・Ctrl/Cmd+Z",
    save: "保存",
    save_tooltip: "確認のうえ元ファイルを上書き・Ctrl/Cmd+S",
    save_as: "名前を付けて保存…",
    save_as_tooltip: "コピーを書き出す・Ctrl/Cmd+Shift+S",
    edit_exit: "終了",
    edit_exit_tooltip: "編集を終了し、変更を破棄",
    edit_rotate_tooltip: "画像を回転・Ctrl/Cmd+R / L",
    saving_image: "画像を保存中…",
    save_blocked_closing: "ウィンドウを閉じているため、画像は保存されませんでした。",
    archive_cannot_overwrite: "書庫内のファイルは上書きできません。名前を付けて保存してください。",
    saved: "保存しました。",
    saved_as_prefix: "別名で保存しました：",
    reopen_folder: "・フォルダーを開き直すと新しいファイルが表示されます。",
    export_failed_prefix: "保存に失敗しました：",
    unknown_save_format_prefix: "保存形式を判別できません：",
    save_confirm_format_prefix: "形式：",
    save_confirm_warning: "EXIF、ICC プロファイル、アニメーションは保持されません。",
    save_confirm_irreversible: "上書き保存は Oxeye 内では取り消せません。",
    save_confirm_atomic: "一時ファイルに書き出してから、対象と置き換えます。",
    save_confirm_transparency: "この JPEG には透明度があり、保存すると失われます。",
    save_confirm_webp: "WebP は可逆圧縮で書き出すため、ファイルが大きくなる場合があります。",
    save_confirm_quality: "JPEG 品質",
    save_confirm_replace: "置き換える",
    save_confirm_replace_tooltip: "ディスク上のファイルを置き換える",

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
    pixel_read_only: "px",
    idle_read_only: "保存するまでファイルは変更されません。",
    name_separator: " + ",

    error_title: "この画像またはアーカイブを開けませんでした",
    no_images: "対応する画像が見つかりません。",
    archive_changed: "アーカイブが変更されました。開き直してください。",
    could_not_open: "を開けません",
    showing_last: "前の画像を表示中",
    settings_warning: "設定を読み込めないため、既定値を使用します。",
    settings_saved: "設定を保存しました。",
    progress_warning: "読書の進捗を読み込めないため、最初から表示します。",
    progress_save_failed: "読書の進捗を保存できませんでした：",
    copied_image: "画像をコピーしました",
    copied_selected_page: "選択したページをコピーしました（見開きが大きすぎるため結合できません）",
    applying_saving: "適用しました・設定を保存中…",
    reading_mode_saving: "表示モードを変更しました・保存中…",
    previews_showing: "プレビューを表示・保存中…",
    previews_hiding: "プレビューを非表示・保存中…",
    saving_before_close: "終了前に設定を保存しています…",
    save_failed: "適用しましたが保存できませんでした：",
    close_again: "もう一度閉じると保存せず終了します。",
    open_picker_title: "画像またはアーカイブを開く",
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
    pub fn wide_label(&self, mode: PageOverride) -> &'static str {
        match mode {
            PageOverride::Auto => self.wide_auto,
            PageOverride::Single => self.wide_single,
            PageOverride::Spread => self.wide_spread,
        }
    }

    pub fn dimensions(&self, width: u32, height: u32) -> String {
        format!("{width} × {height} {}", self.pixel_read_only)
    }

    pub fn edit_oversize(&self, width: u32, height: u32) -> String {
        format!("{}{width} × {height}", self.edit_too_large)
    }

    pub fn edit_failed(&self, error: &str) -> String {
        format!("{}{error}", self.edit_failed_prefix)
    }

    pub fn save_confirm_format(&self, format: &str) -> String {
        format!("{}{format}", self.save_confirm_format_prefix)
    }

    /// First line of the confirmation: the name that will be written or replaced.
    pub fn save_confirm_heading(&self, name: &str, overwrite: bool) -> String {
        match (self.ui_language, overwrite) {
            (Language::SimplifiedChinese, true) => format!("将覆盖 {name}"),
            (Language::SimplifiedChinese, false) => format!("另存为 {name}"),
            (Language::Japanese, true) => format!("{name} を置き換えます"),
            (Language::Japanese, false) => format!("{name} として保存します"),
            (_, true) => format!("Replace {name}"),
            (_, false) => format!("Save a copy as {name}"),
        }
    }

    pub fn saved_as(&self, path: &str) -> String {
        format!("{}{path}", self.saved_as_prefix)
    }

    pub fn export_failed(&self, error: &str) -> String {
        format!("{}{error}", self.export_failed_prefix)
    }

    pub fn unknown_save_format(&self, name: &str) -> String {
        format!("{}{name}", self.unknown_save_format_prefix)
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

    pub fn progress_warning(&self, detail: &str) -> String {
        format!("{}\n{detail}", self.progress_warning)
    }

    pub fn progress_save_failed(&self, detail: &str) -> String {
        if self.ui_language == Language::English {
            format!("{} {detail}", self.progress_save_failed)
        } else {
            format!("{}{detail}", self.progress_save_failed)
        }
    }

    pub fn open_failed(&self, name: &str, detail: &str) -> String {
        format!("{}\n{detail}", self.could_not_open(name, false))
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
            ("wide_pages", catalog.wide_pages),
            ("auto_wide_auto", catalog.auto_wide_auto),
            ("auto_wide_off", catalog.auto_wide_off),
            ("auto_wide_tooltip", catalog.auto_wide_tooltip),
            ("wide_auto", catalog.wide_auto),
            ("wide_single", catalog.wide_single),
            ("wide_spread", catalog.wide_spread),
            ("wide_tooltip", catalog.wide_tooltip),
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
            ("editing", catalog.editing),
            ("editing_unsaved", catalog.editing_unsaved),
            ("edit_unsupported_format", catalog.edit_unsupported_format),
            ("edit_single_frame", catalog.edit_single_frame),
            ("edit_source_mismatch", catalog.edit_source_mismatch),
            ("edit_failed_prefix", catalog.edit_failed_prefix),
            ("edit_empty", catalog.edit_empty),
            ("edit_too_large", catalog.edit_too_large),
            ("edit_too_many_ops", catalog.edit_too_many_ops),
            ("crop_empty", catalog.crop_empty),
            ("edit_discarded", catalog.edit_discarded),
            ("crop", catalog.crop),
            ("crop_tooltip", catalog.crop_tooltip),
            ("crop_hint", catalog.crop_hint),
            ("crop_zoom_hint", catalog.crop_zoom_hint),
            ("crop_aspect_tooltip", catalog.crop_aspect_tooltip),
            ("crop_apply", catalog.crop_apply),
            ("crop_apply_tooltip", catalog.crop_apply_tooltip),
            ("crop_cancel", catalog.crop_cancel),
            ("crop_cancel_tooltip", catalog.crop_cancel_tooltip),
            ("aspect_free", catalog.aspect_free),
            ("aspect_square", catalog.aspect_square),
            ("aspect_four_three", catalog.aspect_four_three),
            ("aspect_three_two", catalog.aspect_three_two),
            ("aspect_sixteen_nine", catalog.aspect_sixteen_nine),
            ("aspect_original", catalog.aspect_original),
            ("flip_h", catalog.flip_h),
            ("flip_h_tooltip", catalog.flip_h_tooltip),
            ("flip_v", catalog.flip_v),
            ("flip_v_tooltip", catalog.flip_v_tooltip),
            ("undo", catalog.undo),
            ("undo_tooltip", catalog.undo_tooltip),
            ("save", catalog.save),
            ("save_tooltip", catalog.save_tooltip),
            ("save_as", catalog.save_as),
            ("save_as_tooltip", catalog.save_as_tooltip),
            ("edit_exit", catalog.edit_exit),
            ("edit_exit_tooltip", catalog.edit_exit_tooltip),
            ("edit_rotate_tooltip", catalog.edit_rotate_tooltip),
            ("saving_image", catalog.saving_image),
            ("save_blocked_closing", catalog.save_blocked_closing),
            ("archive_cannot_overwrite", catalog.archive_cannot_overwrite),
            ("saved", catalog.saved),
            ("saved_as_prefix", catalog.saved_as_prefix),
            ("reopen_folder", catalog.reopen_folder),
            ("export_failed_prefix", catalog.export_failed_prefix),
            (
                "unknown_save_format_prefix",
                catalog.unknown_save_format_prefix,
            ),
            (
                "save_confirm_format_prefix",
                catalog.save_confirm_format_prefix,
            ),
            ("save_confirm_warning", catalog.save_confirm_warning),
            (
                "save_confirm_irreversible",
                catalog.save_confirm_irreversible,
            ),
            ("save_confirm_atomic", catalog.save_confirm_atomic),
            (
                "save_confirm_transparency",
                catalog.save_confirm_transparency,
            ),
            ("save_confirm_webp", catalog.save_confirm_webp),
            ("save_confirm_quality", catalog.save_confirm_quality),
            ("save_confirm_replace", catalog.save_confirm_replace),
            (
                "save_confirm_replace_tooltip",
                catalog.save_confirm_replace_tooltip,
            ),
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
            ("no_images", catalog.no_images),
            ("archive_changed", catalog.archive_changed),
            ("could_not_open", catalog.could_not_open),
            ("showing_last", catalog.showing_last),
            ("settings_warning", catalog.settings_warning),
            ("settings_saved", catalog.settings_saved),
            ("progress_warning", catalog.progress_warning),
            ("progress_save_failed", catalog.progress_save_failed),
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
            assert_eq!(fields(catalog).len(), 161, "catalog field list is stale");
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
            assert_eq!(catalog.wide_label(PageOverride::Auto), catalog.wide_auto);
            assert_eq!(
                catalog.wide_label(PageOverride::Single),
                catalog.wide_single
            );
            assert_eq!(
                catalog.wide_label(PageOverride::Spread),
                catalog.wide_spread
            );
            assert!(catalog.wide_tooltip.contains('W'));
            assert_ne!(catalog.wide_auto, catalog.wide_single);
            assert_ne!(catalog.wide_single, catalog.wide_spread);
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
            assert!(
                catalog
                    .progress_warning("raw detail")
                    .contains("raw detail")
            );
            assert!(
                catalog
                    .progress_save_failed("raw error")
                    .contains("raw error")
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
        assert_ne!(EN.open_failed("x", "detail"), JA.open_failed("x", "detail"));
        assert!(EN.settings_warning("raw detail").contains('\n'));
        assert!(EN.save_failed("raw error", true).contains('\n'));
        assert_eq!(EN.language_name(Language::SimplifiedChinese), "简体中文");
        assert_eq!(JA.language_name(Language::Japanese), "日本語");
        assert_eq!(ZH.language_name(Language::Automatic), ZH.automatic);
    }

    #[test]
    fn save_confirm_heading_names_the_target_in_every_language() {
        for catalog in [&EN, &ZH, &JA] {
            let overwrite = catalog.save_confirm_heading("photo.jpg", true);
            let copy = catalog.save_confirm_heading("photo.jpg", false);
            assert!(overwrite.contains("photo.jpg"), "{overwrite}");
            assert!(copy.contains("photo.jpg"), "{copy}");
            assert_ne!(overwrite, copy);
        }
        // Chinese prefixes the verb, Japanese suffixes it.
        assert!(ZH.save_confirm_heading("a.png", true).starts_with("将覆盖"));
        assert!(
            JA.save_confirm_heading("a.png", true)
                .ends_with("置き換えます")
        );
        assert!(
            EN.save_confirm_heading("a.png", true)
                .starts_with("Replace")
        );
        assert_ne!(
            EN.save_confirm_heading("a.png", true),
            ZH.save_confirm_heading("a.png", true)
        );
    }
}
