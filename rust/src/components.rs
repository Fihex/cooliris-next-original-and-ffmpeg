// Custom (hand-rolled) UI components for the UI: the top bar (Open · Sort ▾ · Filter ▾ ·
// Search · Settings ▾ · counts · Info · Fullscreen), the dropdown menus, the search box, the
// info panel, and the video player controls bar.
//
// These are immediate-mode: each frame `State` fills a `UiCtx` snapshot, `build()` turns it
// into screen-space rects (`OverlayRect`, drawn by the overlay pipeline) + text (`ui::Line`), and
// `hit()` maps a click to a `UiAction` that `State` then applies. No widget owns any state.

use crate::ui::Line;

/// A screen-space coloured rectangle in NDC (x, y bottom-left, w, h) — the overlay pipeline's vertex.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct OverlayRect {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// Library sort order.
#[derive(Clone, Copy, PartialEq)]
pub enum SortMode {
    NameAsc,
    NameDesc,
    DateNew,
    DateOld,
}
impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::NameAsc => "Name \u{2191}",
            SortMode::NameDesc => "Name \u{2193}",
            SortMode::DateNew => "Date \u{2193}",
            SortMode::DateOld => "Date \u{2191}",
        }
    }
    pub const ALL: [SortMode; 4] = [
        SortMode::NameAsc,
        SortMode::NameDesc,
        SortMode::DateNew,
        SortMode::DateOld,
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
    Sort,
    Filter,
    Settings,
}

/// What a click on the UI means (applied by State).
#[derive(Clone, Copy, PartialEq)]
pub enum UiAction {
    Open,
    Fullscreen,
    ToggleInfo,
    ToggleMenu(MenuKind),
    CloseMenu,
    SetSort(SortMode),
    SetFilter(Filter),
    ToggleGifAnim,
    ToggleReflections,
    ActivateSearch,
    ClearSearch,
    VideoPause,
    VideoSeekRel(i32),
    VideoSeekFrac(f32),
    VideoVolume(f32),
    VideoAudio,
    VideoSub,
}

/// Snapshot of the playing item's state for the video bar.
pub struct VideoCtx {
    pub pos: f64,
    pub dur: f64,
    pub paused: bool,
    pub vol: f64,
    pub aid: i64,
    pub sid: i64,
    pub title: String,
    pub visible: bool, // controls shown (auto-hide)
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
    pub gif_anim: bool,
    pub reflections: bool,
    pub show_info: bool,
    pub total: usize,
    pub ready: usize,
    pub inflight: usize,
    pub info: Option<(String, String)>, // (name, dir) for the info panel
    pub video: Option<VideoCtx>,
    pub pointer: [f32; 2], // pixel cursor (for hover highlight)
}

const BAR_H: f32 = 48.0;
const BTN_Y: f32 = 8.0;
const BTN_H: f32 = 32.0;
const ROW_H: f32 = 30.0;

fn hit(r: [f32; 4], x: f32, y: f32) -> bool {
    x >= r[0] && x <= r[0] + r[2] && y >= r[1] && y <= r[1] + r[3]
}

/// Top-bar button rects (pixels).
struct Bar {
    open: [f32; 4],
    sort: [f32; 4],
    filter: [f32; 4],
    search: [f32; 4],
    full: [f32; 4],
    settings: [f32; 4],
    info: [f32; 4],
}
fn bar(w: f32) -> Bar {
    // Left group.
    let open = [12.0, BTN_Y, 64.0, BTN_H];
    let sort = [open[0] + open[2] + 8.0, BTN_Y, 120.0, BTN_H];
    let filter = [sort[0] + sort[2] + 8.0, BTN_Y, 118.0, BTN_H];
    let search = [filter[0] + filter[2] + 8.0, BTN_Y, 210.0, BTN_H];
    // Right group (right to left).
    let full = [w - 12.0 - 116.0, BTN_Y, 116.0, BTN_H];
    let settings = [full[0] - 8.0 - 104.0, BTN_Y, 104.0, BTN_H];
    let info = [settings[0] - 8.0 - 60.0, BTN_Y, 60.0, BTN_H];
    Bar {
        open,
        sort,
        filter,
        search,
        full,
        settings,
        info,
    }
}

/// Dropdown panel + row rects under an anchor button.
fn menu(anchor: [f32; 4], n: usize) -> ([f32; 4], Vec<[f32; 4]>) {
    let rw = anchor[2].max(118.0);
    let px = anchor[0];
    let py = anchor[1] + anchor[3] + 4.0;
    let panel = [px, py, rw, ROW_H * n as f32 + 8.0];
    let rows = (0..n)
        .map(|i| [px + 4.0, py + 4.0 + i as f32 * ROW_H, rw - 8.0, ROW_H])
        .collect();
    (panel, rows)
}

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
    let bh = 84.0;
    let by = h - bh;
    let seek = [20.0, by + 30.0, w - 40.0, 8.0]; // full-width track
    let row_y = by + 48.0;
    let back = [20.0, row_y, 44.0, 28.0];
    let play = [68.0, row_y, 44.0, 28.0];
    let fwd = [116.0, row_y, 44.0, 28.0];
    let vol = [180.0, row_y + 10.0, 120.0, 8.0];
    let full = [w - 20.0 - 44.0, row_y, 44.0, 28.0];
    let subs = [full[0] - 8.0 - 70.0, row_y, 70.0, 28.0];
    let audio = [subs[0] - 8.0 - 80.0, row_y, 80.0, 28.0];
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

