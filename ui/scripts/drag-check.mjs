#!/usr/bin/env node
// Drives the device column preview (`dev/devchain.html`) in headless Chrome with real input
// events through the DevTools protocol, and checks what the column asks the plugin host for.
//
//   node scripts/drag-check.mjs            pointer drags, the move buttons and Alt+arrow keys
//   node scripts/drag-check.mjs --html5    proves no native (HTML5) drag session starts any more -
//                                          the path WKWebView does not deliver (see DeviceChain)
//
// Starts its own vite dev server (port 5199) and Chrome (port 9333, throw-away profile) and stops
// both at the end. Exit code 0 when every check passed. No app, no engine, no plugin involved:
// the Tauri bridge is the stand-in in `src/dev/previewStub.ts`.

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const UI = join(dirname(fileURLToPath(import.meta.url)), "..");
const VITE_PORT = 5199;
const CDP_PORT = 9333;
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const html5 = process.argv.includes("--html5");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(url, tries = 60) {
  for (let i = 0; i < tries; i += 1) {
    try {
      const res = await fetch(url);
      if (res.ok) return res;
    } catch {
      /* not up yet */
    }
    await sleep(250);
  }
  throw new Error(`${url} did not come up`);
}

const vite = spawn("npx", ["vite", "--port", String(VITE_PORT), "--strictPort"], { cwd: UI, stdio: "ignore" });
const profile = mkdtempSync(join(tmpdir(), "looper-drag-check-"));
const chrome = spawn(
  CHROME,
  [
    "--headless=new",
    "--disable-gpu",
    `--remote-debugging-port=${CDP_PORT}`,
    `--user-data-dir=${profile}`,
    "--window-size=480,1000",
    "about:blank",
  ],
  { stdio: "ignore" },
);

let failures = 0;
function check(ok, what) {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures += 1;
}

