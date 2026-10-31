export default {
  id: "ghostback",
  async tui(api) {
    const report = reporter("opencode");
    const capture = () => {
      try {
        if (!api.state.ready || api.lifecycle.signal.aborted) return;
        const route = api.route.current;
        if (route.name === "home") {
          report.send({ event: "unselected" });
        } else if (route.name === "session") {
          const id = route.params?.sessionID;
          const session = id && api.state.session.get(id);
          // Loading states and plugin routes are not evidence of another conversation.
          if (session) report.send({ session_id: id, cwd: session.directory });
        }
      } catch { /* Loading/disposal must never break the TUI. */ }
    };
    capture();
    const timer = setInterval(capture, 1000);
    timer.unref?.();
    api.lifecycle.onDispose(() => { clearInterval(timer); report.stop(); });
  },
};