/// Build this frame's UI: overlay rects + text lines.
pub fn build(c: &UiCtx) -> (Vec<OverlayRect>, Vec<Line>) {
    let (w, h) = (c.w.max(1.0), c.h.max(1.0));
    let mut rects = Vec::new();
    let mut lines = Vec::new();
    // pixel box (top-left origin) → overlay NDC rect (bottom-left + size).
    let nd = |r: [f32; 4]| {
        [
            r[0] / w * 2.0 - 1.0,
            1.0 - (r[1] + r[3]) / h * 2.0,
            r[2] / w * 2.0,
            r[3] / h * 2.0,
        ]
    };
    let chip = [1.0, 1.0, 1.0, 0.12];
    let chip_on = [1.0, 1.0, 1.0, 0.24];
    let mut rect = |r: [f32; 4], color: [f32; 4]| rects.push(OverlayRect { rect: nd(r), color });
    let mut label = |t: String, x: f32, y: f32, size: f32, col: [u8; 4]| {
        lines.push(Line {
            text: t,
            x,
            y,
            size,
            color: col,
        })
    };
    let white = [235, 235, 240, 255];

    let b = bar(w);
    // Glass top bar.
    rect([0.0, 0.0, w, BAR_H], [0.05, 0.05, 0.08, 0.66]);
    rect(b.open, chip);
    label("Open".into(), b.open[0] + 12.0, b.open[1] + 8.0, 16.0, white);
    rect(b.sort, if c.menu == Some(MenuKind::Sort) { chip_on } else { chip });
    label(format!("Sort: {} \u{25be}", c.sort.label()), b.sort[0] + 10.0, b.sort[1] + 8.0, 14.0, white);
    rect(b.filter, if c.menu == Some(MenuKind::Filter) { chip_on } else { chip });
    label(format!("Filter: {} \u{25be}", c.filter.label()), b.filter[0] + 10.0, b.filter[1] + 8.0, 14.0, white);
    // Search box.
    rect(b.search, if c.search_active { chip_on } else { [1.0, 1.0, 1.0, 0.08] });
    if c.search.is_empty() && !c.search_active {
        label("\u{1f50d} Search".into(), b.search[0] + 10.0, b.search[1] + 8.0, 14.0, [170, 170, 180, 220]);
    } else {
        let cursor = if c.search_active { "_" } else { "" };
        label(format!("{}{}", c.search, cursor), b.search[0] + 10.0, b.search[1] + 8.0, 14.0, white);
        if !c.search.is_empty() {
            label("\u{2715}".into(), b.search[0] + b.search[2] - 20.0, b.search[1] + 8.0, 14.0, [200, 200, 210, 230]);
        }
    }
    rect(b.settings, if c.menu == Some(MenuKind::Settings) { chip_on } else { chip });
    label("\u{2699} Settings \u{25be}".into(), b.settings[0] + 8.0, b.settings[1] + 8.0, 14.0, white);
    rect(b.info, if c.show_info { chip_on } else { chip });
    label("Info".into(), b.info[0] + 14.0, b.info[1] + 8.0, 14.0, white);
    rect(b.full, chip);
    label("\u{26f6} Fullscreen".into(), b.full[0] + 10.0, b.full[1] + 8.0, 14.0, white);
    // Counts (left of Info).
    let counts = if c.inflight > 0 {
        format!("{} · {} loaded · {} loading", c.total, c.ready, c.inflight)
    } else {
        format!("{} items", c.total)
    };
    label(counts, b.info[0] - 220.0, b.info[1] + 9.0, 13.0, [205, 205, 215, 230]);

    // Open dropdown.
    if let Some(kind) = c.menu {
        let (anchor, items): ([f32; 4], Vec<(String, bool)>) = match kind {
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
            MenuKind::Settings => (
                b.settings,
                vec![
                    (format!("{} Animate GIFs", check(c.gif_anim)), false),
                    (format!("{} Reflections", check(c.reflections)), false),
                ],
            ),
        };
        let (panel, rows) = menu(anchor, items.len());
        rect(panel, [0.08, 0.08, 0.11, 0.97]);
        for (r, (text, current)) in rows.iter().zip(items) {
            let hov = hit(*r, c.pointer[0], c.pointer[1]);
            if current {
                rect(*r, [1.0, 1.0, 1.0, 0.22]);
            } else if hov {
                rect(*r, [1.0, 1.0, 1.0, 0.12]);
            }
            label(text, r[0] + 8.0, r[1] + 7.0, 14.0, white);
        }
    }

    // Info panel.
    if c.show_info {
        if let Some((name, dir)) = &c.info {
            rect([10.0, BAR_H + 8.0, 360.0, 46.0], [0.0, 0.0, 0.0, 0.66]);
            label(name.clone(), 18.0, BAR_H + 12.0, 15.0, white);
            label(dir.clone(), 18.0, BAR_H + 32.0, 12.0, [180, 180, 190, 220]);
        }
    }

    // Video controls bar.
    if let Some(v) = &c.video {
        if v.visible {
            let vl = video_layout(w, h);
            rect(vl.bar, [0.03, 0.03, 0.05, 0.82]);
            label(v.title.clone(), 20.0, vl.bar[1] + 8.0, 14.0, white);
            // seek track + fill + knob
            let frac = if v.dur > 0.0 { (v.pos / v.dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
            rect(vl.seek, [1.0, 1.0, 1.0, 0.22]);
            rect([vl.seek[0], vl.seek[1], vl.seek[2] * frac, vl.seek[3]], [0.35, 0.7, 1.0, 0.95]);
            rect([vl.seek[0] + vl.seek[2] * frac - 5.0, vl.seek[1] - 5.0, 10.0, vl.seek[3] + 10.0], [1.0, 1.0, 1.0, 0.95]);
            label(fmt_time(v.pos), vl.seek[0], vl.seek[1] - 18.0, 12.0, [210, 210, 220, 220]);
            label(fmt_time(v.dur), vl.seek[0] + vl.seek[2] - 36.0, vl.seek[1] - 18.0, 12.0, [210, 210, 220, 220]);
            // transport buttons
            for (r, t) in [
                (vl.back, "\u{23ea}".to_string()),
                (vl.play, if v.paused { "\u{25b6}".into() } else { "\u{23f8}".into() }),
                (vl.fwd, "\u{23e9}".to_string()),
            ] {
                rect(r, chip);
                label(t, r[0] + 12.0, r[1] + 5.0, 16.0, white);
            }
            // volume
            label("\u{1f50a}".into(), vl.vol[0] - 22.0, vl.vol[1] - 6.0, 14.0, white);
            let vf = (v.vol / 100.0).clamp(0.0, 1.0) as f32;
            rect(vl.vol, [1.0, 1.0, 1.0, 0.22]);
            rect([vl.vol[0], vl.vol[1], vl.vol[2] * vf, vl.vol[3]], [0.8, 0.8, 0.9, 0.9]);
            // audio / subs / fullscreen
            let trk = |id: i64| if id > 0 { id.to_string() } else { "off".to_string() };
            rect(vl.audio, chip);
            label(format!("Audio {}", trk(v.aid)), vl.audio[0] + 8.0, vl.audio[1] + 5.0, 13.0, white);
            rect(vl.subs, chip);
            label(format!("Subs {}", trk(v.sid)), vl.subs[0] + 8.0, vl.subs[1] + 5.0, 13.0, white);
            rect(vl.full, chip);
            label("\u{26f6}".into(), vl.full[0] + 14.0, vl.full[1] + 5.0, 16.0, white);
        }
    }

    (rects, lines)
}

fn check(on: bool) -> &'static str {
    if on {
        "\u{2611}"
    } else {
        "\u{2610}"
    }
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
        let anchor = match kind {
            MenuKind::Sort => bar(c.w).sort,
            MenuKind::Filter => bar(c.w).filter,
            MenuKind::Settings => bar(c.w).settings,
        };
        let n = match kind {
            MenuKind::Sort => 4,
            MenuKind::Filter => 4,
            MenuKind::Settings => 2,
        };
        let (_, rows) = menu(anchor, n);
        for (i, r) in rows.iter().enumerate() {
            if hit(*r, x, y) {
                return Some(match kind {
                    MenuKind::Sort => UiAction::SetSort(SortMode::ALL[i]),
                    MenuKind::Filter => UiAction::SetFilter(Filter::ALL[i]),
                    MenuKind::Settings => {
                        if i == 0 {
                            UiAction::ToggleGifAnim
                        } else {
                            UiAction::ToggleReflections
                        }
                    }
                });
            }
        }
        if !hit(anchor, x, y) {
            return Some(UiAction::CloseMenu);
        }
    }

    // Video controls.
    if let Some(v) = &c.video {
        if v.visible {
            let vl = video_layout(c.w, c.h);
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
                    return Some(UiAction::VideoAudio);
                }
                if hit(vl.subs, x, y) {
                    return Some(UiAction::VideoSub);
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

    // Top bar buttons.
    let b = bar(c.w);
    if hit(b.open, x, y) {
        return Some(UiAction::Open);
    }
    if hit(b.sort, x, y) {
        return Some(UiAction::ToggleMenu(MenuKind::Sort));
    }
    if hit(b.filter, x, y) {
        return Some(UiAction::ToggleMenu(MenuKind::Filter));
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
    if hit(b.info, x, y) {
        return Some(UiAction::ToggleInfo);
    }
    if hit(b.full, x, y) {
        return Some(UiAction::Fullscreen);
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
