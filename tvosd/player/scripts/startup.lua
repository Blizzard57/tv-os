-- TV OS — playback-start signal.
--
-- Writes a marker file (MPV_HOME/.started) the moment mpv actually loads the
-- stream. tvosd deletes this before launching and then waits for it, so it can
-- tell "playback really started" from "a window that never appears" and show a
-- proper error otherwise. This works for torrents too: webtorrent launches mpv
-- with --really-quiet (which suppresses mpv's log file), but Lua scripts still
-- run, so this signal is reliable where log-parsing isn't. It also writes a
-- small health file while playback runs; tvosd can use that for diagnostics
-- without delaying the launch response that the shell is waiting for.

local mp = require 'mp'

local loaded_at = nil
local timer = nil

local function write_file(name, text)
  local path = mp.command_native({ 'expand-path', '~~/' .. name })
  local file = io.open(path, 'w')
  if file then
    file:write(text)
    file:close()
  end
end

local function bool_json(value)
  return value and 'true' or 'false'
end

local function number_json(value)
  return string.format('%.3f', tonumber(value) or 0)
end

local function write_health()
  if not loaded_at then return end
  local played = mp.get_property_number('time-pos', 0) or 0
  local paused_for_cache = mp.get_property_bool('paused-for-cache', false) or false
  local eof = mp.get_property_bool('eof-reached', false) or false
  local idle = mp.get_property_bool('idle-active', false) or false
  local cache = mp.get_property_native('demuxer-cache-state') or {}
  local ahead = 0
  if type(cache) == 'table' then
    ahead = tonumber(cache['fw-duration'] or 0) or 0
  end
  write_file('.health', string.format(
    '{"time":%d,"loaded_for":%s,"played":%s,"cache_ahead":%s,"paused_for_cache":%s,"eof":%s,"idle":%s}',
    os.time(),
    number_json(mp.get_time() - loaded_at),
    number_json(played),
    number_json(ahead),
    bool_json(paused_for_cache),
    bool_json(eof),
    bool_json(idle)
  ))
end

local function mark()
  loaded_at = mp.get_time()
  write_file('.started', tostring(os.time()))
  write_health()
  if timer then timer:kill() end
  timer = mp.add_periodic_timer(0.5, write_health)
end

-- file-loaded fires once the demuxer is open and tracks are known, i.e. data is
-- actually flowing from the source.
mp.register_event('file-loaded', mark)
mp.register_event('end-file', write_health)
mp.observe_property('paused-for-cache', 'bool', write_health)
mp.observe_property('time-pos', 'number', write_health)

-- The on-start "Enhance: <profile>" hint lives in scripts/upscaler.lua, which
-- knows which profile the resolver picked.
