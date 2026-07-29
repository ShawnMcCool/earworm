import { describe, expect, it } from "vitest";
import {
  adjustWindow,
  clampToLoop,
  followView,
  makePlayheadClock,
  playheadSecs,
  secToX,
  snapToGrid,
  subdivisionTimes,
  tickPlayhead,
  visibleBuckets,
  xToSec,
  zoom,
  type View,
} from "./waveform-math";

describe("subdivisionTimes", () => {
  const beats = [0, 1, 2, 3];
  const downbeats = [0, 2];
  it("bar → downbeats, beat → beats", () => {
    expect(subdivisionTimes(beats, downbeats, "bar")).toEqual(downbeats);
    expect(subdivisionTimes(beats, downbeats, "beat")).toEqual(beats);
  });
  it("eighth interleaves beat midpoints", () => {
    expect(subdivisionTimes(beats, downbeats, "eighth")).toEqual([0, 0.5, 1, 1.5, 2, 2.5, 3]);
  });
});

const view: View = { startSec: 10, endSec: 30, width: 800 };

describe("secToX / xToSec", () => {
  it("maps view edges to canvas edges", () => {
    expect(secToX(view, 10)).toBe(0);
    expect(secToX(view, 30)).toBe(800);
    expect(secToX(view, 20)).toBe(400);
  });

  it("round-trips", () => {
    for (const s of [10, 13.37, 22.5, 30]) {
      expect(xToSec(view, secToX(view, s))).toBeCloseTo(s, 9);
    }
    for (const x of [0, 123, 400, 800]) {
      expect(secToX(view, xToSec(view, x))).toBeCloseTo(x, 9);
    }
  });
});

describe("clampToLoop", () => {
  const loop = { start: 10, end: 20 };
  it("passes through when no loop is engaged", () => {
    expect(clampToLoop(25, null)).toBe(25);
  });
  it("keeps an in-loop playhead untouched", () => {
    expect(clampToLoop(15, loop)).toBe(15);
  });
  it("clamps an overshoot to the loop end", () => {
    expect(clampToLoop(20.4, loop)).toBe(20);
  });
  it("clamps a backward extrapolation to the loop start", () => {
    expect(clampToLoop(9.5, loop)).toBe(10);
  });
});

describe("playheadSecs", () => {
  it("extrapolates by wall time times rate", () => {
    const t0 = 5000;
    const pos = { secs: 10, rate: 0.75, playing: true, at: t0 };
    expect(playheadSecs(pos, t0 + 2000)).toBeCloseTo(11.5, 9);
  });

  it("freezes when paused", () => {
    const pos = { secs: 10, rate: 0.75, playing: false, at: 5000 };
    expect(playheadSecs(pos, 9000)).toBe(10);
  });

  it("freezes during the count-in even though playing", () => {
    const pos = { secs: 10, rate: 1, playing: true, at: 5000, countIn: { beat: 2, of: 4 } };
    expect(playheadSecs(pos, 9000)).toBe(10);
  });
});

