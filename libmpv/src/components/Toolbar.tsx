import { useRef, useState } from "react";

export interface LoadProgress {
  loaded: number;
  total: number;
  pending: number;
}

export type SortKey =
  | "default"
  | "name-asc"
  | "name-desc"
  | "modified-desc"
  | "modified-asc"
  | "created-desc"
  | "created-asc";

const SORT_OPTIONS: { key: SortKey; label: string }[] = [
  { key: "default", label: "Default (as loaded)" },
  { key: "name-asc", label: "Name (A → Z)" },
  { key: "name-desc", label: "Name (Z → A)" },
  { key: "modified-desc", label: "Modified (newest)" },
  { key: "modified-asc", label: "Modified (oldest)" },
  { key: "created-desc", label: "Created (newest)" },
  { key: "created-asc", label: "Created (oldest)" },
];

interface ToolbarProps {
  title?: string;
  count: number;
  busy: boolean;
  progress: LoadProgress;
  slideshow: boolean;
  search: string;
  fromDate: string;
  toDate: string;
  dateField: "modified" | "created";
  hasCreated: boolean;
  sort: SortKey;
  onSort: (k: SortKey) => void;
  onOpen: () => void;
  onSearch: (q: string) => void;
  onFromDate: (d: string) => void;
  onToDate: (d: string) => void;
  onDateField: (f: "modified" | "created") => void;
  onToggleSlideshow: () => void;
  onOpenSettings: () => void;
  onFullscreen: () => void;
}

