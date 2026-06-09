interface SettingsDialogProps {
  titles: boolean;
  gifAnim: boolean;
  onClose: () => void;
  onToggleTitles: () => void;
  onToggleGifAnim: () => void;
}

/** Settings modal: display toggles (both default off). */
export function SettingsDialog(props: SettingsDialogProps) {
  return (
    <div
      className="absolute inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
      onClick={props.onClose}
    >
      <div
        className="w-full max-w-sm rounded-2xl bg-neutral-900 p-5 text-white shadow-2xl ring-1 ring-white/10"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold">Settings</h2>
          <button
            onClick={props.onClose}
            className="rounded-full px-2 text-2xl leading-none text-white/60 hover:text-white"
            aria-label="Close"
          >
            ×
          </button>
        </div>

        <Toggle
          label="Show titles"
          hint="Show every photo's filename on the wall."
          on={props.titles}
          onChange={props.onToggleTitles}
        />
        <Toggle
          label="Animate GIFs on the wall"
          hint="Play GIF tiles while browsing (heavier). Off = static first frame; GIFs still animate when opened."
          on={props.gifAnim}
          onChange={props.onToggleGifAnim}
        />
      </div>
    </div>
  );
}

function Toggle({
  label,
  hint,
  on,
  onChange,
}: {
  label: string;
  hint: string;
  on: boolean;
  onChange: () => void;
}) {
  return (
    <button
      onClick={onChange}
      className="flex w-full items-start gap-3 rounded-xl px-2 py-3 text-left hover:bg-white/5"
      role="switch"
      aria-checked={on}
    >
      <span
        className={`mt-0.5 flex h-6 w-10 shrink-0 items-center rounded-full p-0.5 transition ${
          on ? "bg-emerald-500" : "bg-white/20"
        }`}
      >
        <span
          className={`h-5 w-5 rounded-full bg-white transition ${on ? "translate-x-4" : ""}`}
        />
      </span>
      <span className="min-w-0">
        <span className="block text-sm font-medium">{label}</span>
        <span className="block text-xs text-white/45">{hint}</span>
      </span>
    </button>
  );
}
