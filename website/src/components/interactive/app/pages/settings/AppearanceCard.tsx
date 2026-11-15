import {
  ACCENTS,
  dispatch,
  useApp,
  type AccentId,
  type SettingsState,
} from "../../store";
import { Icon, type IconName } from "../../ui/Icon";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Card } from "./parts";

const THEMES: { id: SettingsState["theme"]; label: string; icon: IconName }[] =
  [
    { id: "dark", label: "Dark", icon: "moon" },
    { id: "light", label: "Light", icon: "sun" },
    { id: "system", label: "System", icon: "monitor" },
  ];

/** Miniature window that follows the chosen theme and accent. */
function LivePreview() {
  return (
    <div
      aria-hidden="true"
      data-live-preview
      className="flex min-w-0 flex-col gap-2 rounded-xl border border-accent bg-background p-3"
    >
      <span className="app-text-sm text-text">Live Preview</span>
      <div className="flex gap-2 rounded-lg border border-border bg-surface p-2">
        <div className="flex w-1/5 flex-col gap-1.5 pt-1">
          <span className="h-1.5 w-3/4 rounded-full bg-surface-hover" />
          <span className="h-1.5 w-1/2 rounded-full bg-surface-hover" />
          <span className="h-1.5 w-full rounded-full bg-accent" />
          <span className="h-1.5 w-2/3 rounded-full bg-surface-hover" />
        </div>
        <div className="flex min-w-0 flex-1 flex-col gap-1.5">
          <div className="relative grid aspect-video place-items-center overflow-hidden rounded-md bg-surface-raised">
            <span className="absolute inset-x-0 bottom-0 h-1/2 bg-accent-subtle" />
            <Icon name="play" className="relative size-[1.6em] text-text" />
          </div>
          <span className="flex gap-1">
            <span className="h-1.5 flex-1 rounded-full bg-surface-hover" />
            <span className="h-1.5 flex-1 rounded-full bg-surface-hover" />
          </span>
          <span className="h-1.5 w-full rounded-full bg-accent" />
        </div>
      </div>
    </div>
  );
}

export function AppearanceCard() {
  const { theme, accent } = useApp((s) => s.settings);
  const set = (patch: Partial<SettingsState>) =>
    dispatch({ type: "settings/set", patch });

  return (
    <Card
      id="appearance"
      icon="palette"
      title="Appearance"
      description="Customize the look and feel of clipuh."
    >
      <div className="flex flex-col gap-4 @min-[60rem]/app-window:flex-row">
        <div className="flex min-w-0 flex-1 flex-col gap-3">
          <div data-element="settings.theme" className="flex flex-col gap-2">
            <span
              id="settings-theme-label"
              className="app-text-base flex items-center gap-1.5 text-text"
            >
              Theme
              <PlannedBadge id="settings.theme" />
            </span>
            <div
              role="radiogroup"
              aria-labelledby="settings-theme-label"
              className="flex flex-wrap gap-2"
            >
              {THEMES.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  role="radio"
                  aria-checked={theme === t.id}
                  onClick={() => set({ theme: t.id })}
                  className={`app-text-base inline-flex min-h-9 cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5 text-text ${theme === t.id ? "border-accent bg-accent-subtle" : "border-border bg-surface-raised hover:border-border-hover"}`}
                >
                  <Icon name={t.icon} className="size-[1.1em]" />
                  {t.label}
                </button>
              ))}
            </div>
          </div>
          <div data-element="settings.accent" className="flex flex-col gap-2">
            <span
              id="settings-accent-label"
              className="app-text-base flex items-center gap-1.5 text-text"
            >
              Accent Color
              <PlannedBadge id="settings.accent" />
            </span>
            <div
              role="radiogroup"
              aria-labelledby="settings-accent-label"
              className="flex flex-wrap gap-2"
            >
              {(Object.keys(ACCENTS) as AccentId[]).map((id) => (
                <button
                  key={id}
                  type="button"
                  role="radio"
                  aria-checked={accent === id}
                  aria-label={ACCENTS[id].label}
                  title={ACCENTS[id].label}
                  onClick={() => set({ accent: id })}
                  style={{ background: `var(${ACCENTS[id].tokenVar})` }}
                  className={`grid size-[2.4em] cursor-pointer place-items-center rounded-lg border-2 text-accent-foreground ${accent === id ? "border-text" : "border-transparent"}`}
                >
                  {accent === id && (
                    <Icon name="check" className="size-[55%]" />
                  )}
                </button>
              ))}
            </div>
          </div>
        </div>
        <div className="@min-[60rem]/app-window:w-[42%]">
          <LivePreview />
        </div>
      </div>
    </Card>
  );
}
