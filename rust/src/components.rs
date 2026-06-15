// Custom (hand-rolled) UI components for the UI: the top bar (Open · Sort ▾ · Filter ▾ ·
// Search · Settings ▾ · counts · Info · Fullscreen), the dropdown menus, the search box, the
// info panel, and the video player controls bar.
//
// These are immediate-mode: each frame `State` fills a `UiCtx` snapshot, `build()` turns it
// into screen-space rects (`OverlayRect`, drawn by the overlay pipeline) + text (`ui::Line`), and
// `hit()` maps a click to a `UiAction` that `State` then applies. No widget owns any state.

use crate::icons::IconReq;
use crate::ui::Line;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Measured pixel widths of the (static) toolbar labels, populated once at startup from the real
/// font, so buttons size + centre their text exactly instead of estimating per-character.
static LABEL_W: OnceLock<HashMap<String, f32>> = OnceLock::new();
pub fn set_label_widths(m: HashMap<String, f32>) {
    let _ = LABEL_W.set(m);
}
/// Measured width of `text` (falls back to a per-char estimate before the cache is populated).
fn label_w(text: &str) -> f32 {
    LABEL_W
        .get()
        .and_then(|m| m.get(text).copied())
        .unwrap_or_else(|| text.chars().count() as f32 * 7.6)
}

/// A screen-space coloured rectangle in NDC (x, y bottom-left, w, h) — the overlay pipeline's vertex.
/// `round` = [corner_radius_px, width_px, height_px, _]; radius 0 = a plain sharp rect.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OverlayRect {
    pub rect: [f32; 4],
    pub color: [f32; 4],
    pub round: [f32; 4],
}

/// Library sort order (matches the web wall's 7 options).
#[derive(Clone, Copy, PartialEq)]
pub enum SortMode {
    Default,
    NameAsc,
    NameDesc,
    ModifiedNew,
    ModifiedOld,
    CreatedNew,
    CreatedOld,
}
impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Default => "Default (as loaded)",
            SortMode::NameAsc => "Name (A \u{2192} Z)",
            SortMode::NameDesc => "Name (Z \u{2192} A)",
            SortMode::ModifiedNew => "Modified (newest)",
            SortMode::ModifiedOld => "Modified (oldest)",
            SortMode::CreatedNew => "Created (newest)",
            SortMode::CreatedOld => "Created (oldest)",
        }
    }
    pub const ALL: [SortMode; 7] = [
        SortMode::Default,
        SortMode::NameAsc,
        SortMode::NameDesc,
        SortMode::ModifiedNew,
        SortMode::ModifiedOld,
        SortMode::CreatedNew,
        SortMode::CreatedOld,
    ];
}

/// Library type filter.
#[derive(Clone, Copy, PartialEq)]
pub enum Filter {
    All,
    Photos,
    Videos,
    Audio,
}
impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Photos => "Photos",
            Filter::Videos => "Videos",
            Filter::Audio => "Audio",
        }
    }
    pub const ALL: [Filter; 4] = [Filter::All, Filter::Photos, Filter::Videos, Filter::Audio];
}

/// Which top-bar dropdown is open.
#[derive(Clone, Copy, PartialEq)]
pub enum MenuKind {
    Open,
    Sort,
    Filter,
    Dates,
    Settings,
}

/// Which video track-selection menu is open (over the controls bar).
#[derive(Clone, Copy, PartialEq)]
pub enum TrackMenu {
    Audio,
    Sub,
}

/// What a click on the UI means (applied by State).
#[derive(Clone, Copy, PartialEq)]
pub enum UiAction {
    OpenFiles,
    OpenFolder,
    OpenJson, // Open dialog → "From JSON…" (load a manifest of media paths)
    Back, // close the lightbox (✕)
    Fullscreen,
    ToggleSlideshow,
    ToggleInfo,
    ToggleMenu(MenuKind),
    CloseMenu,
    Noop, // consume a click without doing anything (e.g. clicking inside a modal)
    SetSort(SortMode),
    SetFilter(Filter),
    ToggleGifAnim,
    ToggleReflections,
    ToggleMem,
    ToggleShowTitles,
    ActivateSearch,
    ClearSearch,
    SetDateBy(bool),   // false = Modified, true = Created
    ActivateDate(u8),  // focus a date field: 1 = From, 2 = To
    ClearDates,
    VideoPause,
    VideoSeekRel(i32),
    VideoSeekFrac(f32),
    VideoVolume(f32),
    ToggleTrackMenu(TrackMenu),
    SetAudio(i64), // track id (<=0 = off)
    SetSub(i64),
}

/// Snapshot of the playing item's state for the video bar.
pub struct VideoCtx {
    pub pos: f64,
    pub dur: f64,
    pub paused: bool,
    pub vol: f64,
    pub aid: i64,
    pub sid: i64,
    pub visible: bool, // controls shown (auto-hide)
    pub scrub: Option<f32>, // active drag fraction (knob tracks the cursor, not lagging mpv pos)
    pub track_menu: Option<TrackMenu>,           // open audio/sub selection menu
    pub audio_tracks: Vec<(i64, String, bool)>,  // (id, label, selected) — only when its menu is open
    pub sub_tracks: Vec<(i64, String, bool)>,
}

/// Everything the UI needs this frame.
pub struct UiCtx {
    pub w: f32,
    pub h: f32,
    pub sort: SortMode,
    pub filter: Filter,
    pub menu: Option<MenuKind>,
    pub search: String,
    pub search_active: bool,
    pub slideshow: bool,
    pub gif_anim: bool,
    pub reflections: bool,
    pub date_created: bool,     // Dates filter: false = Modified, true = Created
    pub date_from: String,      // "YYYY-MM-DD" lower bound (Dates panel field)
    pub date_to: String,        // "YYYY-MM-DD" upper bound
    pub date_active: u8,        // which date field has focus: 0 none, 1 From, 2 To
    pub caret: usize,           // caret char-index within the focused text field
    pub show_titles: bool,      // Settings: filename on every wall tile
    pub show_mem: bool,         // Settings: show the memory-usage readout
    pub mem_mb: Option<u64>,    // resident set size in MB (when show_mem)
    pub show_info: bool,
    pub total: usize,
    pub ready: usize,
    pub inflight: usize,
    pub focused: bool,                          // an item is open (lightbox) — hide the top bar
    pub info: Option<(String, String, String)>, // (title, filename, meta) for the info card
    pub info_w: [f32; 3], // measured pixel widths of the 3 info lines (0 = fall back to estimate)
    pub video: Option<VideoCtx>,
    pub pointer: [f32; 2], // pixel cursor (for hover highlight)
    pub fullscreen: bool,  // window is currently fullscreen (swaps the fullscreen icon)
    pub caret_on: bool,    // text caret blink phase (true = show the caret this frame)
}

const BAR_H: f32 = 48.0;
const BTN_Y: f32 = 8.0;
const BTN_H: f32 = 32.0;
const ROW_H: f32 = 30.0;

