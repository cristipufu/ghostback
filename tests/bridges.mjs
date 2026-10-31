// Development-only bridge contracts. Ghostback itself does not require Node.
import test from "node:test";
import assert from "node:assert/strict";
import { readFile, mkdtemp, writeFile, rm, chmod } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

async function adapter(name) {
  const seen = [];
  let tick;
  let disposed;
  const prefix = `
const reporter = () => globalThis[${JSON.stringify(name)}].report;
const setInterval = callback => { globalThis[${JSON.stringify(name)}].tick = callback; return { unref() {} }; };
const clearInterval = () => {};
`;
  globalThis[name] = { report: { send: value => seen.push(value), stop() {} }, tick };
  const source = await readFile(new URL(`../integrations/${name}.js`, import.meta.url), "utf8");
  const module = await import(`data:text/javascript;base64,${Buffer.from(prefix + source).toString("base64")}`);
  return { module, seen, tick: () => globalThis[name].tick?.(), dispose: () => disposed?.(), onDispose: fn => disposed = fn };
}

test("Pi captures persistent selections, clears ephemeral ones, and ignores non-TUI modes", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "ghostback-pi-bridge-"));
  try {
    const file = path.join(root, "exact 'session'.jsonl");
    const bridge = await adapter("pi");
    const events = {};
    bridge.module.default({ on: (name, fn) => events[name] = fn });
    const ctx = { mode: "rpc", sessionManager: {} };
    events.session_start({}, ctx);
    assert.equal(bridge.seen.length, 0);
    ctx.mode = "tui";
    ctx.sessionManager = { getSessionFile: () => file, getSessionId: () => "Custom-ID", getCwd: () => root };
    events.session_start({}, ctx);
    assert.deepEqual(bridge.seen.at(-1), { event: "unselected" });
    await writeFile(file, "session metadata");
    bridge.tick();
    assert.deepEqual(bridge.seen.at(-1), { session_id: "Custom-ID", session_file: file, cwd: root });
    ctx.sessionManager.getSessionFile = () => undefined;
    bridge.tick();
    assert.deepEqual(bridge.seen.at(-1), { event: "unselected" });
    const count = bridge.seen.length;
    events.session_shutdown({ reason: "quit" }, ctx);
    assert.equal(bridge.seen.length, count); // Shutdown is not proof of deliberate exit.
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("OpenCode follows the selected TUI route, never unrelated server activity", async () => {
  const bridge = await adapter("opencode");
  const api = {
    state: { ready: false, session: { get: id => id === "ses_A" ? { directory: "/project" } : undefined } },
    route: { current: { name: "home" } },
    lifecycle: { signal: { aborted: false }, onDispose: bridge.onDispose },
  };
  await bridge.module.default.tui(api);
  assert.equal(bridge.seen.length, 0);
  api.state.ready = true;
  bridge.tick();
  assert.deepEqual(bridge.seen.at(-1), { event: "unselected" });
  api.route.current = { name: "session", params: { sessionID: "ses_A" } };
  bridge.tick();
  assert.deepEqual(bridge.seen.at(-1), { session_id: "ses_A", cwd: "/project" });
  const count = bridge.seen.length;
  api.route.current = { name: "session", params: { sessionID: "ses_unloaded" } };
  bridge.tick();
  assert.equal(bridge.seen.length, count);
  api.route.current = { name: "custom-plugin-route" };
  bridge.tick();
  assert.equal(bridge.seen.length, count);
  bridge.dispose();
});

test("reporter orders observations, deduplicates, and survives failed child launches", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "ghostback-reporter-"));
  const previous = process.env.GHOSTTY_SURFACE_ID;
  const output = path.join(root, "captures.jsonl");
  const previousOutput = process.env.GHOSTBACK_TEST_CAPTURE;
  try {
    process.env.GHOSTTY_SURFACE_ID = "1";
    process.env.GHOSTBACK_TEST_CAPTURE = output;
    const executable = path.join(root, "capture.js");
    await writeFile(executable, `#!/usr/bin/env node\nconst fs=require('node:fs');let data='';process.stdin.on('data',c=>data+=c);process.stdin.on('end',()=>fs.appendFileSync(process.env.GHOSTBACK_TEST_CAPTURE,data+'\\n'));\n`);
    await chmod(executable, 0o700);
    const source = await readFile(new URL("../integrations/bridge.js", import.meta.url), "utf8");
    async function reporter(binary) {
      const code = source.replace("__GHOSTBACK_CONFIG__", JSON.stringify({ binary, state: root })) + "\nexport { reporter };";
      return (await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`)).reporter("opencode");
    }
    const report = await reporter(executable);
    report.send({ session_id: "ses_A" });
    report.send({ session_id: "ses_A" });
    report.send({ session_id: "ses_B" });
    const start = Date.now();
    let rows;
    while (Date.now() - start < 5000) {
      try { rows = (await readFile(output,"utf8")).trim().split("\n").map(JSON.parse); } catch {}
      if (rows?.length === 2) break;
      await new Promise(resolve => setTimeout(resolve, 25));
    }
    assert.deepEqual(rows.map(r => r.session_id), ["ses_A", "ses_B"]);
    assert.equal(rows[0].owner_pid, process.pid);
    assert.ok(BigInt(rows[0].observed_ns) < BigInt(rows[1].observed_ns));
    // Returning to the previous selection while another hook runs must still be delivered.
    report.send({ session_id: "ses_A" });
    report.send({ session_id: "ses_B" });
    const secondStart = Date.now();
    while (Date.now() - secondStart < 5000) {
      rows = (await readFile(output,"utf8")).trim().split("\n").map(JSON.parse);
      if (rows.length === 4) break;
      await new Promise(resolve => setTimeout(resolve, 25));
    }
    assert.deepEqual(rows.map(r => r.session_id), ["ses_A", "ses_B", "ses_A", "ses_B"]);
    report.stop();
    const failed = await reporter(path.join(root, "does-not-exist"));
    failed.send({ session_id: "ses_C" });
    await new Promise(resolve => setTimeout(resolve, 100));
    failed.stop();
  } finally {
    if (previous === undefined) delete process.env.GHOSTTY_SURFACE_ID; else process.env.GHOSTTY_SURFACE_ID = previous;
    if (previousOutput === undefined) delete process.env.GHOSTBACK_TEST_CAPTURE; else process.env.GHOSTBACK_TEST_CAPTURE = previousOutput;
    await rm(root, { recursive: true, force: true });
  }
});
