// Experimental embedded-mpv mode (Option 1). When the app is launched with
// COOLIRIS_MPV_EMBED=1, the main process renders hardware-decoded mpv video INTO the
// window beneath the web layer, and signals the renderer via the ?embed=1 URL param.
// In this mode the web layer must be transparent wherever the video shows (the window
// itself is transparent, the page background is cleared, and the wall canvas is hidden
// while a video is open) so the mpv surface underneath is visible, with the controls
// floating on top. Off → normal canvas frame-pump; nothing here applies.
//
// (Named embedMode to avoid the existing src/embed/ wall-engine directory.)
const params = typeof location !== "undefined" ? new URLSearchParams(location.search) : new URLSearchParams();

// Transparent player styling (mpv renders under the web; controls overlay). True in the
// child video window.
export const EMBED = params.has("embed");
// The MAIN (wall) window in two-window mode: route video opens to the child window.
export const TWO_WIN = params.has("twowin");
// This renderer instance IS the child video window (renders only the player).
export const VIDEO_CHILD = params.has("videochild");
