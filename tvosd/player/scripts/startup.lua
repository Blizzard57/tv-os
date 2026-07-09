-- TV OS — playback-start signal.
--
-- Writes a marker file (MPV_HOME/.started) the moment mpv actually loads the
-- stream. tvosd deletes this before launching and then waits for it, so it can
-- tell "playback really started" from "a window that never appears" and show a
-- proper error otherwise. This works for torrents too: webtorrent launches mpv
-- with --really-quiet (which suppresses mpv's log file), but Lua scripts still
-- run, so this signal is reliable where log-parsing isn't.

local mp = require 'mp'

local function mark()
  local path = mp.command_native({ 'expand-path', '~~/.started' })
  local file = io.open(path, 'w')
  if file then
    file:write(tostring(os.time()))
    file:close()
  end
end

-- file-loaded fires once the demuxer is open and tracks are known, i.e. data is
-- actually flowing from the source.
mp.register_event('file-loaded', mark)

-- The on-start "Enhance: <profile>" hint lives in scripts/upscaler.lua, which
-- knows which profile the resolver picked.
