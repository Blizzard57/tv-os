-- TV OS — saves the playback position so "Continue" can resume.
--
-- tvosd passes the per-item position file as $TVOS_POSITION_FILE and resumes by
-- writing `start=` into mpv.conf on the next launch. Here we just keep that file
-- updated, and zero it when the title finishes so a watched item doesn't resume.

local mp = require 'mp'

local path = os.getenv('TVOS_POSITION_FILE')
if not path or path == '' then return end

local content_id = os.getenv('TVOS_CONTENT_ID')
if not content_id or content_id == '' then content_id = os.getenv('TVOS_ITEM_ID') end

local function save(seconds)
  local file = io.open(path, 'w')
  if file then
    file:write(tostring(seconds))
    file:close()
  end
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
mp.add_periodic_timer(5, function()
  local t = mp.get_property_number('time-pos')
  if t and t > 0 then
    save(t)
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
    save(0)
    mark_played()
    mark_watched()
  else
    local t = mp.get_property_number('time-pos')
    if t and t > 0 then
      save(t)
      mark_played()
    end
    if percent >= 90 then mark_watched() end
  end
end)
