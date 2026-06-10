export interface ToastMessage {
  id: number;
  kind: "info" | "error";
  text: string;
}

export function Toasts({ toasts }: { toasts: ToastMessage[] }) {
  if (!toasts.length) return null;
  return (
    <div className="pointer-events-none absolute bottom-16 left-1/2 z-30 flex -translate-x-1/2 flex-col items-center gap-2">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`rounded-full px-4 py-2 text-sm shadow-lg backdrop-blur ${
            t.kind === "error"
              ? "bg-red-500/90 text-white"
              : "bg-white/10 text-white"
          }`}
        >
          {t.text}
        </div>
      ))}
    </div>
  );
}
