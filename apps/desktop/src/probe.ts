// Dev-only in-engine probe. Activated by running vite with VITE_PROBE=1 (see
// main.ts); never part of a release build. Runs INSIDE the real webview —
// WebKitGTK under tauri, at whatever zoom the app applied — so it exercises
// the engine's actual hit-testing and render pipeline, and POSTs findings to
// a listener on 127.0.0.1:5199. Used to chase paint-vs-hit bugs that only
// reproduce in WebKitGTK (chrome renders the same DOM correctly).

const REPORT = "http://127.0.0.1:5199/report";

async function post(step: string, data: unknown): Promise<void> {
  try {
    await fetch(REPORT, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ step, t: performance.now(), data }),
    });
  } catch {
    /* listener gone — keep probing */
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function scanTabs() {
  const bar = document.querySelector<HTMLElement>(".region.right .tabs");
  if (!bar) return { error: "no right tab bar" };
  const tabs = [...bar.querySelectorAll<HTMLElement>(".tab")];
  const perTab = tabs.map((t) => {
    const r = t.getBoundingClientRect();
    const cx = (r.left + r.right) / 2;
    const dead: string[] = [];
    for (let y = Math.ceil(r.top); y <= Math.floor(r.bottom); y++) {
      const el = document.elementFromPoint(cx, y);
      if (el !== t && !t.contains(el)) dead.push(`${y}:${el?.tagName.toLowerCase() ?? "null"}.${(el as HTMLElement)?.className?.toString().split(" ")[0] ?? ""}`);
    }
    return { tab: t.dataset.tab, top: +r.top.toFixed(1), bottom: +r.bottom.toFixed(1), dead };
  });
  const b = bar.getBoundingClientRect();
  const cx0 = tabs.length ? (tabs[0].getBoundingClientRect().left + tabs[0].getBoundingClientRect().right) / 2 : 0;
  const barGaps: number[] = [];
  for (let y = Math.ceil(b.top) + 1; y < Math.floor(b.bottom) - 1; y++) {
    const el = document.elementFromPoint(cx0, y);
    if (!el?.closest(".tab")) barGaps.push(y);
  }
  return { rows: [...new Set(tabs.map((t) => Math.round(t.getBoundingClientRect().top)))], perTab, barGaps };
}

async function dropdownTest() {
  const box = document.querySelector<HTMLElement>('[data-box="recordings"]');
  const trigger = box?.querySelector<HTMLElement>(".dropdown .trigger");
  if (!trigger) return { error: "no recordings dropdown" };
  trigger.click();
  await sleep(400);
  const openMenu = !!box!.querySelector(".dropdown .menu");
  await post("menu-open", { openMenu });
  await sleep(1500); // window for an outside screenshot of the open menu
  const opt = [...box!.querySelectorAll<HTMLElement>(".dropdown .opt")].find((o) => o.textContent?.includes("from playhead"));
  if (!opt) return { error: "no from-playhead option", openMenu };
  opt.click();
  await sleep(400);
  const menuAfter = !!box!.querySelector(".dropdown .menu");
  const valueAfter = box!.querySelector(".dropdown .cur")?.textContent;
  await post("menu-picked", { menuAfter, valueAfter });
  await sleep(1500); // window for the after screenshot (ghost check)
  return { openMenu, menuAfter, valueAfter };
}

export async function runProbe(): Promise<void> {
  await sleep(3000); // let settings/zoom/init + the startup resync settle
  const zoom = await import("./lib/zoom").then((m) => m.getZoom()).catch(() => -1);
  await post("env", {
    zoom,
    dpr: window.devicePixelRatio,
    innerW: window.innerWidth,
    innerH: window.innerHeight,
    screenW: window.screen.width,
    ua: navigator.userAgent,
  });

  const stores = await import("./lib/stores");
  stores.workspace.set({
    left: { layout: [{ tabs: ["library"], active: "library", weight: 1 }], collapsed: false },
    right: {
      layout: [
        {
          tabs: ["structure", "routines", "profile", "settings", "export", "loops", "devices", "guide"],
          active: "settings",
          weight: 1,
        },
      ],
      collapsed: false,
    },
    stage: { collapsed: [], hidden: [], order: ["isolation", "notes", "metronome", "recordings", "click", "tuner", "drill"] },
  });
  await sleep(500);

  await post("tab-scan", scanTabs());
  await post("dropdown", await dropdownTest());
  await post("done", {});
}
