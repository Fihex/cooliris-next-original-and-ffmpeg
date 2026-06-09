import { useRef, useState } from "react";

interface JsonDialogProps {
  onClose: () => void;
  onUrl: (url: string) => void;
  onFile: (file: File) => void;
  onText: (text: string) => void;
}

/** Modal to load a JSON manifest by URL, local file, or pasted text. */
export function JsonDialog({ onClose, onUrl, onFile, onText }: JsonDialogProps) {
  const [url, setUrl] = useState("");
  const [text, setText] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);

  return (
    <div
      className="absolute inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
      onClick={onClose}
    >
      <div
        className="w-full max-w-lg rounded-2xl bg-neutral-900 p-5 text-white shadow-2xl ring-1 ring-white/10"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold">Load JSON feed</h2>
          <button
            onClick={onClose}
            className="rounded-full px-2 text-2xl leading-none text-white/60 hover:text-white"
            aria-label="Close"
          >
            ×
          </button>
        </div>

        {/* From URL */}
        <label className="mb-1 block text-xs font-medium text-white/50">From URL</label>
        <form
          className="mb-4 flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (url.trim()) onUrl(url.trim());
          }}
        >
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://example.com/feed.json"
            className="flex-1 rounded-lg bg-white/10 px-3 py-2 text-sm outline-none ring-white/20 focus:ring-2"
          />
          <Btn type="submit" disabled={!url.trim()}>
            Load
          </Btn>
        </form>

        {/* From file */}
        <label className="mb-1 block text-xs font-medium text-white/50">From file</label>
        <div className="mb-4">
          <input
            ref={fileRef}
            type="file"
            accept=".json,application/json"
            className="hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) onFile(f);
            }}
          />
          <Btn onClick={() => fileRef.current?.click()}>Choose .json file…</Btn>
        </div>

        {/* From pasted text */}
        <label className="mb-1 block text-xs font-medium text-white/50">Paste JSON</label>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder='{ "items": [ { "full": "https://…/photo.jpg", "title": "…" } ] }'
          rows={5}
          className="mb-3 w-full resize-y rounded-lg bg-white/10 px-3 py-2 font-mono text-xs outline-none ring-white/20 focus:ring-2"
        />
        <div className="flex justify-end">
          <Btn onClick={() => text.trim() && onText(text)} disabled={!text.trim()}>
            Load pasted JSON
          </Btn>
        </div>

        <p className="mt-4 text-xs text-white/40">
          Accepts an array or <code className="text-white/60">{`{ items: [...] }`}</code>. Each item
          needs at least <code className="text-white/60">full</code> (or{" "}
          <code className="text-white/60">thumb</code>). Cross-site URLs are retried via a CORS proxy;
          remote images must allow CORS to render.
        </p>
      </div>
    </div>
  );
}

function Btn({
  children,
  onClick,
  type = "button",
  disabled,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  type?: "button" | "submit";
  disabled?: boolean;
}) {
  return (
    <button
      type={type}
      onClick={onClick}
      disabled={disabled}
      className="rounded-lg bg-white/15 px-3 py-2 text-sm font-medium hover:bg-white/25 disabled:cursor-not-allowed disabled:opacity-40"
    >
      {children}
    </button>
  );
}
