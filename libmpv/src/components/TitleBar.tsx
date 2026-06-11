import { useEffect, useState } from "react";

// Custom window chrome for the frameless embed window. The window must be frameless
// (it needs transparency to show the mpv video surface underneath), so the standard
// title bar — drag region + minimize / maximize / close — is drawn here in HTML.
// `-webkit-app-region: drag` makes the bar move the OS window; buttons opt out with
// `no-drag` so their clicks register.
const drag = { WebkitAppRegion: "drag" } as React.CSSProperties;
const noDrag = { WebkitAppRegion: "no-drag" } as React.CSSProperties;

export function TitleBar() {
  const [maximized, setMaximized] = useState(true);
  const e = typeof window !== "undefined" ? window.electron : undefined;

  useEffect(() => {
    e?.winIsMaximized().then((m) => setMaximized(!!m));
  }, [e]);

  const Btn = ({
    onClick,
    label,
    danger,
    children,
  }: {
    onClick: () => void;
    label: string;
    danger?: boolean;
    children: React.ReactNode;
  }) => (
    <button
      onClick={onClick}
      aria-label={label}
      title={label}
      style={noDrag}
      className={`flex h-full w-12 items-center justify-center text-white/70 transition hover:text-white ${
        danger ? "hover:bg-red-600" : "hover:bg-white/15"
      }`}
    >
      {children}
    </button>
  );

  return (
    <div
      style={drag}
      className="absolute inset-x-0 top-0 z-[60] flex h-8 select-none items-center justify-between bg-black/80 text-white backdrop-blur"
    >
      <div className="px-3 text-xs font-medium tracking-wide text-white/70">Cooliris Next</div>
      <div className="flex h-full">
        <Btn onClick={() => e?.winMinimize()} label="Minimize">
          <svg width="11" height="11" viewBox="0 0 12 12">
            <rect x="1" y="5.5" width="10" height="1" fill="currentColor" />
          </svg>
        </Btn>
        <Btn
          onClick={() => e?.winMaximize().then((m) => setMaximized(!!m))}
          label={maximized ? "Restore" : "Maximize"}
        >
          {maximized ? (
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor">
              <rect x="2.5" y="2.5" width="6" height="6" />
              <path d="M4 2.5V1h7v7H9.5" />
            </svg>
          ) : (
            <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor">
              <rect x="1.5" y="1.5" width="9" height="9" />
            </svg>
          )}
        </Btn>
        <Btn onClick={() => e?.winClose()} label="Close" danger>
          <svg width="11" height="11" viewBox="0 0 12 12" stroke="currentColor" strokeWidth="1.2">
            <path d="M2 2l8 8M10 2l-8 8" />
          </svg>
        </Btn>
      </div>
    </div>
  );
}
