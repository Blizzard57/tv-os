-- TV OS — saves the playback position so "Continue" can resume.
--
-- tvosd passes the per-item position file as $TVOS_POSITION_FILE and resumes by
-- writing `start=` into mpv.conf on the next launch. Here we just keep that file
-- updated, and zero it when the title finishes so a watched item doesn't resume.

local mp = require 'mp'
local utils = require 'mp.utils'

local path = os.getenv('TVOS_POSITION_FILE')
if not path or path == '' then return end

local content_id = os.getenv('TVOS_CONTENT_ID')
if not content_id or content_id == '' then content_id = os.getenv('TVOS_ITEM_ID') end
local track_id = os.getenv('TVOS_ITEM_ID') or content_id
local session_id = os.getenv('TVOS_PLAYBACK_SESSION') or ('mpv-' .. tostring(os.time()))
local sequence = 0

local function json_escape(s)
  return tostring(s or ''):gsub('\\', '\\\\'):gsub('"', '\\"'):gsub('\n', '\\n')
end

-- Report richer lifecycle signals to the local personalization store. This is
-- best-effort and never blocks playback; completion markers remain the durable
-- tracker fallback when curl or the daemon is temporarily unavailable.
local function snapshot(kind, reason)
  if not content_id or content_id == '' then return end
  local pos = mp.get_property_number('time-pos') or 0
  local dur = mp.get_property_number('duration') or 0
  sequence = sequence + 1
  return string.format('{"item_id":"%s","content_id":"%s","track_id":"%s","session_id":"%s","sequence":%d,"kind":"%s","position":%.3f,"duration":%.3f,"context":"mpv","reason":"%s","ts":%d}',
    json_escape(content_id), json_escape(content_id), json_escape(track_id), json_escape(session_id),
    sequence, kind, pos, dur, json_escape(reason or kind), os.time())
end

local function report(kind, reason, body)
  body = body or snapshot(kind, reason)
  if not body then return end
  mp.command_native_async({
    name = 'subprocess', playback_only = false,
    args = { 'curl', '--silent', '--max-time', '1', '-X', 'POST',
      '-H', 'Content-Type: application/json', '--data', body,
      'http://127.0.0.1:8484/api/interactions' }
  }, function() end)
end

local function atomic_write(target, value)
  local tmp = target .. '.tmp'
  local file = io.open(tmp, 'w')
  if not file then return end
  file:write(value)
  file:flush()
  file:close()
  os.rename(tmp, target)
end

local function save(seconds, kind, reason, send)
  local file = io.open(path, 'w')
  if file then
    file:write(tostring(seconds))
    file:close()
  end
  local body = snapshot(kind or 'progress', reason)
  if body then atomic_write(path .. '.progress.json', body) end
  if send then report(kind or 'progress', reason, body) end
end

-- A lightweight local marker tells tvosd "this content was actively watched".
-- Unlike the .done marker below, this is written for normal quits too, so the
-- Continue row can move to the latest title even when you stop midway.
local function mark_played()
  if not content_id or content_id == '' then return end
  local file = io.open(path .. '.played', 'w')
  if file then
    file:write(content_id)
    file:close()
  end
end

-- Persist roughly every 5s during playback.
local ticks = 0
mp.add_periodic_timer(5, function()
  local t = mp.get_property_number('time-pos')
  if t and t > 0 then
    ticks = ticks + 1
    save(t, 'progress', 'timer', ticks % 2 == 0)
    mark_played()
  end
end)

-- A completion marker next to the position file tells the daemon this title
-- was actually watched (natural end, or quit in the last 10%) — that's what
-- the Trakt/AniList/MAL scrobbler picks up.
local function mark_watched()
  local id = os.getenv('TVOS_ITEM_ID')
  if not id or id == '' then return end
  local file = io.open(path .. '.done', 'w')
  if file then
    file:write(id)
    file:close()
  end
end

-- On stop/quit save where we are; on natural end clear it (watched).
mp.register_event('end-file', function(event)
  local percent = mp.get_property_number('percent-pos') or 0
  if event.reason == 'eof' then
    save(0, 'complete', 'eof', true)
    mark_played()
    mark_watched()
  else
    local t = mp.get_property_number('time-pos')
    if t and t > 0 then
      save(t, percent >= 95 and 'complete' or 'abandon', event.reason or 'stop', true)
      mark_played()
    end
    if percent >= 95 then mark_watched() end
  end
end)

mp.register_event('file-loaded', function() save(mp.get_property_number('time-pos') or 0, 'play', 'file_loaded', true) end)
mp.register_event('seek', function() save(mp.get_property_number('time-pos') or 0, 'progress', 'seek', true) end)
mp.observe_property('pause', 'bool', function(_, paused)
  if paused ~= nil then save(mp.get_property_number('time-pos') or 0, paused and 'pause' or 'progress', paused and 'pause' or 'resume', true) end
end)
mp.register_script_message('tvos-progress-next', function()
  save(mp.get_property_number('time-pos') or 0, 'complete', 'next_episode', true)
  mark_watched()
end)