// Shared UI theme — matches the reference web Toolbar (white/10 glass buttons on a black→transparent
// gradient bar; active = solid white + black text; neutral-900 dropdowns ringed white/10).
pub const PANEL_BORDER: [f32; 4] = [1.0, 1.0, 1.0, 0.05]; // faint white ring
pub const BTN_GRAD: f32 = 0.0; // flat fill
pub const BTN_TEXT: [u8; 4] = [235, 235, 240, 255]; // button label colour
pub const BTN_ON: [f32; 4] = [1.0, 1.0, 1.0, 1.0]; // active = bg-white
pub const BTN_ON_TEXT: [u8; 4] = [14, 14, 16, 255]; // active = text-black
pub const PANEL_BG: [f32; 4] = [0.09, 0.09, 0.09, 0.99]; // neutral-900 dropdowns / panels / info
pub const HOVER_OVERLAY: [f32; 4] = [1.0, 1.0, 1.0, 0.10]; // subtle highlight over icon buttons / rows
// Top-bar buttons — very light glass (white @ 5%), so they read as transparent, not gray.
pub const TOPBTN_FILL: [f32; 4] = [1.0, 1.0, 1.0, 0.02];
pub const TOPBTN_HOVER: [f32; 4] = [1.0, 1.0, 1.0, 0.05];
// The wall/lightbox prev-next arrows are transparent black glass (see-through), no border.
pub const ARROW_FILL: [f32; 4] = [0.0, 0.0, 0.0, 0.45];
pub const ARROW_HOVER: [f32; 4] = [0.0, 0.0, 0.0, 0.62];

fn hit(r: [f32; 4], x: f32, y: f32) -> bool {
    x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]
}

/// Lightbox "← Back" button rect — top-left, for any focused item.
fn lightbox_back(_w: f32) -> [f32; 4] {
    [16.0, 14.0, 92.0, 36.0]
}
/// Lightbox Info toggle rect — a circular ⓘ at the top-right.
fn lightbox_info(w: f32) -> [f32; 4] {
    [w - 16.0 - 36.0, 14.0, 36.0, 36.0]
}

/// The field's text with a caret bar inserted at `caret` (only when the field is focused).
fn caret_str(text: &str, caret: usize, active: bool) -> String {
    if !active {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 1);
    for (i, ch) in text.chars().enumerate() {
        if i == caret {
            out.push('|');
        }
        out.push(ch);
    }
    if caret >= text.chars().count() {
        out.push('|');
    }
    out
}

/// Top-bar button rects (pixels). Layout mirrors the web Toolbar.
struct Bar {
    open: [f32; 4],
    slideshow: [f32; 4],
    full: [f32; 4],
    settings: [f32; 4],
    sort: [f32; 4],
    filter: [f32; 4],
    dates: [f32; 4],
    search: [f32; 4],
}
const BRAND_W: f32 = 128.0; // space reserved for the "Cooliris Next" wordmark on the left
fn bar(w: f32) -> Bar {
    // Each button is sized to its measured label width + 14px padding each side.
    let bw = |label: &str| label_w(label) + 28.0;
    // Left group: [Cooliris Next]  Open  |  Slideshow  Fullscreen
    let open = [12.0 + BRAND_W, BTN_Y, bw("Open"), BTN_H];
    let slideshow = [open[0] + open[2] + 20.0, BTN_Y, bw("Slideshow"), BTN_H]; // gap leaves room for a divider
    let full = [slideshow[0] + slideshow[2] + 8.0, BTN_Y, bw("Fullscreen"), BTN_H];
    // Right group, laid out right→left: Search  Dates  Filter  Sort  Settings  [count]
    let search = [w - 16.0 - 210.0, BTN_Y, 210.0, BTN_H];
    let (dw, fw, sw, stw) = (bw("Dates"), bw("Filter"), bw("Sort"), bw("Settings"));
    let dates = [search[0] - 8.0 - dw, BTN_Y, dw, BTN_H];
    let filter = [dates[0] - 8.0 - fw, BTN_Y, fw, BTN_H];
    let sort = [filter[0] - 8.0 - sw, BTN_Y, sw, BTN_H];
    let settings = [sort[0] - 8.0 - stw, BTN_Y, stw, BTN_H];
    Bar { open, slideshow, full, settings, sort, filter, dates, search }
}

/// Dropdown panel + row rects under an anchor button. The width floor fits the longest label
/// ("Default (as loaded)"). Right-side buttons drop their panel right-aligned (like the web), so it
/// never runs off the screen edge.
fn menu(anchor: [f32; 4], n: usize, w: f32) -> ([f32; 4], Vec<[f32; 4]>) {
    let rw = anchor[2].max(208.0);
    let px = if anchor[0] + anchor[2] * 0.5 > w * 0.5 {
        (anchor[0] + anchor[2] - rw).max(8.0) // right-align to the button's right edge
    } else {
        anchor[0]
    };
    let py = anchor[1] + anchor[3] + 6.0;
    let panel = [px, py, rw, ROW_H * n as f32 + 8.0];
    let rows = (0..n)
        .map(|i| [px + 4.0, py + 4.0 + i as f32 * ROW_H, rw - 8.0, ROW_H])
        .collect();
    (panel, rows)
}

/// Settings modal geometry (centred dialog with toggle-switch rows + a close button).
struct SettingsUi {
    panel: [f32; 4],
    close: [f32; 4],
    rows: Vec<[f32; 4]>,     // full clickable row (whole row toggles)
    switches: Vec<[f32; 4]>, // the toggle pill within each row
}
fn settings_layout(w: f32, h: f32, n: usize) -> SettingsUi {
    let pw = 460.0_f32;
    let ph = 60.0 + n as f32 * 62.0 + 14.0;
    let px = (w - pw) * 0.5;
    let py = (h - ph) * 0.5;
    let close = [px + pw - 42.0, py + 16.0, 26.0, 26.0];
    let mut rows = Vec::with_capacity(n);
    let mut switches = Vec::with_capacity(n);
    for i in 0..n {
        let ry = py + 56.0 + i as f32 * 62.0;
        rows.push([px + 18.0, ry, pw - 36.0, 54.0]);
        switches.push([px + 22.0, ry + 15.0, 44.0, 24.0]);
    }
    SettingsUi { panel: [px, py, pw, ph], close, rows, switches }
}

/// Dates filter panel geometry (dropdown under the Dates button).
struct DatesUi {
    panel: [f32; 4],
    modified: [f32; 4],
    created: [f32; 4],
    from: [f32; 4],
    to: [f32; 4],
    clear: [f32; 4],
    done: [f32; 4],
}
fn dates_layout(anchor: [f32; 4]) -> DatesUi {
    let pw = 280.0_f32; // wider, so the labels/fields breathe
    let px = (anchor[0] + anchor[2] - pw).max(8.0); // right-aligned to the Dates button
    let py = anchor[1] + anchor[3] + 6.0;
    let pad = 14.0;
    let iw = pw - pad * 2.0;
    let tabw = iw * 0.5; // joined segmented control (no gap)
    let modified = [px + pad, py + 34.0, tabw, 30.0];
    let created = [px + pad + tabw, py + 34.0, tabw, 30.0];
    let from = [px + pad, py + 94.0, iw, 32.0];
    let to = [px + pad, py + 158.0, iw, 32.0];
    let clear = [px + pad, py + 206.0, 116.0, 32.0];
    let done = [px + pw - pad - 84.0, py + 206.0, 84.0, 32.0];
    DatesUi {
        panel: [px, py, pw, 270.0],
        modified,
        created,
        from,
        to,
        clear,
        done,
    }
}