describe("tickPlayhead", () => {
  it("returns the exact position when paused", () => {
    const c = makePlayheadClock();
    const pos = { secs: 12, rate: 1, playing: false, at: 1000 };
    expect(tickPlayhead(c, pos, 1000)).toBe(12);
    expect(tickPlayhead(c, pos, 2000)).toBe(12);
  });

  it("locks to truth on the first playing frame", () => {
    const c = makePlayheadClock();
    const pos = { secs: 10, rate: 1, playing: true, at: 1000 };
    // at == now → target is exactly secs
    expect(tickPlayhead(c, pos, 1000)).toBeCloseTo(10, 9);
  });

  it("holds the playhead in place during the count-in", () => {
    const c = makePlayheadClock();
    const pos = { secs: 10, rate: 1, playing: true, at: 1000, countIn: { beat: 1, of: 4 } };
    tickPlayhead(c, pos, 1000);
    // wall time passes, but a held count-in must not advance the playhead
    expect(tickPlayhead(c, pos, 1500)).toBe(10);
    expect(tickPlayhead(c, pos, 2000)).toBe(10);
  });

  it("advances at rate across smooth frames (no jitter sawtooth)", () => {
    const c = makePlayheadClock();
    const pos = { secs: 10, rate: 1, playing: true, at: 1000 };
    tickPlayhead(c, pos, 1000); // init
    // step 16ms frames; display should track real time closely and monotonically
    let prev = c.display;
    for (let t = 1016; t <= 1300; t += 16) {
      const v = tickPlayhead(c, pos, t);
      expect(v).toBeGreaterThanOrEqual(prev); // monotonic, never steps back
      prev = v;
    }
    // ~0.3 s of wall time at rate 1 ≈ +0.3 s
    expect(c.display).toBeCloseTo(10.3, 1);
  });

  it("snaps backward on a loop wrap", () => {
    const c = makePlayheadClock();
    let pos = { secs: 10, rate: 1, playing: true, at: 1000 };
    tickPlayhead(c, pos, 1000);
    tickPlayhead(c, pos, 1050);
    // loop wraps back to 2.0
    pos = { secs: 2, rate: 1, playing: true, at: 1060 };
    expect(tickPlayhead(c, pos, 1060)).toBeCloseTo(2, 2);
  });

  it("resyncs after a stall / resume gap", () => {
    const c = makePlayheadClock();
    const pos = { secs: 10, rate: 1, playing: true, at: 1000 };
    tickPlayhead(c, pos, 1000);
    // a >100ms gap (tab was hidden / just resumed) snaps instead of lurching
    const pos2 = { secs: 30, rate: 1, playing: true, at: 5000 };
    expect(tickPlayhead(c, pos2, 5000)).toBeCloseTo(30, 2);
  });
});

describe("zoom", () => {
  const duration = 100;

  it("keeps the anchor's pixel position stable", () => {
    const anchor = 22.5;
    const xBefore = secToX(view, anchor);
    const zoomed = zoom(view, anchor, 0.5, duration);
    expect(secToX(zoomed, anchor)).toBeCloseTo(xBefore, 6);
    expect(zoomed.endSec - zoomed.startSec).toBeCloseTo(10, 9);
  });

  it("clamps at song bounds", () => {
    const out = zoom({ startSec: 0, endSec: 90, width: 800 }, 80, 2, duration);
    expect(out.startSec).toBeGreaterThanOrEqual(0);
    expect(out.endSec).toBeLessThanOrEqual(duration);
    expect(out.endSec - out.startSec).toBeCloseTo(duration, 9);
  });

  it("clamps at the 2 s minimum span", () => {
    const tight: View = { startSec: 20, endSec: 22.5, width: 800 };
    const out = zoom(tight, 21, 0.1, duration);
    expect(out.endSec - out.startSec).toBeCloseTo(2, 9);
  });

  it("never moves the window start below zero", () => {
    const out = zoom({ startSec: 0, endSec: 20, width: 800 }, 1, 2, duration);
    expect(out.startSec).toBeGreaterThanOrEqual(0);
  });
});

describe("followView", () => {
  const duration = 100;
  // 20 s span; margin 0.2 → dead-zone band is [start+4, end-4]
  const v: View = { startSec: 10, endSec: 30, width: 800 };

  it("does not scroll while the playhead is inside the band", () => {
    expect(followView(v, 15, duration, 0.2)).toBe(v); // just inside left edge
    expect(followView(v, 25, duration, 0.2)).toBe(v); // just inside right edge
    expect(followView(v, 20, duration, 0.2)).toBe(v); // centre
  });

  it("scrolls forward, pinning the playhead at the right boundary", () => {
    const out = followView(v, 28, duration, 0.2); // past hi (26)
    expect(out).not.toBe(v);
    expect(out.endSec - out.startSec).toBeCloseTo(20, 9); // span preserved
    // playhead now sits on the right dead-zone boundary (end - margin*span)
    expect(out.endSec - 0.2 * 20).toBeCloseTo(28, 9);
  });

  it("scrolls backward (e.g. a loop wrap), pinning at the left boundary", () => {
    const out = followView(v, 12, duration, 0.2); // before lo (14)
    expect(out).not.toBe(v);
    expect(out.startSec + 0.2 * 20).toBeCloseTo(12, 9);
  });

  it("clamps at song start without overscrolling", () => {
    const near: View = { startSec: 2, endSec: 22, width: 800 };
    const out = followView(near, 1, duration, 0.2); // would want start = -3
    expect(out.startSec).toBe(0);
    expect(out.endSec).toBeCloseTo(20, 9);
  });

  it("clamps at song end without overscrolling", () => {
    const near: View = { startSec: 78, endSec: 98, width: 800 };
    const out = followView(near, 99, duration, 0.2); // would push past the end
    expect(out.endSec).toBeLessThanOrEqual(duration);
    expect(out.startSec).toBeCloseTo(80, 9); // duration - span
  });

  it("never scrolls when the whole song already fits", () => {
    const whole: View = { startSec: 0, endSec: 100, width: 800 };
    expect(followView(whole, 90, duration, 0.2)).toBe(whole);
  });
});

