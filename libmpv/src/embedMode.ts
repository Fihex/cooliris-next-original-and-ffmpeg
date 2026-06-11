// Experimental embedded-mpv mode (Option 1). When the app is launched with
// COOLIRIS_MPV_EMBED=1, the main process renders hardware-decoded mpv video INTO the
// window beneath the web layer, and signals the renderer via the ?embed=1 URL param.
// In this mode the web layer must be transparent wherever the video shows (the window
// itself is transparent, the page background is cleared, and the wall canvas is hidden
// while a video is open) so the mpv surface underneath is visible, with the controls
// floating on top. Off → normal canvas frame-pump; nothing here applies.
//
// (Named embedMode to avoid the existing src/embed/ wall-engine directory.)
export const EMBED =
  typeof document !== "undefined" && new URLSearchParams(location.search).has("embed");