/// "Open media" modal geometry (centred dialog: a drag-drop zone + Choose files / Choose folder /
/// From JSON). Mirrors the reference OpenDialog (max-w-lg, p-5).
struct OpenUi {
    panel: [f32; 4],
    close: [f32; 4],
    drop: [f32; 4],
    files: [f32; 4],
    folder: [f32; 4],
    json: [f32; 4],
}
fn open_dialog_layout(w: f32, h: f32) -> OpenUi {
    let pw = 520.0_f32.min(w - 32.0);
    let ph = 300.0_f32;
    let px = (w - pw) * 0.5;
    let py = (h - ph) * 0.5;
    let close = [px + pw - 44.0, py + 18.0, 28.0, 28.0];
    let drop = [px + 20.0, py + 62.0, pw - 40.0, 148.0];
    let bh = 38.0;
    let by = py + ph - 20.0 - bh;
    let files = [px + 20.0, by, label_w("Choose files\u{2026}") + 30.0, bh];
    let folder = [files[0] + files[2] + 10.0, by, label_w("Choose folder\u{2026}") + 30.0, bh];
    let jw = label_w("From JSON\u{2026}") + 30.0;
    let json = [px + pw - 20.0 - jw, by, jw, bh]; // right-aligned (a flex spacer pushes it over)
    OpenUi { panel: [px, py, pw, ph], close, drop, files, folder, json }
}

/// The Settings rows (label, description). `on` is read from the UiCtx in build()/hit_test().
const SETTINGS_ROWS: [(&str, &str); 4] = [
    ("Show titles", "Show every file's name on the wall."),
    ("Animate GIFs on the wall", "Play GIFs on the wall (heavier)."),
    ("Reflections", "Mirror the bottom row as a reflection."),
    ("Memory usage", "Show and log resident memory (RSS)."),
];

/// Video controls bar layout (pixels).
struct VLayout {
    bar: [f32; 4],
    seek: [f32; 4],
    back: [f32; 4],
    play: [f32; 4],
    fwd: [f32; 4],
    vol: [f32; 4],
    audio: [f32; 4],
    subs: [f32; 4],
    full: [f32; 4],
}
fn video_layout(w: f32, h: f32) -> VLayout {
    // Single row, like the reference player:
    //   ▶  [vol]  0:01 / 5:01  [════ seek ════]   ⏪ ⏩  subs  audio  ⛶
    let bh = 44.0;
    let by = h - bh;
    let cy = by + (bh - 26.0) * 0.5; // 26px-tall buttons, vertically centred
    let my = by + bh * 0.5; // bar mid-line (sliders/time sit on it)
    let play = [16.0, cy, 26.0, 26.0];
    let vol = [play[0] + play[2] + 14.0, my - 4.0, 80.0, 8.0];
    // Time text occupies a fixed slot after the volume slider; the seek track fills the gap to the
    // right-hand button cluster. Wide enough for "H:MM:SS / H:MM:SS".
    let time_w = 124.0;
    // Right cluster (right → left): fullscreen, audio, subs, skip-fwd, skip-back.
    let full = [w - 16.0 - 26.0, cy, 26.0, 26.0];
    let audio = [full[0] - 14.0 - 26.0, cy, 26.0, 26.0];
    let subs = [audio[0] - 10.0 - 26.0, cy, 26.0, 26.0];
    let fwd = [subs[0] - 14.0 - 26.0, cy, 26.0, 26.0];
    let back = [fwd[0] - 8.0 - 26.0, cy, 26.0, 26.0];
    let seek_x = vol[0] + vol[2] + 12.0 + time_w;
    let seek = [seek_x, my - 3.0, (back[0] - 16.0 - seek_x).max(40.0), 6.0];
    VLayout {
        bar: [0.0, by, w, bh],
        seek,
        back,
        play,
        fwd,
        vol,
        audio,
        subs,
        full,
    }
}

/// Pixel x where the search box text starts (for click-to-place caret).
pub fn search_text_x0(w: f32) -> f32 {
    bar(w).search[0] + 10.0
}
/// Pixel x where a Dates field's text starts (which: 1 = From, 2 = To).
pub fn date_text_x0(w: f32, which: u8) -> f32 {
    let du = dates_layout(bar(w).dates);
    (if which == 1 { du.from } else { du.to })[0] + 8.0
}

/// Fraction (0..1) along the seek track at pixel x — for drag-scrubbing the timeline.
pub fn video_seek_frac(w: f32, h: f32, x: f32) -> f32 {
    let vl = video_layout(w.max(1.0), h.max(1.0));
    ((x - vl.seek[0]) / vl.seek[2].max(1.0)).clamp(0.0, 1.0)
}
/// Fraction (0..1) along the volume slider at pixel x — for drag-setting the volume.
pub fn video_vol_frac(w: f32, h: f32, x: f32) -> f32 {
    let vl = video_layout(w.max(1.0), h.max(1.0));
    ((x - vl.vol[0]) / vl.vol[2].max(1.0)).clamp(0.0, 1.0)
}

/// Track-selection dropdown geometry, growing UPWARD from a controls-bar button.
fn track_menu_layout(anchor: [f32; 4], n: usize, w: f32) -> ([f32; 4], Vec<[f32; 4]>) {
    let rw = 240.0_f32;
    let rh = 28.0_f32;
    let ph = n as f32 * rh + 8.0;
    // Centre on the button, but keep the whole panel on screen (audio/subs sit near the right edge).
    let px = (anchor[0] + anchor[2] * 0.5 - rw * 0.5).clamp(8.0, (w - rw - 8.0).max(8.0));
    // Above the button, but never off the top of the screen (so every track stays visible/clickable).
    let py = (anchor[1] - 8.0 - ph).max(8.0);
    let rows = (0..n)
        .map(|i| [px + 4.0, py + 4.0 + i as f32 * rh, rw - 8.0, rh])
        .collect();
    ([px, py, rw, ph], rows)
}

