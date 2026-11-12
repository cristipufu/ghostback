//! Native Ghostty AppleScript automation scripts for focus, title overrides, and split/tab creation.

pub const FOCUS_SCRIPT: &str = r#"on run argv
    set targetId to item 1 of argv
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        set winId to ""
        set tabId to ""
        set trmId to ""
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    if (id of trm as text) is targetId then
                        set winId to (id of w as text)
                        set tabId to (id of tb as text)
                        set trmId to (id of trm as text)
                        exit repeat
                    end if
                end repeat
                if trmId is not "" then exit repeat
            end repeat
            if trmId is not "" then exit repeat
        end repeat
        if trmId is "" then error "Terminal is no longer open in Ghostty"
        activate
        activate window (window id winId)
        select tab (tab id tabId of window id winId)
        focus (terminal id trmId)
        return trmId
    end tell
end run"#;

pub const TITLE_SCRIPT: &str = r#"on run argv
    set targetId to item 1 of argv
    set newTitle to item 2 of argv
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        set winId to ""
        set tabId to ""
        set trmId to ""
        set singlePane to false
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    if (id of trm as text) is targetId then
                        set winId to (id of w as text)
                        set tabId to (id of tb as text)
                        set trmId to (id of trm as text)
                        set singlePane to ((count of terminals of tb) is 1)
                        exit repeat
                    end if
                end repeat
                if trmId is not "" then exit repeat
            end repeat
            if trmId is not "" then exit repeat
        end repeat
        if trmId is "" then error "Terminal is no longer open in Ghostty"
        set name override of (terminal id trmId) to newTitle
        if singlePane then
            set name override of (tab id tabId of window id winId) to newTitle
        end if
        return trmId
    end tell
end run"#;

pub const SPAWN_SCRIPT: &str = r#"on run argv
    set targetId to item 1 of argv
    set mode to item 2 of argv
    set workDir to item 3 of argv
    set launchCmd to item 4 of argv
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        set cfg to new surface configuration
        if workDir is not "" then
            set initial working directory of cfg to workDir
        end if
        if launchCmd is not "" then
            set command of cfg to launchCmd
        end if
        if mode is "window" then
            activate
            set w to new window with configuration cfg
            return id of focused terminal of selected tab of w as text
        end if
        set winId to ""
        set trmId to ""
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    if targetId is "" or (id of trm as text) is targetId then
                        set winId to (id of w as text)
                        set trmId to (id of trm as text)
                        exit repeat
                    end if
                end repeat
                if trmId is not "" then exit repeat
            end repeat
            if trmId is not "" then exit repeat
        end repeat
        if trmId is "" and (count of windows) > 0 then
            set winId to (id of front window as text)
            set trmId to (id of focused terminal of selected tab of front window as text)
        end if
        if trmId is not "" then
            activate
            if mode is "tab" then
                set newTb to new tab in (window id winId) with configuration cfg
                return id of focused terminal of newTb as text
            else if mode is "right" then
                set newTrm to split (terminal id trmId) direction right with configuration cfg
                focus newTrm
                return id of newTrm as text
            else if mode is "sidebar_right" then
                set newTrm to split (terminal id trmId) direction right with configuration cfg
                set name override of (terminal id trmId) to "◈ HERD"
                perform action "resize_split:left,260" on (terminal id trmId)
                focus newTrm
                return id of newTrm as text
            else if mode is "down" then
                set newTrm to split (terminal id trmId) direction down with configuration cfg
                focus newTrm
                return id of newTrm as text
            else if mode is "left" then
                set newTrm to split (terminal id trmId) direction left with configuration cfg
                set name override of newTrm to "◈ HERD"
                perform action "resize_split:left,260" on newTrm
                focus newTrm
                return id of newTrm as text
            end if
        end if
        if mode is "tab" then
            activate
            set newTb to new tab with configuration cfg
            return id of focused terminal of newTb as text
        end if
        error "Target terminal is no longer open in Ghostty"
    end tell
end run"#;

pub const FOCUS_FROM_SIDEBAR_SCRIPT: &str = r#"on run argv
    set targetId to item 1 of argv
    set selfId to item 2 of argv
    set workDir to item 3 of argv
    set sidebarCmd to item 4 of argv
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        set winId to ""
        set tabId to ""
        set trmId to ""
        set selfTabId to ""
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    set tid to (id of trm as text)
                    if tid is targetId then
                        set winId to (id of w as text)
                        set tabId to (id of tb as text)
                        set trmId to tid
                    end if
                    if selfId is not "" and tid is selfId then
                        set selfTabId to (id of tb as text)
                    end if
                end repeat
            end repeat
        end repeat
        if trmId is "" then error "Terminal is no longer open in Ghostty"
        activate
        activate window (window id winId)
        select tab (tab id tabId of window id winId)
        if selfTabId is not "" and selfTabId is tabId then
            focus (terminal id trmId)
            return "same_tab"
        end if
        set existingSideId to ""
        set firstTrmId to trmId
        set idx to 0
        repeat with trm in terminals of (tab id tabId of window id winId)
            set idx to idx + 1
            if idx is 1 then
                set firstTrmId to (id of trm as text)
            end if
            if (name override of trm as text) is "◈ HERD" or (name of trm as text) is "◈ HERD" then
                set existingSideId to (id of trm as text)
            end if
        end repeat
        if existingSideId is "" and sidebarCmd is not "" then
            set cfg to new surface configuration
            if workDir is not "" then
                set initial working directory of cfg to workDir
            end if
            set command of cfg to sidebarCmd
            set environment variables of cfg to {"GHOSTBACK_SELECT_KEY=" & targetId}
            set sideTrm to split (terminal id firstTrmId) direction left with configuration cfg
            set name override of sideTrm to "◈ HERD"
            perform action "resize_split:left,260" on sideTrm
        end if
        focus (terminal id trmId)
        return "moved_tab"
    end tell
