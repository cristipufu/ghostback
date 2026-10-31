import { existsSync } from "node:fs";

export default function ghostback(pi) {
  let timer;
  let report;
  pi.on("session_start", (_event, ctx) => {
    clearInterval(timer);
    report?.stop();
    if (ctx.mode !== "tui") return;
    report = reporter("pi");
    const capture = () => {
      try {
        const manager = ctx.sessionManager;
        const file = manager.getSessionFile();
        report.send(file && existsSync(file)
          ? { session_id: manager.getSessionId(), session_file: file, cwd: manager.getCwd() }
          : { event: "unselected" });
      } catch { /* A disposed context is not a conversation selection. */ }
    };
    capture();
    timer = setInterval(capture, 1000);
    timer.unref?.();
  });
  pi.on("session_shutdown", () => {
    clearInterval(timer);
    report?.stop();
  });
}