/// Build this frame's UI: overlay rects + text lines + SVG icons.
pub fn build(c: &UiCtx) -> (Vec<OverlayRect>, Vec<Line>, Vec<IconReq>) {
    let (w, h) = (c.w.max(1.0), c.h.max(1.0));
    let mut rects = Vec::new();
    let mut lines = Vec::new();
    let mut icons: Vec<IconReq> = Vec::new();
    // pixel box (top-left origin) → overlay NDC rect (bottom-left + size).
    let nd = |r: [f32; 4]| {
        [
            r[0] / w * 2.0 - 1.0,
            1.0 - (r[1] + r[3]) / h * 2.0,
            r[2] / w * 2.0,
            r[3] / h * 2.0,
        ]
    };
    let white_rect = [1.0, 1.0, 1.0, 0.95];
    // An SVG icon centred (square) inside the pixel rect `$r`, at `$sz` px, tinted `$tint`.
    macro_rules! icon {
        ($r:expr, $sz:expr, $name:expr, $tint:expr $(,)?) => {{
            let ir = $r;
            let s: f32 = $sz;
            icons.push(IconReq {
                rect: [ir[0] + (ir[2] - s) * 0.5, ir[1] + (ir[3] - s) * 0.5, s, s],
                name: $name,
                tint: $tint,
            });
        }};
    }
    macro_rules! rect {
        ($r:expr, $c:expr $(,)?) => {
            rects.push(OverlayRect { rect: nd($r), color: $c, round: [0.0, 0.0, 0.0, 0.0] })
        };
    }
    // Rounded pill: $rad px corner radius (pass a big radius for a fully-rounded pill).
    macro_rules! pill {
        ($r:expr, $c:expr, $rad:expr $(,)?) => {{
            let pr = $r;
            rects.push(OverlayRect { rect: nd(pr), color: $c, round: [$rad, pr[2], pr[3], 0.0] })
        }};
    }
    // Rounded pill with a vertical gradient ($grad = strength: lighter top, darker bottom) — the
    // DAW button look.
    macro_rules! gpill {
        ($r:expr, $c:expr, $rad:expr, $grad:expr $(,)?) => {{
            let pr = $r;
            rects.push(OverlayRect { rect: nd(pr), color: $c, round: [$rad, pr[2], pr[3], $grad] })
        }};
    }
    // A panel: a hairline border ring under the solid fill (borders are panel-only).
    macro_rules! panel {
        ($r:expr, $rad:expr $(,)?) => {{
            let pr = $r;
            pill!([pr[0] - 1.0, pr[1] - 1.0, pr[2] + 2.0, pr[3] + 2.0], PANEL_BORDER, $rad + 1.0);
            pill!(pr, PANEL_BG, $rad);
        }};
    }
    macro_rules! label {
        ($t:expr, $x:expr, $y:expr, $s:expr, $col:expr $(,)?) => {
            lines.push(Line { text: $t, x: $x, y: $y, size: $s, color: $col })
        };
    }
    let white = BTN_TEXT;
    // An icon "button": a subtle rounded highlight behind the icon when hovered, then the icon.
    macro_rules! icon_btn {
        ($r:expr, $sz:expr, $name:expr, $tint:expr $(,)?) => {{
            let r = $r;
            if hit(r, c.pointer[0], c.pointer[1]) {
                pill!(r, HOVER_OVERLAY, 6.0);
            }
            icon!(r, $sz, $name, $tint);
        }};
    }

    // Top bar — hidden while an item is open (the lightbox is uncluttered).
    let b = bar(w);
    if !c.focused {
    // Top bar — fully transparent (no strip): the white/10 buttons are real glass over the wall,
    // not gray pills on a dark bar.
    // White/10 glass buttons (reference style); active = solid white + black text.
    let pill_on = BTN_ON; // active = white pill, black text
    let txt_on = BTN_ON_TEXT;
    let rad = BTN_H * 0.5; // 100%-round — full pill
    let grad = BTN_GRAD; // vertical gradient strength for the dark buttons
    // A pill button: a flat fill (white when active, lighter when hovered), then a centred label.
    macro_rules! btn {
        ($rect:expr, $text:expr, $on:expr) => {{
            let br = $rect;
            let on: bool = $on;
            let t: String = $text;
            let hov = hit(br, c.pointer[0], c.pointer[1]);
            if on {
                pill!(br, pill_on, rad);
            } else {
                gpill!(br, if hov { TOPBTN_HOVER } else { TOPBTN_FILL }, rad, grad);
            }
            let tw = label_w(&t); // measured width → exact horizontal centring
            // Vertically centre the label in the button (the glyph sits ~2px low at +9).
            label!(t, br[0] + (br[2] - tw) * 0.5, br[1] + br[3] * 0.5 - 9.0, 14.0, if on { txt_on } else { white });
        }};
    }
    // Wordmark — "Cooliris" with a tucked-in "Next", vertically centred like the buttons.
    label!("Cooliris".into(), 14.0, BTN_Y + BTN_H * 0.5 - 17.0 * 0.64, 17.0, white);
    label!("Next".into(), 84.0, BTN_Y + BTN_H * 0.5 - 15.0 * 0.64, 15.0, [150, 150, 160, 220]);
    btn!(b.open, "Open".into(), c.menu == Some(MenuKind::Open));
    // divider between Open and Slideshow
    rect!([b.open[0] + b.open[2] + 9.0, BTN_Y + 5.0, 1.0, BTN_H - 10.0], [1.0, 1.0, 1.0, 0.15]);
    btn!(b.slideshow, if c.slideshow { "Stop".into() } else { "Slideshow".into() }, c.slideshow);
    btn!(b.full, "Fullscreen".into(), false);
    btn!(b.settings, "Settings".into(), c.menu == Some(MenuKind::Settings));
    btn!(b.sort, "Sort".into(), c.menu == Some(MenuKind::Sort));
    btn!(b.filter, "Filter".into(), c.menu == Some(MenuKind::Filter));
    let dates_set = !c.date_from.is_empty() || !c.date_to.is_empty();
    btn!(
        b.dates,
        if dates_set { "Dates \u{2022}".into() } else { "Dates".into() },
        c.menu == Some(MenuKind::Dates) || dates_set
    );
    // Search (input pill) — flat fill, no border (borders are panel-only now).
    gpill!(b.search, if c.search_active { TOPBTN_HOVER } else { TOPBTN_FILL }, rad, grad);
    if c.search.is_empty() && !c.search_active {
        label!("Search\u{2026}".into(), b.search[0] + 14.0, b.search[1] + 8.0, 14.0, [160, 160, 170, 220]);
    } else {
        label!(caret_str(&c.search, c.caret, c.search_active && c.caret_on), b.search[0] + 14.0, b.search[1] + 8.0, 14.0, white);
        if !c.search.is_empty() {
            label!("\u{2715}".into(), b.search[0] + b.search[2] - 22.0, b.search[1] + 8.0, 14.0, [200, 200, 210, 230]);
        }
    }
    // Count (loaded/total) + memory readout — just left of Settings, laid out right→left, so
    // loading progress is easy to see. A small blue dot flags "still loading".
    let mut info_x = b.settings[0] - 12.0; // right edge of the readout cluster
    if c.total > 0 {
        let t = format!("{}/{}", c.ready.min(c.total), c.total);
        let cw = t.chars().count() as f32 * 7.6;
        label!(t, info_x - cw, BTN_Y + BTN_H * 0.5 - 8.3, 13.0, [185, 185, 195, 235]);
        info_x -= cw + 8.0;
        if c.inflight > 0 {
            pill!([info_x - 9.0, BTN_Y + 12.0, 9.0, 9.0], [0.45, 0.75, 1.0, 0.95], 4.5);
            info_x -= 9.0 + 8.0;
        }
    }
    if let Some(mb) = c.mem_mb {
        let m = format!("{mb} MB");
        let mw = m.chars().count() as f32 * 7.0;
        label!(m, info_x - mw, BTN_Y + BTN_H * 0.5 - 7.7, 12.0, [150, 200, 160, 220]);
    }

    // Dropdowns (Open / Sort / Filter) and the Settings modal.
    if c.menu == Some(MenuKind::Settings) {
        let on = [c.show_titles, c.gif_anim, c.reflections, c.show_mem];
        let su = settings_layout(w, h, SETTINGS_ROWS.len());
        rect!([0.0, 0.0, w, h], [0.0, 0.0, 0.0, 0.5]); // scrim
        panel!(su.panel, 16.0);
        label!("Settings".into(), su.panel[0] + 22.0, su.panel[1] + 18.0, 18.0, white);
        label!("\u{2715}".into(), su.close[0] + 5.0, su.close[1] + 3.0, 17.0, [200, 200, 210, 230]);
        for (i, (lbl, desc)) in SETTINGS_ROWS.iter().enumerate() {
            let sw = su.switches[i];
            if hit(su.rows[i], c.pointer[0], c.pointer[1]) {
                pill!(su.rows[i], [1.0, 1.0, 1.0, 0.05], 10.0);
            }
            // Toggle switch: a rounded track + a circular knob.
            pill!(sw, if on[i] { [0.30, 0.62, 0.45, 1.0] } else { [1.0, 1.0, 1.0, 0.16] }, sw[3] * 0.5);
            let kx = if on[i] { sw[0] + sw[2] - 22.0 } else { sw[0] + 2.0 };
            pill!([kx, sw[1] + 2.0, 20.0, sw[3] - 4.0], white_rect, (sw[3] - 4.0) * 0.5); // knob
            let tx = sw[0] + sw[2] + 16.0;
            label!((*lbl).into(), tx, sw[1] - 4.0, 15.0, white);
            label!((*desc).into(), tx, sw[1] + 16.0, 12.0, [160, 160, 170, 210]);
        }
    } else if c.menu == Some(MenuKind::Dates) {
        let du = dates_layout(b.dates);
        let lbl = [255, 255, 255, 120]; // dim section labels (~white/47)
        let ph = [255, 255, 255, 70]; // dim placeholders (~white/27)
        let tab_off = [255, 255, 255, 170]; // inactive tab text (~white/67)
        panel!(du.panel, 14.0);
        label!("Filter by".into(), du.from[0], du.panel[1] + 14.0, 12.0, lbl);
        // A dark inset field: a subtle edge (brighter when focused) + a near-black fill.
        macro_rules! field {
            ($r:expr, $active:expr) => {{
                let fr = $r;
                pill!([fr[0] - 1.0, fr[1] - 1.0, fr[2] + 2.0, fr[3] + 2.0], [1.0, 1.0, 1.0, if $active { 0.22 } else { 0.07 }], 9.0);
                pill!(fr, [0.0, 0.0, 0.0, 0.45], 8.0);
            }};
        }
        // Modified / Created — a joined, ringed segmented control with a white pill on the active tab.
        let seg = [du.modified[0], du.modified[1], du.modified[2] + du.created[2], du.modified[3]];
        pill!([seg[0] - 1.0, seg[1] - 1.0, seg[2] + 2.0, seg[3] + 2.0], [1.0, 1.0, 1.0, 0.15], 9.0); // ring
        pill!(seg, [0.0, 0.0, 0.0, 0.25], 8.0); // dark track
        let active = if c.date_created { du.created } else { du.modified };
        pill!([active[0] + 2.0, active[1] + 2.0, active[2] - 4.0, active[3] - 4.0], BTN_ON, 6.0); // sliding pill
        let mon = !c.date_created;
        label!("Modified".into(), du.modified[0] + (du.modified[2] - label_w("Modified")) * 0.5, du.modified[1] + 7.0, 13.0, if mon { txt_on } else { tab_off });
        label!("Created".into(), du.created[0] + (du.created[2] - label_w("Created")) * 0.5, du.created[1] + 7.0, 13.0, if c.date_created { txt_on } else { tab_off });
        // From field.
        let fa = c.date_active == 1;
        label!("From".into(), du.from[0], du.from[1] - 20.0, 12.0, lbl);
        field!(du.from, fa);
        if c.date_from.is_empty() && !fa {
            label!("YYYY-MM-DD".into(), du.from[0] + 8.0, du.from[1] + 8.0, 13.0, ph);
        } else {
            label!(caret_str(&c.date_from, c.caret, fa && c.caret_on), du.from[0] + 8.0, du.from[1] + 8.0, 13.0, white);
        }
        // To field.
        let ta = c.date_active == 2;
        label!("To".into(), du.to[0], du.to[1] - 20.0, 12.0, lbl);
        field!(du.to, ta);
        if c.date_to.is_empty() && !ta {
            label!("YYYY-MM-DD".into(), du.to[0] + 8.0, du.to[1] + 8.0, 13.0, ph);
        } else {
            label!(caret_str(&c.date_to, c.caret, ta && c.caret_on), du.to[0] + 8.0, du.to[1] + 8.0, 13.0, white);
        }
        // Clear = dark (like the inputs); Done = solid white.
        field!(du.clear, false);
        label!("Clear dates".into(), du.clear[0] + (du.clear[2] - label_w("Clear dates")) * 0.5, du.clear[1] + 8.0, 13.0, white);
        pill!(du.done, BTN_ON, 8.0);
        label!("Done".into(), du.done[0] + (du.done[2] - label_w("Done")) * 0.5, du.done[1] + 8.0, 13.0, txt_on);
        // Footer hint (reference: text-white/35).
        let by = if c.date_created { "created" } else { "modified" };
        label!(format!("Filtering by file {by} date."), du.from[0], du.panel[1] + du.panel[3] - 22.0, 11.0, [255, 255, 255, 89]);
    } else if c.menu == Some(MenuKind::Open) {
        // "Open media" modal — matches the reference OpenDialog: a dashed drop zone (click → choose
        // a folder) plus Choose files / Choose folder / From JSON. Drag-drop works anywhere on the
        // window, so the zone here is the click target + visual cue.
        let ou = open_dialog_layout(w, h);
        rect!([0.0, 0.0, w, h], [0.0, 0.0, 0.0, 0.7]); // scrim (bg-black/70)
        panel!(ou.panel, 16.0); // rounded-2xl neutral-900, ring-white/10
        label!("Open media".into(), ou.panel[0] + 22.0, ou.panel[1] + 20.0, 18.0, white);
        // Close ✕ (top-right).
        if hit(ou.close, c.pointer[0], c.pointer[1]) {
            pill!(ou.close, HOVER_OVERLAY, ou.close[3] * 0.5);
        }
        label!("\u{2715}".into(), ou.close[0] + 8.0, ou.close[1] + 5.0, 17.0, [200, 200, 210, 230]);
        // Drop zone — subtle fill (brighter on hover) under a dashed white border.
        let dr = ou.drop;
        let dhov = hit(dr, c.pointer[0], c.pointer[1]);
        pill!(dr, if dhov { [1.0, 1.0, 1.0, 0.10] } else { [1.0, 1.0, 1.0, 0.05] }, 12.0);
        let dcol = if dhov { [1.0, 1.0, 1.0, 0.70] } else { [1.0, 1.0, 1.0, 0.22] };
        let (dl, gap, th, ins) = (10.0_f32, 7.0_f32, 1.5_f32, 12.0_f32); // dash len, gap, thickness, corner inset
        let (mut x, x1) = (dr[0] + ins, dr[0] + dr[2] - ins);
        while x < x1 {
            let dw = dl.min(x1 - x);
            rect!([x, dr[1], dw, th], dcol); // top edge
            rect!([x, dr[1] + dr[3] - th, dw, th], dcol); // bottom edge
            x += dl + gap;
        }
        let (mut y, y1) = (dr[1] + ins, dr[1] + dr[3] - ins);
        while y < y1 {
            let dh = dl.min(y1 - y);
            rect!([dr[0], y, th, dh], dcol); // left edge
            rect!([dr[0] + dr[2] - th, y, th, dh], dcol); // right edge
            y += dl + gap;
        }
        // Two centred lines of guidance text.
        let t1 = "Drag & drop files or a folder here";
        let t2 = "or click to choose a folder \u{00b7} images, videos, audio";
        let cy = dr[1] + dr[3] * 0.5;
        label!(t1.into(), dr[0] + (dr[2] - label_w(t1)) * 0.5, cy - 16.0, 14.0, white);
        label!(t2.into(), dr[0] + (dr[2] - label_w(t2)) * 0.5, cy + 4.0, 12.0, [255, 255, 255, 110]);
        // Buttons — white/15 glass (reference Btn).
        macro_rules! obtn {
            ($r:expr, $t:expr) => {{
                let r = $r;
                let hov = hit(r, c.pointer[0], c.pointer[1]);
                pill!(r, if hov { [1.0, 1.0, 1.0, 0.22] } else { [1.0, 1.0, 1.0, 0.13] }, 9.0);
                label!($t.into(), r[0] + (r[2] - label_w($t)) * 0.5, r[1] + r[3] * 0.5 - 9.0, 14.0, white);
            }};
        }
        obtn!(ou.files, "Choose files\u{2026}");
        obtn!(ou.folder, "Choose folder\u{2026}");
        obtn!(ou.json, "From JSON\u{2026}");
    } else if let Some(kind) = c.menu {
        let (anchor, items): ([f32; 4], Vec<(String, bool)>) = match kind {
            MenuKind::Open => unreachable!("open is drawn as a modal above"),
            MenuKind::Sort => (
                b.sort,
                SortMode::ALL
                    .iter()
                    .map(|m| (m.label().to_string(), *m == c.sort))
                    .collect(),
            ),
            MenuKind::Filter => (
                b.filter,
                Filter::ALL
                    .iter()
                    .map(|f| (f.label().to_string(), *f == c.filter))
                    .collect(),
            ),
            MenuKind::Dates => unreachable!("dates is drawn as a panel above"),
            MenuKind::Settings => unreachable!("settings is drawn as a modal above"),
        };
        let (panel, rows) = menu(anchor, items.len(), w);
        // rounded-xl neutral-900 panel
        panel!(panel, 12.0);
        for (r, (text, current)) in rows.iter().zip(items) {
            let hov = hit(*r, c.pointer[0], c.pointer[1]);
            if current {
                pill!(*r, [1.0, 1.0, 1.0, 0.15], 7.0);
            } else if hov {
                pill!(*r, [1.0, 1.0, 1.0, 0.10], 7.0);
            }
            label!(text, r[0] + 10.0, r[1] + 6.0, 14.0, if current { white } else { [200, 200, 210, 235] });
        }
    }
    } // end top bar (hidden while an item is focused)

    // Info pill — bottom-centre while focused (above the video controls bar): title + position,
    // plus filename + date when Info is on. Hidden with a video's controls when it goes idle.
    let info_hidden = c.video.as_ref().map_or(false, |v| !v.visible);
    if !info_hidden {
        if let Some((title, line2, line3)) = &c.info {
            let three = !line3.is_empty();
            let ph = if three { 66.0 } else { 46.0 };
            let bottom = if c.video.is_some() { h - 44.0 - 14.0 } else { h - 18.0 };
            let py = bottom - ph;
            // Measured line widths (exact horizontal centring); fall back to an estimate if unset.
            let wpx = |t: &str, per: f32| t.chars().count() as f32 * per;
            let iw = [
                if c.info_w[0] > 0.0 { c.info_w[0] } else { wpx(title, 7.8) },
                if c.info_w[1] > 0.0 { c.info_w[1] } else { wpx(line2, 6.6) },
                if c.info_w[2] > 0.0 { c.info_w[2] } else { wpx(line3, 6.6) },
            ];
            let maxw = (w - 40.0).max(220.0);
            let pw = (iw[0].max(iw[1]).max(iw[2]) + 40.0).clamp(360.0, maxw);
            let px = (w - pw) * 0.5;
            panel!([px, py, pw, ph], 10.0);
            // Each line centred on screen (= on the panel) using its measured width.
            label!(title.clone(), (w - iw[0]) * 0.5, py + 9.0, 14.0, white);
            label!(line2.clone(), (w - iw[1]) * 0.5, py + 28.0, 12.0, [175, 175, 185, 225]);
            if three {
                label!(line3.clone(), (w - iw[2]) * 0.5, py + 46.0, 12.0, [150, 150, 160, 210]);
            }
        }
    }

    // Video controls bar — single row of bare glyphs on a translucent strip (over the video).
    // (No separate title box — the Info card/toggle is the single source of item info.)
    if let Some(v) = &c.video {
        if v.visible {
            let vl = video_layout(w, h);
            rect!(vl.bar, [0.0, 0.0, 0.0, 0.55]);
            let dim = [200, 200, 210, 235];
            let off = [150, 150, 160, 200];
            // play / pause
            icon_btn!(vl.play, 22.0, if v.paused { "play" } else { "pause" }, white);
            // volume slider (track · fill · knob)
            let vf = (v.vol / 100.0).clamp(0.0, 1.0) as f32;
            rect!(vl.vol, [1.0, 1.0, 1.0, 0.20]);
            rect!([vl.vol[0], vl.vol[1], vl.vol[2] * vf, vl.vol[3]], [0.9, 0.9, 0.95, 0.9]);
            rect!([vl.vol[0] + vl.vol[2] * vf - 4.0, vl.vol[1] - 4.0, 8.0, vl.vol[3] + 8.0], white_rect);
            // time "pos / dur"
            label!(
                format!("{} / {}", fmt_time(v.pos), fmt_time(v.dur)),
                vl.vol[0] + vl.vol[2] + 14.0,
                vl.bar[1] + 14.0,
                13.0,
                dim,
            );
            // seek track · fill · knob (knob follows the live drag fraction when scrubbing)
            let frac = match v.scrub {
                Some(f) => f.clamp(0.0, 1.0),
                None if v.dur > 0.0 => (v.pos / v.dur).clamp(0.0, 1.0) as f32,
                None => 0.0,
            };
            rect!(vl.seek, [1.0, 1.0, 1.0, 0.20]);
            rect!([vl.seek[0], vl.seek[1], vl.seek[2] * frac, vl.seek[3]], [0.9, 0.9, 0.95, 0.95]);
            rect!([vl.seek[0] + vl.seek[2] * frac - 4.0, vl.seek[1] - 5.0, 8.0, vl.seek[3] + 10.0], white_rect);
            // Hover readout: a black box with white time text at the cursor over the seek track.
            let pt = c.pointer;
            if v.dur > 0.0 && pt[1] >= vl.bar[1] && pt[0] >= vl.seek[0] && pt[0] <= vl.seek[0] + vl.seek[2] {
                let hf = ((pt[0] - vl.seek[0]) / vl.seek[2]).clamp(0.0, 1.0) as f64;
                let ht = fmt_time(hf * v.dur);
                // Exact width from per-character (digit/colon) measurements → precise centring.
                let tw: f32 = ht.chars().map(|c| label_w(&c.to_string())).sum();
                let bw = tw + 18.0;
                let bx = (pt[0] - bw * 0.5).clamp(4.0, w - bw - 4.0);
                let tip_y = vl.bar[1] - 30.0;
                pill!([bx, tip_y, bw, 22.0], [0.0, 0.0, 0.0, 0.88], 6.0);
                label!(ht, bx + (bw - tw) * 0.5, tip_y + 22.0 * 0.5 - 13.0 * 0.64, 13.0, white);
            }
            // right cluster: skip-back · skip-fwd · subtitles · audio · fullscreen (SVG icons)
            icon_btn!(vl.back, 22.0, "back10", white);
            icon_btn!(vl.fwd, 22.0, "fwd10", white);
            let subs_on = v.sid > 0 || v.track_menu == Some(TrackMenu::Sub);
            let aud_on = v.aid > 0 || v.track_menu == Some(TrackMenu::Audio);
            icon_btn!(vl.subs, 21.0, "cc", if subs_on { white } else { off });
            icon_btn!(vl.audio, 21.0, "audio", if aud_on { white } else { off });
            icon_btn!(vl.full, 20.0, if c.fullscreen { "fullscreen-exit" } else { "fullscreen" }, white);

            // Audio / subtitle track menu (opens above its button).
            if let Some(tm) = v.track_menu {
                let (anchor, tracks, off_sel) = match tm {
                    TrackMenu::Audio => (vl.audio, &v.audio_tracks, v.aid <= 0),
                    TrackMenu::Sub => (vl.subs, &v.sub_tracks, v.sid <= 0),
                };
                let (panel, rows) = track_menu_layout(anchor, tracks.len() + 1, c.w);
                panel!(panel, 12.0);
                if off_sel {
                    rect!(rows[0], [1.0, 1.0, 1.0, 0.22]);
                } else if hit(rows[0], c.pointer[0], c.pointer[1]) {
                    rect!(rows[0], [1.0, 1.0, 1.0, 0.12]);
                }
                label!("Off".into(), rows[0][0] + 8.0, rows[0][1] + 6.0, 13.0, white);
                for (r, (_id, lbl, sel)) in rows[1..].iter().zip(tracks.iter()) {
                    if *sel {
                        rect!(*r, [1.0, 1.0, 1.0, 0.22]);
                    } else if hit(*r, c.pointer[0], c.pointer[1]) {
                        rect!(*r, [1.0, 1.0, 1.0, 0.12]);
                    }
                    let mut t = lbl.clone();
                    if t.chars().count() > 30 {
                        t = t.chars().take(29).collect::<String>() + "\u{2026}";
                    }
                    label!(t, r[0] + 8.0, r[1] + 6.0, 13.0, white);
                }
            }
        }
    }

    // Lightbox controls — top-right, for any focused item (the top bar is hidden while focused):
    // an Info toggle and a close (✕). For a video they hide together with the controls bar when
    // it goes idle, so nothing is left floating over the picture.
    let controls_hidden = c.video.as_ref().map_or(false, |v| !v.visible);
    if c.focused && !controls_hidden {
        // "← Back" button, top-left — transparent glass like the arrows.
        let bk = lightbox_back(w);
        let hov = hit(bk, c.pointer[0], c.pointer[1]);
        gpill!(bk, if hov { ARROW_HOVER } else { ARROW_FILL }, 8.0, BTN_GRAD);
        label!("\u{2190} Back".into(), bk[0] + 16.0, bk[1] + 10.0, 14.0, white);
        // Circular Info (ⓘ) toggle, top-right — transparent glass (white when on).
        let ib = lightbox_info(w);
        let on = c.show_info;
        let hov = hit(ib, c.pointer[0], c.pointer[1]);
        if on {
            pill!(ib, BTN_ON, ib[3] * 0.5);
        } else {
            gpill!(ib, if hov { ARROW_HOVER } else { ARROW_FILL }, ib[3] * 0.5, BTN_GRAD);
        }
        icon!(ib, 19.0, "info", if on { BTN_ON_TEXT } else { BTN_TEXT });
    }

    (rects, lines, icons)
}

