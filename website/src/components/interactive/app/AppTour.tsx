import { AppShell } from "./AppShell";
import type { PageId } from "./store";

/**
 * Island root for one tour chapter. Every chapter mounts its own AppTour
 * (client:visible); they share the module-level store in store.ts.
 */
export default function AppTour({
  initialPage,
  windowLabel,
}: {
  initialPage: PageId;
  windowLabel?: string;
}) {
  return <AppShell initialPage={initialPage} windowLabel={windowLabel} />;
}
