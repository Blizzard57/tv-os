-- TV OS — auto-skip intros, recaps and outros.
--
-- Many releases (especially anime and TV) carry chapter markers like "Intro",
-- "Opening"/"OP", "Ending"/"ED", "Outro", "Credits", "Recap" or "Preview".
-- When playback enters one of those chapters we jump to the next chapter.
-- Edit the patterns below to taste (tvosd/player/scripts/skip.lua).

local mp = require 'mp'

local SKIP = {
  'intro', 'opening', '^op$', 'op ',
  'ending', '^ed$', 'ed ', 'outro',
  'credits', 'recap', 'preview', 'next episode',
}

local function skippable(name)
  if not name then return false end
  local n = name:lower()
  for _, pat in ipairs(SKIP) do
    if n:find(pat) then return true end
  end
  return false
end

local function on_chapter(_, idx)
  if idx == nil or idx < 0 then return end
  local list = mp.get_property_native('chapter-list') or {}
  local current = list[idx + 1] -- mpv's chapter index is 0-based
  local next_chapter = list[idx + 2]
  -- Only skip when there's somewhere to skip *to* (a following chapter), so we
  -- don't cut off the end of a movie.
  if current and next_chapter and skippable(current.title) then
    mp.set_property_number('time-pos', next_chapter.time)
    mp.osd_message('Skipped ' .. (current.title or 'section'), 1.5)
  end
end

mp.observe_property('chapter', 'number', on_chapter)