/// Seconds → "M:SS" (or "H:MM:SS").
pub fn fmt_time(s: f64) -> String {
    if !s.is_finite() || s < 0.0 {
        return "0:00".into();
    }
    let t = s as u64;
    let (h, m, sec) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Map a click to a UI action (None = not on the UI). An open menu captures the click.
pub fn hit_test(c: &UiCtx, x: f32, y: f32) -> Option<UiAction> {
    // Open dropdown captures first: a row picks, anything else closes (the button re-toggles).
    if let Some(kind) = c.menu {
        // Settings is a modal: rows toggle, ✕/outside closes, inside is consumed.
        if kind == MenuKind::Settings {
            let su = settings_layout(c.w, c.h, SETTINGS_ROWS.len());
            if hit(su.close, x, y) {
                return Some(UiAction::CloseMenu);
            }
            for (i, row) in su.rows.iter().enumerate() {
                if hit(*row, x, y) {
                    return Some(match i {
                        0 => UiAction::ToggleShowTitles,
                        1 => UiAction::ToggleGifAnim,
                        2 => UiAction::ToggleReflections,
                        _ => UiAction::ToggleMem,
                    });
                }
            }
            return Some(if hit(su.panel, x, y) {
                UiAction::Noop
            } else {
                UiAction::CloseMenu
            });
        }
        // Open is a modal: ✕/outside closes, the 3 buttons act, the drop zone chooses a folder.
        if kind == MenuKind::Open {
            let ou = open_dialog_layout(c.w, c.h);
            if hit(ou.close, x, y) {
                return Some(UiAction::CloseMenu);
            }
            if hit(ou.files, x, y) {
                return Some(UiAction::OpenFiles);
            }
            if hit(ou.folder, x, y) {
                return Some(UiAction::OpenFolder);
            }
            if hit(ou.json, x, y) {
                return Some(UiAction::OpenJson);
            }
            if hit(ou.drop, x, y) {
                return Some(UiAction::OpenFolder); // clicking the drop zone chooses a folder
            }
            return Some(if hit(ou.panel, x, y) {
                UiAction::Noop
            } else {
                UiAction::CloseMenu
            });
        }
        // Dates is a custom panel (tabs, From/To fields, Clear/Done).
        if kind == MenuKind::Dates {
            let du = dates_layout(bar(c.w).dates);
            if hit(du.modified, x, y) {
                return Some(UiAction::SetDateBy(false));
            }
            if hit(du.created, x, y) {
                return Some(UiAction::SetDateBy(true));
            }
            if hit(du.from, x, y) {
                return Some(UiAction::ActivateDate(1));
            }
            if hit(du.to, x, y) {
                return Some(UiAction::ActivateDate(2));
            }
            if hit(du.clear, x, y) {
                return Some(UiAction::ClearDates);
            }
            if hit(du.done, x, y) {
                return Some(UiAction::CloseMenu);
            }
            if hit(du.panel, x, y) {
                return Some(UiAction::Noop);
            }
            if !hit(bar(c.w).dates, x, y) {
                return Some(UiAction::CloseMenu);
            }
            // on the Dates button → fall through to toggle it closed
        }
        let (anchor, n) = match kind {
            MenuKind::Open => unreachable!("open is a modal, handled above"),
            MenuKind::Sort => (bar(c.w).sort, SortMode::ALL.len()),
            MenuKind::Filter => (bar(c.w).filter, Filter::ALL.len()),
            MenuKind::Dates => (bar(c.w).dates, 0),
            MenuKind::Settings => unreachable!(),
        };
        let (_, rows) = menu(anchor, n, c.w);
        for (i, r) in rows.iter().enumerate() {
            if hit(*r, x, y) {
                return Some(match kind {
                    MenuKind::Open => unreachable!(),
                    MenuKind::Sort => UiAction::SetSort(SortMode::ALL[i]),
                    MenuKind::Filter => UiAction::SetFilter(Filter::ALL[i]),
                    MenuKind::Dates => unreachable!(),
                    MenuKind::Settings => unreachable!(),
                });
            }
        }
        if !hit(anchor, x, y) {
            return Some(UiAction::CloseMenu);
        }
    }

    // Lightbox controls — top-right, while an item is focused (hidden when a video's controls are
    // idle, matching what's drawn).
    let controls_hidden = c.video.as_ref().map_or(false, |v| !v.visible);
    if c.focused && !controls_hidden {
        if hit(lightbox_back(c.w), x, y) {
            return Some(UiAction::Back);
        }
        if hit(lightbox_info(c.w), x, y) {
            return Some(UiAction::ToggleInfo);
        }
    }

    // Video controls.
    if let Some(v) = &c.video {
        if v.visible {
            let vl = video_layout(c.w, c.h);
            // An open track menu (above the bar) captures clicks first.
            if let Some(tm) = v.track_menu {
                let (anchor, tracks) = match tm {
                    TrackMenu::Audio => (vl.audio, &v.audio_tracks),
                    TrackMenu::Sub => (vl.subs, &v.sub_tracks),
                };
                let (panel, rows) = track_menu_layout(anchor, tracks.len() + 1, c.w);
                if hit(rows[0], x, y) {
                    return Some(match tm {
                        TrackMenu::Audio => UiAction::SetAudio(-1),
                        TrackMenu::Sub => UiAction::SetSub(-1),
                    });
                }
                for (r, (id, _l, _s)) in rows[1..].iter().zip(tracks.iter()) {
                    if hit(*r, x, y) {
                        return Some(match tm {
                            TrackMenu::Audio => UiAction::SetAudio(*id),
                            TrackMenu::Sub => UiAction::SetSub(*id),
                        });
                    }
                }
                if hit(panel, x, y) {
                    return Some(UiAction::Noop); // inside the panel, not a row → consume
                }
                if !hit(vl.bar, x, y) {
                    return Some(UiAction::ToggleTrackMenu(tm)); // click-away closes
                }
                // on the bar → fall through (lets you switch to the other menu / use controls)
            }
            if hit(vl.bar, x, y) {
                if hit(vl.play, x, y) {
                    return Some(UiAction::VideoPause);
                }
                if hit(vl.back, x, y) {
                    return Some(UiAction::VideoSeekRel(-10));
                }
                if hit(vl.fwd, x, y) {
                    return Some(UiAction::VideoSeekRel(10));
                }
                if hit(vl.audio, x, y) {
                    return Some(UiAction::ToggleTrackMenu(TrackMenu::Audio));
                }
                if hit(vl.subs, x, y) {
                    return Some(UiAction::ToggleTrackMenu(TrackMenu::Sub));
                }
                if hit(vl.full, x, y) {
                    return Some(UiAction::Fullscreen);
                }
                if hit([vl.seek[0], vl.bar[1], vl.seek[2], vl.bar[3]], x, y) {
                    let f = ((x - vl.seek[0]) / vl.seek[2]).clamp(0.0, 1.0);
                    return Some(UiAction::VideoSeekFrac(f));
                }
                if hit([vl.vol[0], vl.bar[1], vl.vol[2], vl.bar[3]], x, y) {
                    let f = ((x - vl.vol[0]) / vl.vol[2]).clamp(0.0, 1.0);
                    return Some(UiAction::VideoVolume(f * 100.0));
                }
                return Some(UiAction::CloseMenu); // consume bar clicks (no-op via CloseMenu)
            }
        }
    }

    // Top bar buttons — only when visible (hidden while an item is focused).
    if !c.focused {
        let b = bar(c.w);
        if hit(b.open, x, y) {
            return Some(UiAction::ToggleMenu(MenuKind::Open));
        }
        if hit(b.sort, x, y) {
            return Some(UiAction::ToggleMenu(MenuKind::Sort));
        }
        if hit(b.filter, x, y) {
            return Some(UiAction::ToggleMenu(MenuKind::Filter));
        }
        if hit(b.dates, x, y) {
            return Some(UiAction::ToggleMenu(MenuKind::Dates));
        }
        if hit(b.search, x, y) {
            // Click the little ✕ at the right of the box to clear, else focus it.
            let clear = [b.search[0] + b.search[2] - 26.0, b.search[1], 26.0, b.search[3]];
            if !c.search.is_empty() && hit(clear, x, y) {
                return Some(UiAction::ClearSearch);
            }
            return Some(UiAction::ActivateSearch);
        }
        if hit(b.settings, x, y) {
            return Some(UiAction::ToggleMenu(MenuKind::Settings));
        }
        if hit(b.slideshow, x, y) {
            return Some(UiAction::ToggleSlideshow);
        }
        if hit(b.full, x, y) {
            return Some(UiAction::Fullscreen);
        }
    }
    None
}

/// True if the pointer is anywhere over the UI (so the wall ignores hover under it).
pub fn pointer_over_ui(c: &UiCtx, x: f32, y: f32) -> bool {
    if y <= BAR_H {
        return true;
    }
    if c.menu.is_some() && hit_test(c, x, y).is_some() {
        return true;
    }
    if let Some(v) = &c.video {
        if v.visible && y >= video_layout(c.w, c.h).bar[1] {
            return true;
        }
    }
    false
}
