import { beforeEach, describe, expect, it } from "vitest";
import { appStore, dispatch, estimateClipMb, visibleClips } from "./store";

beforeEach(() => appStore.reset());

describe("app store", () => {
  it("toggleFavorite flips only that clip", () => {
    const before = appStore.getState().clips;
    const id = before[2].id;
    dispatch({ type: "clip/toggleFavorite", id });
    const after = appStore.getState().clips;
    for (const c of after) {
      const prev = before.find((b) => b.id === c.id)!;
      expect(c.favorite).toBe(c.id === id ? !prev.favorite : prev.favorite);
    }
  });

  it("estimateClipMb(60, 20) === 150", () => {
    expect(estimateClipMb(60, 20)).toBe(150);
  });

  it("delete of the open editor clip moves editorClipId to newest remaining; deleting all sets null", () => {
    const sorted = visibleClips(appStore.getState());
    expect(appStore.getState().editorClipId).toBe(sorted[0].id);
    dispatch({ type: "clips/delete", ids: [sorted[0].id] });
    expect(appStore.getState().editorClipId).toBe(sorted[1].id);
    dispatch({
      type: "clips/delete",
      ids: appStore.getState().clips.map((c) => c.id),
    });
    expect(appStore.getState().editorClipId).toBeNull();
  });

  it("hotkey/set rejects a combo used by another action: hotkeys unchanged, lastError set", () => {
    const before = appStore.getState().settings.hotkeys;
    dispatch({ type: "hotkey/set", action: "openApp", combo: before.saveClip });
    const s = appStore.getState();
    expect(s.settings.hotkeys).toEqual(before);
    expect(s.lastError).toBeTruthy();
    dispatch({ type: "error/clear" });
    expect(appStore.getState().lastError).toBeNull();
    dispatch({ type: "hotkey/set", action: "openApp", combo: "Alt + O" });
    expect(appStore.getState().settings.hotkeys.openApp).toBe("Alt + O");
  });

  it("solo on one source leaves others' solo untouched; mute toggles independently", () => {
    const [a, b] = appStore.getState().sources;
    dispatch({ type: "source/set", id: a.id, patch: { solo: true } });
    dispatch({ type: "source/set", id: b.id, patch: { muted: true } });
    const s = appStore.getState().sources;
    expect(s[0].solo).toBe(true);
    expect(s[0].muted).toBe(false);
    expect(s[1].solo).toBe(false);
    expect(s[1].muted).toBe(true);
    expect(s[2].solo).toBe(false);
  });

  it("subscribe listener fires once per dispatch and stops after unsubscribe", () => {
    let n = 0;
    const off = appStore.subscribe(() => n++);
    dispatch({ type: "capture/set", patch: { fps: 30 } });
    expect(n).toBe(1);
    off();
    dispatch({ type: "capture/set", patch: { fps: 60 } });
    expect(n).toBe(1);
  });

  it('source/add appends an "extra" source with a unique id', () => {
    dispatch({ type: "source/add" });
    dispatch({ type: "source/add" });
    const s = appStore.getState().sources;
    expect(s).toHaveLength(5);
    expect(s[3].kind).toBe("extra");
    expect(s[4].kind).toBe("extra");
    expect(new Set(s.map((x) => x.id)).size).toBe(5);
  });

  it("clips/restore re-inserts deleted clips, ordered by minutesAgo", () => {
    const all = appStore.getState().clips;
    const removed = [all[0], all[5]];
    dispatch({ type: "clips/delete", ids: removed.map((c) => c.id) });
    expect(appStore.getState().clips).toHaveLength(all.length - 2);
    dispatch({ type: "clips/restore", clips: removed });
    const after = appStore.getState().clips;
    expect(after).toHaveLength(all.length);
    const mins = after.map((c) => c.minutesAgo);
    expect(mins).toEqual([...mins].sort((x, y) => x - y));
  });
});
