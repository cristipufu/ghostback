// Runs inside the agent's existing runtime; no shell, transcript, or model access.
import { spawn } from "node:child_process";
import { basename } from "node:path";

function piLaunchArgv(executable, argv) {
  if (["node", "bun", "node.exe", "bun.exe"].includes(basename(executable))) return argv.slice(1);
  // Native Pi builds can expose their bundled script as a virtual argv entry.
  const args = argv.slice(1);
  const entry = args[0];
  if (entry === executable || entry?.includes("$bunfs/") || entry?.includes("~BUN/")) args.shift();
  return [executable, ...args];
}

const config = __GHOSTBACK_CONFIG__;

function reporter(agent) {
  let pending;
  let running = false;
  let stopped = false;
  let last = "";
  let sentAt = 0;
  let child;
  const launch = agent === "pi" ? { launch_argv: piLaunchArgv(process.execPath, process.argv) } : {};
  const active = Boolean(process.env.GHOSTTY_SURFACE_ID || process.env.GHOSTBACK_HOST_KEY);

  function send(payload) {
    if (!active || stopped) return;
    const body = JSON.stringify({ ...payload, owner_pid: process.pid, ...launch });
    if (!running && !pending && body === last && Date.now() - sentAt < 30000) return;
    pending = { body, tick: process.hrtime.bigint().toString() };
    drain();
  }
  function drain() {
    if (running || stopped || !pending) return;
    const { body, tick } = pending;
    pending = undefined;
    if (body === last && Date.now() - sentAt < 30000) return;
    running = true;
    try {
      child = spawn(config.binary, ["--state-dir", config.state, "hook", agent], {
        stdio: ["pipe", "ignore", "ignore"],
      });
      const timer = setTimeout(() => child?.kill("SIGKILL"), 12000);
      timer.unref?.();
      let finished = false;
      const finish = (ok) => {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        child = undefined;
        running = false;
        if (ok) { last = body; sentAt = Date.now(); }
        if (pending) drain();
      };
      child.on("error", () => finish(false));
      child.on("close", code => finish(code === 0));
      child.stdin.on("error", () => {}); // Closed input must never crash the agent.
      child.stdin.end(JSON.stringify({ ...JSON.parse(body), observed_ns: tick }));
    } catch { running = false; }
  }
  return {
    send,
    stop() {
      stopped = true;
      pending = undefined;
      // Do not kill an in-flight capture on shutdown: its transaction may be committing.
    },
  };
}