export function Toolbar(props: ToolbarProps) {
  const stillLoading = props.busy || props.progress.pending > 0;

  return (
    <header className="pointer-events-auto absolute left-0 right-0 top-0 z-20 flex flex-wrap items-center gap-1.5 bg-gradient-to-b from-black/70 to-transparent px-2.5 py-2 sm:gap-2 sm:px-4 sm:py-3">
      <div className="mr-1 flex items-baseline gap-1.5 sm:mr-2">
        <span className="text-base font-semibold tracking-tight sm:text-lg">Cooliris</span>
        <span className="hidden text-base font-light text-white/50 sm:inline sm:text-lg">Next</span>
      </div>

      <Btn onClick={props.onOpen}>Open</Btn>

      <div className="mx-1 h-5 w-px bg-white/15" />

      <Btn onClick={props.onToggleSlideshow} active={props.slideshow}>
        {props.slideshow ? "Stop" : "Slideshow"}
      </Btn>
      <Btn onClick={props.onFullscreen}>Fullscreen</Btn>

      <div className="ml-auto flex items-center gap-2">
        <Btn onClick={props.onOpenSettings}>Settings</Btn>
        <SortMenu sort={props.sort} onSort={props.onSort} />
        <DatesMenu
          from={props.fromDate}
          to={props.toDate}
          dateField={props.dateField}
          hasCreated={props.hasCreated}
          onFrom={props.onFromDate}
          onTo={props.onToDate}
          onDateField={props.onDateField}
        />

        {/* Search */}
        <div className="relative">
          <input
            value={props.search}
            onChange={(e) => props.onSearch(e.target.value)}
            placeholder="Search…"
            className="w-28 rounded-full bg-white/10 px-3 py-1.5 text-sm text-white placeholder-white/40 outline-none ring-white/20 focus:ring-2 sm:w-52"
          />
          {props.search && (
            <button
              onClick={() => props.onSearch("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-white/50 hover:text-white"
              aria-label="Clear search"
            >
              ×
            </button>
          )}
        </div>

        <div className="flex items-center gap-2 text-sm text-white/60">
          {stillLoading && <Spinner />}
          {props.progress.total > 0 && (
            <span>
              {props.progress.loaded}/{props.progress.total}
            </span>
          )}
        </div>
      </div>
    </header>
  );
}

/** A date input you can BOTH type (YYYY-MM-DD) and pick from a calendar. */
function DateField({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const picker = useRef<HTMLInputElement>(null);
  const ymd = /^\d{4}-\d{2}-\d{2}$/.test(value) ? value : "";
  return (
    <div className="relative">
      <input
        type="text"
        inputMode="numeric"
        value={value}
        placeholder="YYYY-MM-DD"
        onChange={(e) => onChange(e.target.value)}
        className="w-full rounded-lg bg-white/10 px-2 py-1.5 pr-9 text-white placeholder-white/30 outline-none ring-white/20 focus:ring-2"
      />
      <button
        type="button"
        aria-label="Pick a date"
        onClick={() => {
          const el = picker.current;
          if (!el) return;
          if (el.showPicker) el.showPicker();
          else el.focus();
        }}
        className="absolute right-1 top-1/2 -translate-y-1/2 rounded px-1 py-0.5 text-white/60 hover:text-white"
      >
        📅
      </button>
      {/* Hidden native date input — only used to open the calendar picker. */}
      <input
        ref={picker}
        type="date"
        value={ymd}
        tabIndex={-1}
        aria-hidden
        onChange={(e) => onChange(e.target.value)}
        className="pointer-events-none absolute right-2 top-1/2 h-px w-px -translate-y-1/2 opacity-0 [color-scheme:dark]"
      />
    </div>
  );
}

/** Sort-order dropdown (name / modified / created, asc & desc). */
function SortMenu({ sort, onSort }: { sort: SortKey; onSort: (k: SortKey) => void }) {
  const [open, setOpen] = useState(false);
  const active = SORT_OPTIONS.find((o) => o.key === sort);
  return (
    <div className="relative">
      <Btn onClick={() => setOpen((o) => !o)} active={open}>
        Sort
      </Btn>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div className="absolute right-0 top-full z-40 mt-1 w-56 overflow-hidden rounded-xl bg-neutral-900 py-1 text-sm shadow-2xl ring-1 ring-white/10">
            {SORT_OPTIONS.map((o) => (
              <button
                key={o.key}
                onClick={() => {
                  onSort(o.key);
                  setOpen(false);
                }}
                className={`block w-full whitespace-nowrap px-3 py-1.5 text-left transition ${
                  o.key === active?.key
                    ? "bg-white/15 text-white"
                    : "text-white/70 hover:bg-white/10"
                }`}
              >
                {o.label}
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

/** Date-range filter popover (filters by file modified or — on desktop — created date). */
function DatesMenu({
  from,
  to,
  dateField,
  hasCreated,
  onFrom,
  onTo,
  onDateField,
}: {
  from: string;
  to: string;
  dateField: "modified" | "created";
  hasCreated: boolean;
  onFrom: (d: string) => void;
  onTo: (d: string) => void;
  onDateField: (f: "modified" | "created") => void;
}) {
  const [open, setOpen] = useState(false);
  const active = !!(from || to);
  return (
    <div className="relative">
      <Btn onClick={() => setOpen((o) => !o)} active={active || open}>
        Dates{active ? " •" : ""}
      </Btn>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div className="absolute right-0 top-full z-40 mt-1 w-56 rounded-xl bg-neutral-900 p-3 text-sm shadow-2xl ring-1 ring-white/10">
            <div className="mb-3">
              <label className="mb-1 block text-xs text-white/50">Filter by</label>
              <div className="flex overflow-hidden rounded-lg ring-1 ring-white/15">
                {(["modified", "created"] as const).map((f) => (
                  <button
                    key={f}
                    onClick={() => onDateField(f)}
                    className={`flex-1 px-2 py-1 text-xs font-medium capitalize transition ${
                      dateField === f ? "bg-white text-black" : "text-white/70 hover:bg-white/10"
                    }`}
                  >
                    {f}
                  </button>
                ))}
              </div>
            </div>
            <label className="mb-1 block text-xs text-white/50">From</label>
            <div className="mb-3">
              <DateField value={from} onChange={onFrom} />
            </div>
            <label className="mb-1 block text-xs text-white/50">To</label>
            <DateField value={to} onChange={onTo} />
            <div className="mt-3 flex items-center justify-between gap-2">
              <button
                onClick={() => {
                  onFrom("");
                  onTo("");
                }}
                disabled={!active}
                className="rounded-lg bg-white/10 px-3 py-1 text-xs font-medium text-white hover:bg-white/20 disabled:cursor-not-allowed disabled:opacity-40"
              >
                Clear dates
              </button>
              <button
                onClick={() => setOpen(false)}
                className="rounded-lg bg-white px-3 py-1 text-xs font-medium text-black hover:bg-white/90"
              >
                Done
              </button>
            </div>
            <p className="mt-2 text-[10px] leading-tight text-white/35">
              {`Filtering by file ${dateField} date.`}
              {dateField === "created" && !hasCreated
                ? " No created date for these — falls back to modified."
                : ""}
            </p>
          </div>
        </>
      )}
    </div>
  );
}

function Btn({
  children,
  onClick,
  active,
}: {
  children: React.ReactNode;
  onClick: () => void;
  active?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={`whitespace-nowrap rounded-full px-2.5 py-1 text-xs font-medium transition sm:px-3 sm:py-1.5 sm:text-sm ${
        active ? "bg-white text-black" : "bg-white/10 text-white hover:bg-white/20"
      }`}
    >
      {children}
    </button>
  );
}

function Spinner() {
  return (
    <span className="inline-block h-4 w-4 animate-spin rounded-full border-2 border-white/30 border-t-white" />
  );
}
