import { useRef, useState } from "react";

interface OpenDialogProps {
  fsAccess: boolean;
  onClose: () => void;
  onFiles: (files: File[]) => void; // fallback <input>
  onFilePicker: () => void; // File System Access API
  onFolderPicker: () => void;
  onDrop: (dt: DataTransfer) => void;
  onJson: () => void;
}

/** Modal shown by the "Open" button: drag-drop zone + file/folder/JSON buttons. */
export function OpenDialog(props: OpenDialogProps) {
  const fileInput = useRef<HTMLInputElement>(null);
  const folderInput = useRef<HTMLInputElement>(null);
  const [over, setOver] = useState(false);

  const chooseFiles = () =>
    props.fsAccess ? props.onFilePicker() : fileInput.current?.click();
  const chooseFolder = () =>
    props.fsAccess ? props.onFolderPicker() : folderInput.current?.click();

  return (
    <div
      className="absolute inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
      onClick={props.onClose}
    >
      <div
        className="w-full max-w-lg rounded-2xl bg-neutral-900 p-5 text-white shadow-2xl ring-1 ring-white/10"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold">Open media</h2>
          <button
            onClick={props.onClose}
            className="rounded-full px-2 text-2xl leading-none text-white/60 hover:text-white"
            aria-label="Close"
          >
            ×
          </button>
        </div>

        {/* Drop zone — also click it to choose a folder. */}
        <div
          role="button"
          tabIndex={0}
          onClick={chooseFolder}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") chooseFolder();
          }}
          onDragOver={(e) => {
            e.preventDefault();
            if (!over) setOver(true);
          }}
          onDragLeave={(e) => {
            if (e.currentTarget === e.target) setOver(false);
          }}
          onDrop={(e) => {
            e.preventDefault();
            setOver(false);
            if (e.dataTransfer) props.onDrop(e.dataTransfer);
          }}
          className={`mb-4 flex h-36 cursor-pointer flex-col items-center justify-center rounded-xl border-2 border-dashed text-center outline-none transition ${
            over ? "border-white/70 bg-white/10" : "border-white/20 bg-white/5 hover:bg-white/10"
          }`}
        >
          <div className="text-sm font-medium">Drag &amp; drop files or a folder here</div>
          <div className="mt-1 text-xs text-white/40">or click to choose a folder · images, videos, audio</div>
        </div>

        <div className="flex gap-2">
          <Btn onClick={chooseFiles}>Choose files…</Btn>
          <Btn onClick={chooseFolder}>Choose folder…</Btn>
          <div className="flex-1" />
          <Btn onClick={props.onJson}>From JSON…</Btn>
        </div>

        <input
          ref={fileInput}
          type="file"
          multiple
          accept="image/*,video/*,audio/*"
          className="hidden"
          onChange={(e) => {
            if (e.target.files) props.onFiles(Array.from(e.target.files));
            e.target.value = "";
          }}
        />
        <input
          ref={folderInput}
          type="file"
          // @ts-expect-error non-standard but widely supported
          webkitdirectory=""
          directory=""
          multiple
          className="hidden"
          onChange={(e) => {
            if (e.target.files) props.onFiles(Array.from(e.target.files));
            e.target.value = "";
          }}
        />
      </div>
    </div>
  );
}

function Btn({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="rounded-lg bg-white/15 px-3 py-2 text-sm font-medium hover:bg-white/25"
    >
      {children}
    </button>
  );
}