describe("snapToGrid", () => {
  // view: 20 s over 800 px → 40 px per second; threshold 10 px = 0.25 s
  const downbeats = [12, 14, 16, 18];

  it("snaps to the nearest downbeat within the pixel threshold", () => {
    expect(snapToGrid(14.2, downbeats, view, 10)).toBe(14);
    expect(snapToGrid(13.8, downbeats, view, 10)).toBe(14);
  });

  it("leaves values outside the threshold alone", () => {
    expect(snapToGrid(13.0, downbeats, view, 10)).toBe(13.0);
    expect(snapToGrid(14.6, downbeats, view, 10)).toBe(14.6);
  });

  it("threshold scales with zoom (px, not seconds)", () => {
    // 4× wider view → 10 px is 1 s, so 14.6 now snaps
    const wide: View = { startSec: 0, endSec: 80, width: 800 };
    expect(snapToGrid(14.6, downbeats, wide, 10)).toBe(14);
  });

  it("exact downbeat stays put", () => {
    expect(snapToGrid(16, downbeats, view, 10)).toBe(16);
  });

  it("no downbeats → identity", () => {
    expect(snapToGrid(13.9, [], view, 10)).toBe(13.9);
  });

  it("Infinity threshold locks to the nearest downbeat regardless of distance", () => {
    expect(snapToGrid(12.9, downbeats, view, Infinity)).toBe(12);
    expect(snapToGrid(19.9, downbeats, view, Infinity)).toBe(18);
  });
});

describe("adjustWindow", () => {
  it("pans within bounds, preserving width", () => {
    expect(adjustWindow("pan", -5, 5, 100, 1)).toEqual({ startSec: 0, endSec: 10 });
    expect(adjustWindow("pan", 95, 105, 100, 1)).toEqual({ startSec: 90, endSec: 100 });
    expect(adjustWindow("pan", 20, 30, 100, 1)).toEqual({ startSec: 20, endSec: 30 });
  });
  it("resizes the start edge, keeping the end and a min width", () => {
    expect(adjustWindow("start", 40, 60, 100, 1)).toEqual({ startSec: 40, endSec: 60 });
    expect(adjustWindow("start", 59.5, 60, 100, 1)).toEqual({ startSec: 59, endSec: 60 }); // clamped to min width 1
    expect(adjustWindow("start", -10, 60, 100, 1)).toEqual({ startSec: 0, endSec: 60 });
  });
  it("resizes the end edge, keeping the start and a min width", () => {
    expect(adjustWindow("end", 40, 40.5, 100, 1)).toEqual({ startSec: 40, endSec: 41 }); // min width
    expect(adjustWindow("end", 40, 200, 100, 1)).toEqual({ startSec: 40, endSec: 100 }); // clamp to duration
  });
});

describe("visibleBuckets", () => {
  // 1024 frames per bucket at 48 kHz ≈ 21.33 ms per bucket
  const fpb = 1024;
  const sr = 48000;

  it("returns the bucket range covering the view", () => {
    const v: View = { startSec: 1, endSec: 2, width: 800 };
    const { first, last } = visibleBuckets(v, fpb, sr, 10000);
    expect(first).toBe(Math.floor((1 * sr) / fpb));
    expect(last).toBe(Math.ceil((2 * sr) / fpb));
  });

  it("clamps to [0, totalBuckets - 1]", () => {
    const v: View = { startSec: -5, endSec: 999, width: 800 };
    const { first, last } = visibleBuckets(v, fpb, sr, 100);
    expect(first).toBe(0);
    expect(last).toBe(99);
  });
});
