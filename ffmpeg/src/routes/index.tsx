import { createFileRoute } from "@tanstack/react-router";
import { WallView } from "@/components/WallView";

export const Route = createFileRoute("/")({
  component: WallView,
});
