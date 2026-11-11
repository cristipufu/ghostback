const ghostty = Application("Ghostty");
if (!ghostty.running()) {
    throw new Error("Ghostty is not running");
}
function safe(fn, fallback) {
    try {
        const value = fn();
        return value === undefined || value === null ? fallback : value;
    } catch (_) {
        return fallback;
    }
}
const frontWindowId = safe(() => String(ghostty.frontWindow().id()), "");
const result = [];
for (const window of ghostty.windows()) {
    const winId = String(window.id());
    const winTitle = safe(() => String(window.name()), "");
    for (const tab of window.tabs()) {
        const tabId = String(tab.id());
        const tabTitle = safe(() => String(tab.name()), "");
        const tabSelected = Boolean(safe(() => tab.selected(), false));
        const focusedId = safe(() => String(tab.focusedTerminal().id()), "");
        for (const terminal of tab.terminals()) {
            const termId = String(terminal.id());
            result.push({
                terminal_key: termId,
                window_id: winId,
                tab_id: tabId,
                tab_index: tab.index(),
                cwd: terminal.workingDirectory(),
                tty: terminal.tty(),
                pid: terminal.pid(),
                window_title: winTitle,
                tab_title: tabTitle,
                tab_selected: tabSelected,
                terminal_title: safe(() => String(terminal.name()), ""),
                focused: winId === frontWindowId && tabSelected && termId === focusedId
            });
        }
    }
}
JSON.stringify(result);