end run"#;

pub const SPAWN_FROM_SIDEBAR_SCRIPT: &str = r#"on run argv
    set targetId to item 1 of argv
    set workDir to item 2 of argv
    set launchCmd to item 3 of argv
    set sidebarCmd to item 4 of argv
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        set cfg to new surface configuration
        if workDir is not "" then
            set initial working directory of cfg to workDir
        end if
        if launchCmd is not "" then
            set command of cfg to launchCmd
        end if
        set winId to ""
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    if targetId is "" or (id of trm as text) is targetId then
                        set winId to (id of w as text)
                        exit repeat
                    end if
                end repeat
                if winId is not "" then exit repeat
            end repeat
            if winId is not "" then exit repeat
        end repeat
        activate
        if winId is not "" then
            set newTb to new tab in (window id winId) with configuration cfg
        else
            set newTb to new tab with configuration cfg
        end if
        set newTrmId to (id of focused terminal of newTb as text)
        if sidebarCmd is not "" then
            set sideCfg to new surface configuration
            if workDir is not "" then
                set initial working directory of sideCfg to workDir
            end if
            set command of sideCfg to sidebarCmd
            set environment variables of sideCfg to {"GHOSTBACK_SELECT_KEY=" & newTrmId}
            set sideTrm to split (terminal id newTrmId) direction left with configuration sideCfg
            set name override of sideTrm to "◈ HERD"
            perform action "resize_split:left,260" on sideTrm
        end if
        focus (terminal id newTrmId)
        return newTrmId
    end tell
end run"#;

pub const CLOSE_TERMINAL_SCRIPT: &str = r#"on run argv
    set targetId to item 1 of argv
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    if (id of trm as text) is targetId then
                        close trm
                        return targetId
                    end if
                end repeat
            end repeat
        end repeat
        return ""
    end tell
end run"#;

pub const APPLY_GRID_SCRIPT: &str = r#"on run argv
    set sideId to item 1 of argv
    set rowCount to (item 2 of argv) as integer
    set colCount to (item 3 of argv) as integer
    tell application "Ghostty"
        if not running then error "Ghostty is not running"
        set winId to ""
        set tabId to ""
        repeat with w in windows
            repeat with tb in tabs of w
                repeat with trm in terminals of tb
                    if sideId is "" or (id of trm as text) is sideId then
                        set winId to (id of w as text)
                        set tabId to (id of tb as text)
                        set sideId to (id of trm as text)
                        exit repeat
                    end if
                end repeat
                if tabId is not "" then exit repeat
            end repeat
            if tabId is not "" then exit repeat
        end repeat
        if tabId is "" and (count of windows) > 0 then
            set winId to (id of front window as text)
            set tabId to (id of selected tab of front window as text)
            set sideId to (id of focused terminal of selected tab of front window as text)
        end if
        if tabId is "" then error "Sidebar terminal is no longer open in Ghostty"
        activate
        activate window (window id winId)
        select tab (tab id tabId of window id winId)
        set oldIds to {}
        repeat with trm in terminals of (tab id tabId of window id winId)
            set tid to (id of trm as text)
            if tid is not sideId then
                set end of oldIds to tid
            end if
        end repeat
        repeat with oldId in oldIds
            try
                close (terminal id (oldId as text))
            end try
        end repeat
        set name override of (terminal id sideId) to "◈ HERD"
        set rowFirstIds to {}
        repeat with r from 1 to rowCount
            set cellIdx to ((r - 1) * colCount) + 1
            set baseIdx to 3 + ((cellIdx - 1) * 3)
            set cCwd to item (baseIdx + 1) of argv
            set cTitle to item (baseIdx + 2) of argv
            set cCmd to item (baseIdx + 3) of argv
            set cfg to new surface configuration
            if cCwd is not "" then
                set initial working directory of cfg to cCwd
            end if
            if cCmd is not "" then
                set command of cfg to cCmd
            end if
            if r is 1 then
                set newTrm to split (terminal id sideId) direction right with configuration cfg
            else
                set prevRowId to item (r - 1) of rowFirstIds
                set newTrm to split (terminal id prevRowId) direction down with configuration cfg
            end if
            if cTitle is not "" then
                set name override of newTrm to cTitle
            end if
            set end of rowFirstIds to (id of newTrm as text)
        end repeat
        set outLines to ""
        repeat with r from 1 to rowCount
            set prevId to item r of rowFirstIds
            if outLines is "" then
                set outLines to prevId
            else
                set outLines to outLines & linefeed & prevId
            end if
            if colCount > 1 then
                repeat with c from 2 to colCount
                    set cellIdx to ((r - 1) * colCount) + c
                    set baseIdx to 3 + ((cellIdx - 1) * 3)
                    set cCwd to item (baseIdx + 1) of argv
                    set cTitle to item (baseIdx + 2) of argv
                    set cCmd to item (baseIdx + 3) of argv
                    set cfg to new surface configuration
                    if cCwd is not "" then
                        set initial working directory of cfg to cCwd
                    end if
                    if cCmd is not "" then
                        set command of cfg to cCmd
                    end if
                    set newTrm to split (terminal id prevId) direction right with configuration cfg
                    if cTitle is not "" then
                        set name override of newTrm to cTitle
                    end if
                    set prevId to (id of newTrm as text)
                    set outLines to outLines & linefeed & prevId
                end repeat
            end if
        end repeat
        if (count of rowFirstIds) > 0 then
            perform action "equalize_splits" on (terminal id (item 1 of rowFirstIds))
        end if
        perform action "resize_split:left,260" on (terminal id sideId)
        focus (terminal id sideId)
        return outLines
    end tell
end run"#;
