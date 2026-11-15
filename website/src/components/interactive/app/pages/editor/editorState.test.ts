import { describe, expect, it } from "vitest";
import {
  deleteAt,
  formatClock,
  historyReducer,
  initialDoc,
  initialHistory,
  parseClock,
  segments,
  setEdge,
  split,
} from "./editorState";

describe("editorState", () => {
  it("formats and parses clock values", () => {
    expect(formatClock(124)).toBe("00:12.400");
    expect(formatClock(600)).toBe("01:00.000");
    expect(parseClock("00:45.000")).toBe(450);
    expect(parseClock("1:05")).toBe(650);
    expect(parseClock("12.5")).toBe(125);
    expect(parseClock("0:75")).toBeNull();
    expect(parseClock("abc")).toBeNull();
  });

  it("clamps trim edges to MIN_TRIM", () => {
    const d = initialDoc(600);
    expect(setEdge(d, "video", "start", 9999).trims.video).toEqual({
      start: 590,
      end: 600,
    });
    expect(setEdge(d, "video", "end", -5).trims.video).toEqual({
      start: 0,
      end: 10,
    });
    expect(setEdge(d, "video", "start", 0)).toBe(d);
  });

  it("splits and deletes video segments", () => {
    let d = split(initialDoc(600), 200);
    d = split(d, 400);
    expect(segments(d)).toHaveLength(3);
    d = deleteAt(d, 300);
    expect(d.removed).toEqual([{ start: 200, end: 400 }]);
    expect(split(d, 300)).toBe(d);
    expect(deleteAt(initialDoc(600), 10).removed).toEqual([]);
  });

  it("undoes edits and records one step per drag", () => {
    let h = initialHistory(600);
    const start = h.present.trims.video.start;
    h = historyReducer(h, {
      type: "edit",
      fn: (d) => setEdge(d, "video", "start", 50),
    });
    h = historyReducer(h, { type: "dragStart" });
    for (const v of [60, 70, 80]) {
      h = historyReducer(h, {
        type: "edit",
        fn: (d) => setEdge(d, "video", "start", v),
      });
    }
    h = historyReducer(h, { type: "dragEnd" });
    expect(h.past).toHaveLength(2);
    h = historyReducer(h, { type: "undo" });
    expect(h.present.trims.video.start).toBe(50);
    h = historyReducer(h, { type: "undo" });
    expect(h.present.trims.video.start).toBe(start);
    h = historyReducer(h, { type: "redo" });
    expect(h.present.trims.video.start).toBe(50);
  });
});
