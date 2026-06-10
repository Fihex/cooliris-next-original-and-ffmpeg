import { Component, type ReactNode } from "react";

interface State {
  error: Error | null;
}

/** Catches render errors so the user sees a message instead of a blank page. */
export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error) {
    // Surface to the console for debugging.
    console.error("Cooliris Next crashed:", error);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-full w-full items-center justify-center p-6">
          <div className="max-w-lg rounded-xl bg-red-500/15 p-5 text-sm text-red-100 ring-1 ring-red-400/30">
            <p className="mb-2 font-semibold">Something went wrong.</p>
            <pre className="whitespace-pre-wrap break-words text-xs text-red-200/80">
              {this.state.error.message}
            </pre>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
