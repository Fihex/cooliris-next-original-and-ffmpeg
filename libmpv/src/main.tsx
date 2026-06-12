import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { RouterProvider, createRouter } from "@tanstack/react-router";
import { routeTree } from "./routeTree.gen";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { VIDEO_CHILD } from "./embedMode";
import { VideoChildView } from "./components/VideoChildView";
import "./index.css";

const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

// The two-window embed CHILD window renders only the video player (no wall/router).
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>{VIDEO_CHILD ? <VideoChildView /> : <RouterProvider router={router} />}</ErrorBoundary>
  </StrictMode>
);