try {
  await waitFor(`http://localhost:${VITE_PORT}/dev/devchain.html`);
  const targets = await (await waitFor(`http://localhost:${CDP_PORT}/json`)).json();
  const page = targets.find((t) => t.type === "page");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener("open", r, { once: true }));
  let id = 0;
  const waiting = new Map();
  const listeners = [];
  ws.addEventListener("message", (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) {
      waiting.get(msg.id)(msg);
      waiting.delete(msg.id);
    } else if (msg.method) {
      for (const l of listeners) l(msg);
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      id += 1;
      waiting.set(id, (msg) => (msg.error ? reject(new Error(`${method}: ${msg.error.message}`)) : resolve(msg.result)));
      ws.send(JSON.stringify({ id, method, params }));
    });
  const evaluate = async (expression) => (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result.value;

  async function open(query = "") {
    await send("Page.navigate", { url: `http://localhost:${VITE_PORT}/dev/devchain.html${query}` });
    for (let i = 0; i < 40; i += 1) {
      await sleep(150);
      if (await evaluate(`!!document.querySelector('[data-dev="plugin.0"]')`)) return;
    }
    throw new Error("the preview did not render");
  }
  /** Centre of the element, or of the upper quarter of a row's header (`above`). */
  async function point(selector, where = "centre") {
    const r = await evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return null; const b = e.getBoundingClientRect(); return {x: b.left + b.width / 2, y: b.top, h: b.height}; })()`);
    if (!r) throw new Error(`no ${selector}`);
    return { x: r.x, y: where === "above" ? r.y + r.h * 0.25 : where === "below" ? r.y + r.h * 0.75 : r.y + r.h / 2 };
  }
  const mouse = (type, p, buttons = 0) =>
    send("Input.dispatchMouseEvent", { type, x: p.x, y: p.y, button: "left", buttons, clickCount: type === "mouseMoved" ? 0 : 1 });
  const calls = () => evaluate("JSON.stringify(window.__calls)").then((t) => JSON.parse(t));
  const places = async () => (await calls()).filter((c) => c.cmd === "plugin_place").map((c) => c.args);

  async function pointerDrag(to) {
    const from = await point('[data-dev="plugin.0"] .dev-grip');
    await mouse("mousePressed", from, 1);
    const steps = 8;
    for (let i = 1; i <= steps; i += 1) {
      await mouse("mouseMoved", { x: from.x + ((to.x - from.x) * i) / steps, y: from.y + ((to.y - from.y) * i) / steps }, 1);
      await sleep(15);
    }
    await mouse("mouseReleased", to, 0);
    await sleep(200);
  }

  async function html5Drag(to) {
    await send("Input.setInterceptDrags", { enabled: true });
    let data = null;
    listeners.push((msg) => {
      if (msg.method === "Input.dragIntercepted") data = msg.params.data;
    });
    const from = await point('[data-dev="plugin.0"] .dev-h');
    await mouse("mousePressed", from, 1);
    await mouse("mouseMoved", { x: from.x, y: from.y - 10 }, 1);
    await mouse("mouseMoved", { x: from.x, y: from.y - 30 }, 1);
    await sleep(200);
    if (!data) {
      await mouse("mouseReleased", from, 0);
      return false;
    }
    for (const type of ["dragEnter", "dragOver"]) await send("Input.dispatchDragEvent", { type, x: to.x, y: to.y, data });
    await sleep(50);
    await send("Input.dispatchDragEvent", { type: "drop", x: to.x, y: to.y, data });
    await mouse("mouseReleased", to, 0);
    await sleep(200);
    return true;
  }

  const ROWS = ["octaver", "high_pass", "eq", "comp", "delay", "reverb"];
  if (html5) {
    for (const row of ROWS) {
      await open();
      const started = await html5Drag(await point(`[data-dev="${row}"] .dev-h`, "above"));
      const got = await places();
      check(!started && got.length === 0, `no HTML5 drag session from the row towards ${row}: ${started ? "a drag started" : "none"}`);
    }
  } else {
    // 1. Drag the plugin row by its handle above every built-in device; the delay is unfolded.
    for (const row of ROWS) {
      await open();
      await pointerDrag(await point(`[data-dev="${row}"] .dev-h`, "above"));
      const got = await places();
      check(got.length === 1 && got[0].before === row && got[0].to === 0, `drag above ${row} -> ${JSON.stringify(got)}`);
    }
    // ... onto the unfolded delay's body: that is behind the delay, in front of the reverb.
    await open();
    await pointerDrag(await point('[data-dev="delay"] .dev-b'));
    check((await places())[0]?.before === "reverb", "drop on the unfolded delay's controls lands before the reverb");
    // ... and back to the end from in front of the octaver.
    await open();
    await pointerDrag(await point('[data-dev="octaver"] .dev-h', "above"));
    await pointerDrag(await point(".dev-add"));
    const both = await places();
    check(both.length === 2 && both[1].before === null, `back to the end: ${JSON.stringify(both)}`);
    // A click on the handle without moving is no drag.
    await open();
    const grip = await point('[data-dev="plugin.0"] .dev-grip');
    await mouse("mousePressed", grip, 1);
    await mouse("mouseReleased", grip, 0);
    await sleep(150);
    check((await places()).length === 0, "a click is not a move");
    // An indicator line shows while dragging.
    await open();
    const from = await point('[data-dev="plugin.0"] .dev-grip');
    const target = await point('[data-dev="reverb"] .dev-h', "above");
    await mouse("mousePressed", from, 1);
    await mouse("mouseMoved", { x: from.x, y: (from.y + target.y) / 2 }, 1);
    await mouse("mouseMoved", target, 1);
    await sleep(100);
    check(await evaluate(`!!document.querySelector('[data-dev="reverb"].drop-above')`), "the line shows above the reverb while dragging");
    await mouse("mouseReleased", target, 0);

    // 2. The move buttons: up through all seven places.
    await open();
    await mouse("mousePressed", await point('[data-dev="plugin.0"] .dev-main'), 1);
    await mouse("mouseReleased", await point('[data-dev="plugin.0"] .dev-main'), 0);
    await sleep(150);
    check(await evaluate(`!!document.querySelector('[data-dev="plugin.0"] .dev-b')`), "a click on the plug-in row unfolds it");
    for (let i = 0; i < 7; i += 1) {
      const up = await evaluate(`(() => { const b = document.querySelector('[data-dev="plugin.0"] [data-move="up"]'); if (!b || b.disabled) return false; b.click(); return true; })()`);
      if (!up) break;
      await sleep(120);
    }
    const ups = (await places()).map((p) => p.before);
    check(JSON.stringify(ups) === JSON.stringify(["reverb", "delay", "comp", "eq", "high_pass", "octaver"]), `Move up steps through every place: ${JSON.stringify(ups)}`);
    check(await evaluate(`document.querySelector('[data-dev="plugin.0"] [data-move="up"]').disabled`), "at the very front, Move up is disabled");

    // 3. Keyboard: Alt+Arrow on the focused row; with two plugins it steps past the other one.
    await open("?plugins=2");
    await evaluate(`document.querySelector('[data-dev="plugin.1"] .dev-main').focus()`);
    await send("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "ArrowUp", code: "ArrowUp", windowsVirtualKeyCode: 38, modifiers: 1 });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: "ArrowUp", code: "ArrowUp", windowsVirtualKeyCode: 38, modifiers: 1 });
    await sleep(150);
    const keyed = await places();
    check(keyed.length === 1 && keyed[0].from === 1 && keyed[0].to === 0 && keyed[0].before === null, `Alt+Up past the plugin at the same place: ${JSON.stringify(keyed)}`);
  }
  ws.close();
} catch (e) {
  console.error(e);
  failures += 1;
} finally {
  chrome.kill();
  vite.kill();
  await sleep(300);
  rmSync(profile, { recursive: true, force: true });
}
console.log(failures === 0 ? "all checks passed" : `${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
