const ghostty = Application("Ghostty");
if (!ghostty.running()) {
    throw new Error("Ghostty is not running");
}
const result = [];
for (const window of ghostty.windows()) {
    for (const tab of window.tabs()) {
        for (const terminal of tab.terminals()) {
            result.push({
                terminal_key: terminal.id(),
                window_id: window.id(),
                tab_id: tab.id(),
                tab_index: tab.index(),
                cwd: terminal.workingDirectory(),
                tty: terminal.tty(),
                pid: terminal.pid()
            });
        }
    }
}
JSON.stringify(result);
