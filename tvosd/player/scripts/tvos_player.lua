-- TV OS player overlay — a D-pad-first, Google TV-style mpv UI.
--
-- Replaces the desktop/mouse OSC for TV playback. Draws a top title block, a
-- bottom scrim, a scrubber with a live thumbnail preview, and a single Google-TV
-- control bar: centered transport (replay-10 · play/pause · forward-10) with a
-- right-aligned cluster of icon buttons (captions, audio, speed, Enhance). Full
-- sheets slide over everything for track/speed/Enhance selection. Keyboard,
-- HDMI-CEC remotes, and controllers all arrive as the same keys.
--
-- Everything is drawn as vector shapes so the icons are crisp at any 10-foot
-- scale and never depend on a glyph font. Controls cross-fade in and out.

local mp = require 'mp'
local utils = require 'mp.utils'
local assdraw = require 'mp.assdraw'

-- Palette mirrors shell/src/styles.css (Google-TV dark). ASS wants colours in
-- BGR hex, so these are the shell's RGB byte-reversed.
local COLORS = {
  bg = '130E0D',       -- #0D0E13  near-black
  surface = '231B19',  -- #191B23  card
  surface2 = '3C312D', -- #2D313C  track
  text = 'FAF5F4',     -- #F4F5FA  off-white
  dim = 'B0A09A',      -- #9AA0B0  muted
  accent = 'F8B48A',   -- #8AB4F8  blue accent
  white = 'FFFFFF',
  black = '0A0806',    -- glyph colour on a focused (white) chip
  live = '2A34E5',     -- #E5342A  live red
}

local LANG = {
  aa='Afar', ab='Abkhazian', af='Afrikaans', ak='Akan', am='Amharic', ar='Arabic',
  as='Assamese', ay='Aymara', az='Azerbaijani', ba='Bashkir', be='Belarusian',
  bg='Bulgarian', bh='Bihari', bi='Bislama', bn='Bengali', bo='Tibetan',
  br='Breton', bs='Bosnian', ca='Catalan', ce='Chechen', co='Corsican',
  cs='Czech', cy='Welsh', da='Danish', de='German', dz='Dzongkha', el='Greek',
  en='English', eo='Esperanto', es='Spanish', et='Estonian', eu='Basque',
  fa='Persian', fi='Finnish', fj='Fijian', fo='Faroese', fr='French',
  fy='Frisian', ga='Irish', gd='Scottish Gaelic', gl='Galician', gn='Guarani',
  gu='Gujarati', ha='Hausa', he='Hebrew', hi='Hindi', hr='Croatian',
  ht='Haitian Creole', hu='Hungarian', hy='Armenian', id='Indonesian',
  is='Icelandic', it='Italian', ja='Japanese', jv='Javanese', ka='Georgian',
  kk='Kazakh', km='Khmer', kn='Kannada', ko='Korean', ku='Kurdish',
  ky='Kyrgyz', la='Latin', lb='Luxembourgish', ln='Lingala', lo='Lao',
  lt='Lithuanian', lv='Latvian', mg='Malagasy', mi='Maori', mk='Macedonian',
  ml='Malayalam', mn='Mongolian', mr='Marathi', ms='Malay', mt='Maltese',
  my='Burmese', ne='Nepali', nl='Dutch', no='Norwegian', oc='Occitan',
  pa='Punjabi', pl='Polish', ps='Pashto', pt='Portuguese', ro='Romanian',
  ru='Russian', sa='Sanskrit', sd='Sindhi', si='Sinhala', sk='Slovak',
  sl='Slovenian', sm='Samoan', sn='Shona', so='Somali', sq='Albanian',
  sr='Serbian', su='Sundanese', sv='Swedish', sw='Swahili', ta='Tamil',
  te='Telugu', tg='Tajik', th='Thai', tk='Turkmen', tl='Tagalog',
  tr='Turkish', tt='Tatar', uk='Ukrainian', ur='Urdu', uz='Uzbek',
  vi='Vietnamese', wo='Wolof', xh='Xhosa', yi='Yiddish', yo='Yoruba',
  zh='Chinese', zu='Zulu',
  eng='English', jpn='Japanese', jap='Japanese', spa='Spanish', esl='Spanish',
  fre='French', fra='French', ger='German', deu='German', ita='Italian',
  por='Portuguese', rus='Russian', kor='Korean', chi='Chinese', zho='Chinese',
  ara='Arabic', hin='Hindi', dut='Dutch', nld='Dutch', swe='Swedish',
  nor='Norwegian', dan='Danish', fin='Finnish', pol='Polish', tur='Turkish',
  gre='Greek', ell='Greek', heb='Hebrew', rum='Romanian', ron='Romanian',
  cze='Czech', ces='Czech', hun='Hungarian', ukr='Ukrainian', tha='Thai',
  ind='Indonesian', may='Malay', msa='Malay',
}

-- Control-bar buttons, left to right. Columns 1-3 are the centered transport;
-- 4-7 are the right-aligned secondary cluster. state.col indexes this list.
local BUTTONS = {
  { id = 'rewind',  kind = 'seek', delta = -10 },
  { id = 'play',    kind = 'pause' },
  { id = 'forward', kind = 'seek', delta = 10 },
  { id = 'settings', kind = 'menu', menu = 'settings' },
}
local FIRST_SECONDARY = 4

local state = {
  visible = false,
  opacity = 0,      -- 0..1, animated for cross-fades
  target = 0,
  fade_timer = nil,
  row = 2,          -- 1 = scrubber, 2 = control bar
  col = 2,          -- index into BUTTONS
  menu = nil,
  menu_sel = 1,
  overlay = nil,
  hide_timer = nil,
  layout = nil,
  upnext = nil,
  upnext_sel = 1,
  upnext_timer = nil,
}

local redraw
local thumb_info = nil
local thumb_shown = false

-- ---------------------------------------------------------------------------
-- small helpers
-- ---------------------------------------------------------------------------

local function esc(s)
  return mp.command_native({ 'escape-ass', tostring(s or '') })
end

-- ASS alpha runs 00 (opaque) .. FF (transparent). Blend the requested base
-- alpha toward fully transparent by the current fade opacity.
local function fa(alpha)
  alpha = alpha or 0
  return math.floor(0xFF - (0xFF - alpha) * state.opacity + 0.5)
end

local function fill(hex, alpha)
  return string.format('\\1c&H%s&\\1a&H%02X&', hex, fa(alpha))
end

local function fmt_time(sec)
  sec = math.max(0, math.floor(tonumber(sec) or 0))
  local h = math.floor(sec / 3600)
  local m = math.floor((sec % 3600) / 60)
  local s = sec % 60
  if h > 0 then return string.format('%d:%02d:%02d', h, m, s) end
  return string.format('%d:%02d', m, s)
end

-- ---------------------------------------------------------------------------
-- metadata
-- ---------------------------------------------------------------------------

local function read_meta()
  local path = mp.find_config_file('player-meta.json')
  if not path then return {} end
  local f = io.open(path, 'r')
  if not f then return {} end
  local text = f:read('*a')
  f:close()
  return utils.parse_json(text or '') or {}
end

local player_meta = read_meta()
local playback_pref = player_meta.playback_preference or {
  scope_key = player_meta.preference_scope, provider = player_meta.preference_provider
}
local sponsor_undo_pos = nil

local function persist_preference(fields)
  if not player_meta.preference_scope or not player_meta.preference_provider then return end
  playback_pref.scope_key = player_meta.preference_scope
  playback_pref.provider = player_meta.preference_provider
  for key, value in pairs(fields) do
    if value == false then playback_pref[key] = nil else playback_pref[key] = value end
  end
  local body = utils.format_json(playback_pref)
  if not body then return end
  mp.command_native_async({ name = 'subprocess', playback_only = false,
    args = { 'curl', '--silent', '--max-time', '1', '-X', 'PUT', '-H', 'Content-Type: application/json',
      '--data', body, 'http://127.0.0.1:8484/api/player/preferences' } }, function() end)
end
if player_meta.next_episode_id and player_meta.next_episode_id ~= '' then
  table.insert(BUTTONS, 4, { id = 'next', kind = 'next' })
end
-- A live stream (a sports channel, a YouTube live broadcast): no duration to
-- scrub, so the overlay shows a "● LIVE" marker instead of a seek bar.
local is_live = player_meta.live == true

local function title()
  return player_meta.title or mp.get_property('media-title') or mp.get_property('filename') or 'TV OS'
end

local function subtitle_line()
  if player_meta.subtitle and player_meta.subtitle ~= '' then return player_meta.subtitle end
  if player_meta.source and player_meta.source ~= '' then return player_meta.source end
  return ''
end

-- ---------------------------------------------------------------------------
-- track / menu data
-- ---------------------------------------------------------------------------

local function normalize_lang(raw)
  if not raw or raw == '' then return nil end
  local v = tostring(raw):lower():gsub('_', '-')
  v = v:gsub('^([a-z][a-z][a-z])%-.*$', '%1')
  v = v:gsub('^([a-z][a-z])%-.*$', '%1')
  if LANG[v] then return LANG[v] end
  if #v > 3 then
    local first = v:match('([a-z][a-z][a-z]?)')
    if first and LANG[first] then return LANG[first] end
  end
  return nil
end

local function clean_title(raw)
  if not raw or raw == '' then return nil end
  local s = tostring(raw):gsub('[_%.]+', ' '):gsub('%s+', ' '):gsub('^%s+', ''):gsub('%s+$', '')
  if s == '' or s:match('^%d+$') or s:match('^[a-zA-Z][a-zA-Z]%d?$') then return nil end
  return s
end

local function track_flags(track)
  local text = ((track.title or '') .. ' ' .. (track.lang or '')):lower()
  local out = {}
  if track.forced or text:find('forced', 1, true) then out[#out + 1] = 'Forced' end
  if text:find('sdh', 1, true) or text:find('hearing', 1, true) then out[#out + 1] = 'SDH' end
  if text:find('commentary', 1, true) then out[#out + 1] = 'Commentary' end
  if track['demux-channel-count'] then out[#out + 1] = tostring(track['demux-channel-count']) .. 'ch' end
  return table.concat(out, ' · ')
end

local function track_label(track, idx)
  local lang = normalize_lang(track.lang)
  local name = clean_title(track.title)
  local flags = track_flags(track)
  local base = lang or name or ('Unknown #' .. tostring(idx))
  if name and lang and not name:lower():find(lang:lower(), 1, true) then base = base .. ' · ' .. name end
  if flags ~= '' and not base:lower():find(flags:lower(), 1, true) then base = base .. ' · ' .. flags end
  return base
end

local function tracks(kind)
  local out = {}
  for _, t in ipairs(mp.get_property_native('track-list') or {}) do
    if t.type == kind then out[#out + 1] = t end
  end
  return out
end

local function active_track(kind)
  local prop = kind == 'audio' and 'aid' or 'sid'
  return mp.get_property_native(prop)
end

local function apply_track(kind, entry)
  local prop = kind == 'audio' and 'aid' or 'sid'
  mp.set_property_native(prop, entry and entry.id or false)
  if kind == 'sub' then
    persist_preference(entry and { subtitle_mode = 'language', subtitle_language = entry.lang } or
      { subtitle_mode = 'off', subtitle_language = false })
  elseif entry then
    persist_preference({ audio_language = entry.lang })
  end
end

local function load_upscalers()
  local path = mp.find_config_file('upscalers.json')
  if not path then return { presets = {} } end
  local f = io.open(path, 'r')
  if not f then return { presets = {} } end
  local text = f:read('*a')
  f:close()
  return utils.parse_json(text or '') or { presets = {} }
end

local function current_shader_string()
  return table.concat(mp.get_property_native('glsl-shaders') or {}, ':')
end

local function apply_upscaler(preset)
  local list = {}
  if preset.shaders and preset.shaders ~= '' then
    for s in string.gmatch(preset.shaders, '([^:]+)') do list[#list + 1] = s end
  end
  mp.set_property_native('glsl-shaders', list)
  mp.osd_message('Enhance: ' .. preset.name, 1.5)
  persist_preference({ enhance_preset = preset.name })
end

local open_menu
local function rows_for_menu(kind)
  local rows = {}
  if kind == 'settings' then
    rows = {
      { label = 'Captions', sub = active_track('sub') and 'On' or 'Off', apply = function() open_menu('subs') end },
      { label = 'Audio', sub = track_label(tracks('audio')[1] or {}, 1), apply = function() open_menu('audio') end },
      { label = 'Playback speed', sub = tostring(mp.get_property_number('speed') or 1) .. '×', apply = function() open_menu('speed') end },
      { label = 'Stream quality', sub = player_meta.quality or 'Auto', apply = function() open_menu('quality') end },
      { label = 'Enhance', sub = ((load_upscalers().capability or {}).backend or load_upscalers().active or 'Off'), apply = function() open_menu('enhance') end },
    }
    if sponsor_undo_pos then
      rows[#rows + 1] = { label = 'Undo SponsorBlock skip', sub = 'Return to skipped segment', apply = function()
        mp.set_property_number('time-pos', sponsor_undo_pos); sponsor_undo_pos = nil
      end }
    end
  elseif kind == 'subs' then
    rows[#rows + 1] = { label = 'Off', selected = not active_track('sub'), apply = function() apply_track('sub', nil) end }
    rows[#rows + 1] = { label = 'Auto', selected = playback_pref.subtitle_mode == 'auto', apply = function()
      mp.set_property_native('sid', 'auto')
      persist_preference({ subtitle_mode = 'auto', subtitle_language = false })
    end }
    for i, t in ipairs(tracks('sub')) do
      rows[#rows + 1] = {
        label = track_label(t, i),
        selected = active_track('sub') == t.id,
        apply = function() apply_track('sub', t) end,
      }
    end
  elseif kind == 'audio' then
    rows[#rows + 1] = { label = 'Auto', selected = playback_pref.audio_language == nil, apply = function()
      mp.set_property_native('aid', 'auto')
      persist_preference({ audio_language = false })
    end }
    for i, t in ipairs(tracks('audio')) do
      rows[#rows + 1] = {
        label = track_label(t, i),
        selected = active_track('audio') == t.id,
        apply = function() apply_track('audio', t) end,
      }
    end
  elseif kind == 'speed' then
    for _, speed in ipairs({ 0.75, 1.0, 1.25, 1.5, 2.0 }) do
      rows[#rows + 1] = {
        label = speed == 1.0 and 'Normal' or (tostring(speed) .. '×'),
        selected = math.abs((mp.get_property_number('speed') or 1) - speed) < 0.01,
        apply = function() mp.set_property_number('speed', speed); persist_preference({ speed = speed }) end,
      }
    end
  elseif kind == 'quality' then
    for _, quality in ipairs({ 'Auto', '4K', '1080p', '720p', '480p' }) do
      rows[#rows + 1] = {
        label = quality,
        selected = (player_meta.quality or 'Auto'):lower() == quality:lower(),
        apply = function()
          local body = utils.format_json({
            content_id = player_meta.content_id or os.getenv('TVOS_CONTENT_ID') or '',
            track_id = player_meta.track_id or os.getenv('TVOS_ITEM_ID') or '',
            quality = quality,
            position = mp.get_property_number('time-pos') or 0,
            duration = mp.get_property_number('duration'),
            title = title(),
            art = player_meta.art,
          })
          mp.osd_message('Switching to ' .. quality .. '…', 2)
          mp.command_native_async({ name = 'subprocess', playback_only = false,
            args = { 'curl', '--silent', '--show-error', '--max-time', '20', '-X', 'POST',
              '-H', 'Content-Type: application/json', '--data', body,
              'http://127.0.0.1:8484/api/player/quality' } }, function(success)
                if not success then mp.osd_message('That quality is unavailable', 3) end
              end)
        end,
      }
    end
  elseif kind == 'enhance' then
    local live = current_shader_string()
    for _, p in ipairs(load_upscalers().presets or {}) do
      rows[#rows + 1] = {
        label = p.name,
        sub = p.hint,
        selected = p.shaders == live,
        apply = function() apply_upscaler(p) end,
      }
    end
  end
  return rows
end

-- ---------------------------------------------------------------------------
-- visibility / fade
-- ---------------------------------------------------------------------------

local function hide_thumb()
  if thumb_shown then
    mp.commandv('script-message-to', 'thumbfast', 'clear')
    thumb_shown = false
  end
end

local function clear_overlay()
  if state.overlay then state.overlay.data = ''; state.overlay:update() end
end

local function tick_fade()
  local step = 1 / (0.14 * 60) -- reach target in ~140ms
  if state.opacity < state.target then
    state.opacity = math.min(state.target, state.opacity + step)
  elseif state.opacity > state.target then
    state.opacity = math.max(state.target, state.opacity - step)
  end
  redraw()
  if state.opacity == state.target then
    if state.fade_timer then state.fade_timer:kill(); state.fade_timer = nil end
    if state.opacity == 0 then
      state.visible = false
      hide_thumb()
      clear_overlay()
    end
  end
end

local function start_fade()
  if not state.fade_timer then
    state.fade_timer = mp.add_periodic_timer(1 / 60, tick_fade)
  end
end

local function schedule_hide()
  if state.hide_timer then state.hide_timer:kill(); state.hide_timer = nil end
  if mp.get_property_native('pause') or state.menu or state.upnext then return end
  state.hide_timer = mp.add_timeout(4, function()
    state.target = 0
    start_fade()
  end)
end

local function show()
  state.visible = true
  state.target = 1
  if state.opacity < 1 then start_fade() end
  schedule_hide()
end

open_menu = function(kind)
  local rows = rows_for_menu(kind)
  if #rows == 0 then
    mp.osd_message(kind == 'audio' and 'No audio tracks' or kind == 'subs' and 'No subtitles' or 'Nothing to choose', 2)
    return
  end
  local parent = state.menu and state.menu.kind == 'settings' and state.menu or nil
  state.menu = { kind = kind, rows = rows, parent = parent }
  state.menu_sel = 1
  for i, row in ipairs(rows) do
    if row.selected then state.menu_sel = i end
  end
  hide_thumb()
  show()
end

local function close_menu()
  state.menu = state.menu and state.menu.parent or nil
  schedule_hide()
end

-- ---------------------------------------------------------------------------
-- navigation
-- ---------------------------------------------------------------------------

local function move_focus(dir)
  if dir == 'up' then
    state.row = is_live and 2 or 1 -- live has no scrubber row
  elseif dir == 'down' then
    state.row = 2
  elseif state.row == 2 then
    if dir == 'left' then state.col = math.max(1, state.col - 1)
    elseif dir == 'right' then state.col = math.min(#BUTTONS, state.col + 1) end
  end
end

local function activate()
  if state.row == 1 then
    mp.commandv('cycle', 'pause')
    return
  end
  local b = BUTTONS[state.col]
  if b.kind == 'seek' then
    mp.commandv('seek', tostring(b.delta), 'relative')
  elseif b.kind == 'pause' then
    mp.commandv('cycle', 'pause')
  elseif b.kind == 'menu' then
    open_menu(b.menu)
  elseif b.kind == 'next' then
    mp.commandv('script-message', 'tvos-progress-next')
    local content = os.getenv('TVOS_CONTENT_ID') or ''
    local function json(s) return tostring(s or ''):gsub('\\', '\\\\'):gsub('"', '\\"') end
    local body = string.format('{"content_id":"%s","track_id":"%s","title":"%s"}',
      json(content), json(player_meta.next_episode_id), json(title()))
    mp.command_native_async({ name = 'subprocess', playback_only = false,
      args = { 'curl', '--silent', '--max-time', '8', '-X', 'POST', '-H', 'Content-Type: application/json',
        '--data', body, 'http://127.0.0.1:8484/api/player/next' } }, function() end)
    mp.osd_message('Starting next episode…', 2)
  elseif b.kind == 'command' then
    mp.command(b.command)
    mp.osd_message(b.id == 'skip_intro' and 'Skipped chapter' or 'Next episode', 1.2)
  end
end

-- ---------------------------------------------------------------------------
-- drawing primitives (all coordinates absolute; \pos(0,0) origin)
-- ---------------------------------------------------------------------------

local DRAW_HEAD = '{\\an7\\pos(0,0)\\bord0\\shad0'

local function shape(a, hex, alpha, path)
  a:new_event()
  a:append(DRAW_HEAD .. fill(hex, alpha) .. '\\p1}' .. path)
end

local function rrect(a, hex, alpha, x0, y0, x1, y1, r)
  a:new_event()
  a:append(DRAW_HEAD .. fill(hex, alpha) .. '\\p1}')
  a:draw_start()
  if r and r > 0 then a:round_rect_cw(x0, y0, x1, y1, r) else a:rect_cw(x0, y0, x1, y1) end
  a:draw_stop()
end

local function disc(a, hex, alpha, cx, cy, r)
  rrect(a, hex, alpha, cx - r, cy - r, cx + r, cy + r, r)
end

local function text(a, x, y, an, size, hex, alpha, bold, str)
  a:new_event()
  a:append(string.format('{\\fnRoboto\\an%d\\pos(%d,%d)\\fs%d\\b%d\\bord0\\shad0%s}%s',
    an, x, y, size, bold and 1 or 0, fill(hex, alpha), esc(str)))
end

-- Filled circular-arc sector (annulus slice) as a raw ASS path, angles in
-- degrees on a y-down plane (0 = right, 90 = down, 180 = left, 270 = up).
local function arc_sector_path(cx, cy, ro, ri, a0, a1, steps)
  local parts = {}
  for i = 0, steps do
    local t = math.rad(a0 + (a1 - a0) * i / steps)
    parts[#parts + 1] = string.format('%.1f %.1f', cx + ro * math.cos(t), cy + ro * math.sin(t))
  end
  for i = steps, 0, -1 do
    local t = math.rad(a0 + (a1 - a0) * i / steps)
    parts[#parts + 1] = string.format('%.1f %.1f', cx + ri * math.cos(t), cy + ri * math.sin(t))
  end
  return 'm ' .. parts[1] .. ' l ' .. table.concat(parts, ' ', 2)
end

-- ---------------------------------------------------------------------------
-- icons — drawn centered at (cx,cy) within glyph radius s
-- ---------------------------------------------------------------------------

local function icon_play(a, cx, cy, s, hex, alpha)
  local x0 = cx - s * 0.46
  local xr = cx + s * 0.66
  local yh = s * 0.78
  shape(a, hex, alpha, string.format('m %.1f %.1f l %.1f %.1f l %.1f %.1f',
    x0, cy - yh, xr, cy, x0, cy + yh))
end

local function icon_pause(a, cx, cy, s, hex, alpha)
  local bw = s * 0.34
  local bh = s * 1.5
  local gap = s * 0.28
  local r = bw * 0.4
  rrect(a, hex, alpha, cx - gap - bw, cy - bh / 2, cx - gap, cy + bh / 2, r)
  rrect(a, hex, alpha, cx + gap, cy - bh / 2, cx + gap + bw, cy + bh / 2, r)
end

-- Circular arrow with "10", Google-TV replay/forward style. dir -1 = back.
local function icon_skip(a, cx, cy, s, hex, alpha, dir)
  local ro, ri = s * 0.98, s * 0.66
  -- Arc covers everything but a ~70° gap at the top; the arrowhead sits at the
  -- gap end in the travel direction.
  if dir > 0 then
    shape(a, hex, alpha, arc_sector_path(cx, cy, ro, ri, 302, 578, 44)) -- 302 -> 218 (+360)
  else
    shape(a, hex, alpha, arc_sector_path(cx, cy, ro, ri, 238, -38, 44)) -- 238 -> 322 backwards
  end
  local ea = dir > 0 and 302 or 238
  local r = math.rad(ea)
  local mid = (ro + ri) / 2
  local px, py = cx + mid * math.cos(r), cy + mid * math.sin(r)
  -- tangent (travel dir) and radial
  local tx, ty = -math.sin(r) * dir, math.cos(r) * dir
  local nx, ny = math.cos(r), math.sin(r)
  local hl, hw = s * 0.5, s * 0.42
  shape(a, hex, alpha, string.format('m %.1f %.1f l %.1f %.1f l %.1f %.1f',
    px + tx * hl, py + ty * hl,
    px - tx * hl * 0.2 + nx * hw, py - ty * hl * 0.2 + ny * hw,
    px - tx * hl * 0.2 - nx * hw, py - ty * hl * 0.2 - ny * hw))
  text(a, cx, cy + s * 0.06, 5, math.floor(s * 0.64), hex, alpha, true, '10')
end

-- Captions: rounded box outline with two subtitle bars inside.
local function icon_cc(a, cx, cy, s, hex, alpha)
  local w, h = s * 1.9, s * 1.34
  a:new_event()
  a:append(DRAW_HEAD .. fill(hex, alpha) .. '\\p1}')
  a:draw_start()
  a:round_rect_cw(cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2, s * 0.34)
  a:round_rect_ccw(cx - w / 2 + s * 0.24, cy - h / 2 + s * 0.24, cx + w / 2 - s * 0.24, cy + h / 2 - s * 0.24, s * 0.2)
  a:draw_stop()
  local by = cy + s * 0.34
  rrect(a, hex, alpha, cx - s * 0.5, by - s * 0.12, cx - s * 0.02, by + s * 0.12, s * 0.12)
  rrect(a, hex, alpha, cx + s * 0.12, by - s * 0.12, cx + s * 0.5, by + s * 0.12, s * 0.12)
end

-- Audio: three slider bars with knobs (the "tune" glyph).
local function icon_audio(a, cx, cy, s, hex, alpha)
  local xs = { cx - s * 0.62, cx, cx + s * 0.62 }
  local knob = { -s * 0.35, s * 0.3, -s * 0.05 }
  local bw = s * 0.14
  for i = 1, 3 do
    rrect(a, hex, alpha, xs[i] - bw, cy - s * 0.9, xs[i] + bw, cy + s * 0.9, bw)
    disc(a, hex, alpha, xs[i], cy + knob[i], s * 0.26)
  end
end

-- Enhance: a four-point sparkle plus a small twinkle.
local function star(a, cx, cy, r, hex, alpha)
  local outer, inner = r, r * 0.32
  local pts = {}
  for i = 0, 7 do
    local ang = math.rad(i * 45 - 90)
    local rad = (i % 2 == 0) and outer or inner
    pts[#pts + 1] = string.format('%.1f %.1f', cx + rad * math.cos(ang), cy + rad * math.sin(ang))
  end
  shape(a, hex, alpha, 'm ' .. pts[1] .. ' l ' .. table.concat(pts, ' ', 2))
end

local function icon_enhance(a, cx, cy, s, hex, alpha)
  star(a, cx - s * 0.18, cy + s * 0.12, s * 0.92, hex, alpha)
  star(a, cx + s * 0.66, cy - s * 0.66, s * 0.4, hex, alpha)
end

local ICONS = {
  rewind = function(a, cx, cy, s, hex, al) icon_skip(a, cx, cy, s, hex, al, -1) end,
  forward = function(a, cx, cy, s, hex, al) icon_skip(a, cx, cy, s, hex, al, 1) end,
  play = function(a, cx, cy, s, hex, al) icon_play(a, cx, cy, s, hex, al) end,
  pause = function(a, cx, cy, s, hex, al) icon_pause(a, cx, cy, s, hex, al) end,
  subs = icon_cc,
  audio = icon_audio,
  enhance = icon_enhance,
  settings = icon_enhance,
}

-- ---------------------------------------------------------------------------
-- layout
-- ---------------------------------------------------------------------------

local function layout(w, h)
  local scale = h / 1080
  local function px(v) return math.floor(v * scale + 0.5) end
  local edge_x = px(80)
  local edge_y = px(54)

  local L = { scale = scale, w = w, h = h, edge_x = edge_x, edge_y = edge_y, buttons = {} }

  -- control bar (icon centers share this y; leave room for the play focus ring)
  L.by = h - edge_y - px(54)
  -- scrubber
  L.sx0 = edge_x
  L.sx1 = w - edge_x
  L.sbar_w = L.sx1 - L.sx0
  L.sth = math.max(4, px(6))
  L.sy = L.by - px(118)
  L.time_y = L.sy + px(26)
  L.time_fs = px(26)
  -- title
  -- The Back affordance owns its own toolbar line. Keeping the title below it
  -- prevents long logos/source labels from colliding with navigation.
  L.back_y = edge_y + px(20)
  L.title_y = edge_y + px(92)
  L.title_fs = px(38)
  L.sub_y = L.title_y + px(50)
  L.sub_fs = px(24)

  local play_ring, skip_ring, sec_ring = px(48), px(40), px(32)
  local cx = math.floor(w / 2)
  local toff = px(148)
  local geo = {
    rewind  = { x = cx - toff, ring = skip_ring, glyph = px(21) },
    play    = { x = cx,        ring = play_ring, glyph = px(27) },
    forward = { x = cx + toff, ring = skip_ring, glyph = px(21) },
  }
  local spacing = px(80)
  local right = w - edge_x - sec_ring
  local sec_glyph = px(17)
  for i = FIRST_SECONDARY, #BUTTONS do
    local from_right = #BUTTONS - i
    geo[BUTTONS[i].id] = { x = right - from_right * spacing, ring = sec_ring, glyph = sec_glyph }
  end

  for i, b in ipairs(BUTTONS) do
    local g = geo[b.id]
    L.buttons[i] = { id = b.id, x = g.x, y = L.by, ring = g.ring, glyph = g.glyph }
  end
  return L
end

-- ---------------------------------------------------------------------------
-- render
-- ---------------------------------------------------------------------------

-- Vertical black gradient built from stacked bands; alpha_top/alpha_bot are ASS
-- alphas (00 opaque .. FF transparent).
local function scrim(a, w, y0, y1, alpha_top, alpha_bot, steps)
  for i = 0, steps - 1 do
    local yy0 = y0 + (y1 - y0) * i / steps
    local yy1 = y0 + (y1 - y0) * (i + 1) / steps
    local al = math.floor(alpha_top + (alpha_bot - alpha_top) * (i + 0.5) / steps + 0.5)
    rrect(a, '000000', al, 0, math.floor(yy0), w, math.ceil(yy1) + 1, 0)
  end
end

local function render_controls(a, L)
  local w, h = L.w, L.h

  scrim(a, w, 0, math.floor(h * 0.24), 0x8E, 0xFF, 18)
  scrim(a, w, math.floor(h * 0.45), h, 0xFF, 0x1C, 22)

  -- dedicated top toolbar + title block
  rrect(a, COLORS.surface, 0x36, L.edge_x, L.back_y - math.floor(20 * L.scale),
    L.edge_x + math.floor(112 * L.scale), L.back_y + math.floor(27 * L.scale), math.floor(23 * L.scale))
  text(a, L.edge_x + math.floor(22 * L.scale), L.back_y + math.floor(2 * L.scale), 4,
    math.floor(23 * L.scale), COLORS.text, 0, true, '←  Back')
  text(a, L.edge_x, L.title_y, 7, L.title_fs, COLORS.text, 0, true, title())
  local sub = subtitle_line()
  if sub ~= '' then
    text(a, L.edge_x, L.sub_y, 7, L.sub_fs, COLORS.dim, 0, false, sub)
  end

  if is_live then
    -- A live stream can't be scrubbed to a fixed position — show a "● LIVE"
    -- marker where the seek bar would be.
    local dot = math.floor(7 * L.scale)
    disc(a, COLORS.live, 0, L.sx0 + dot, L.sy, dot)
    text(a, L.sx0 + dot * 3, L.sy, 4, math.floor(26 * L.scale), COLORS.text, 0, true, 'LIVE')
  else
    -- scrubber
    local dur = mp.get_property_number('duration') or 0
    local pos = mp.get_property_number('time-pos') or 0
    local progress = dur > 0 and math.max(0, math.min(1, pos / dur)) or 0
    local cache = mp.get_property_number('demuxer-cache-time')
    local half = L.sth / 2
    rrect(a, COLORS.surface2, 0x14, L.sx0, L.sy - half, L.sx1, L.sy + half, half)
    if cache and dur > 0 then
      local cf = math.max(progress, math.min(1, cache / dur))
      rrect(a, COLORS.dim, 0x64, L.sx0, L.sy - half, L.sx0 + L.sbar_w * cf, L.sy + half, half)
    end
    if progress > 0 then
      rrect(a, COLORS.accent, 0, L.sx0, L.sy - half, L.sx0 + L.sbar_w * progress, L.sy + half, half)
    end
    local hx = L.sx0 + L.sbar_w * progress
    local focused_bar = state.row == 1
    if focused_bar then disc(a, COLORS.accent, 0x40, hx, L.sy, math.floor(15 * L.scale)) end
    disc(a, COLORS.white, 0, hx, L.sy, math.floor((focused_bar and 10 or 7) * L.scale))

    text(a, L.sx0, L.time_y, 7, L.time_fs, COLORS.text, 0, false, fmt_time(pos))
    text(a, L.sx1, L.time_y, 9, L.time_fs, COLORS.dim, 0, false, fmt_time(dur))
  end

  -- control bar
  local paused = mp.get_property_native('pause')
  for i, b in ipairs(L.buttons) do
    local focused = state.row == 2 and state.col == i
    if focused then disc(a, COLORS.white, 0, b.x, b.y, b.ring) end
    local glyph_col = focused and COLORS.black or COLORS.text
    if b.id == 'next' then
      text(a, b.x, b.y, 5, math.floor(b.glyph * 0.78), glyph_col, 0, true, 'NEXT')
    elseif b.id == 'speed' then
      -- Show the live speed (like YouTube) rather than a gauge glyph — clearer
      -- at a glance and doubles as the current value.
      local spd = mp.get_property_number('speed') or 1
      local lbl = math.abs(spd - 1) < 0.01 and '1×' or (string.format('%.2g', spd) .. '×')
      text(a, b.x, b.y, 5, math.floor(b.glyph * 1.28), glyph_col, 0, true, lbl)
    else
      local id = b.id
      if id == 'play' then id = paused and 'play' or 'pause' end
      local fn = ICONS[id]
      if fn then fn(a, b.x, b.y, b.glyph, glyph_col, 0) end
    end
  end
end

local function cancel_upnext(quit_after)
  local next = state.upnext
  if state.upnext_timer then state.upnext_timer:kill(); state.upnext_timer = nil end
  state.upnext = nil
  if next and next.token then
    local body = utils.format_json({ token = next.token })
    mp.command_native_async({ name = 'subprocess', playback_only = false,
      args = { 'curl', '--silent', '--max-time', '2', '-X', 'POST', '-H', 'Content-Type: application/json',
        '--data', body, 'http://127.0.0.1:8484/api/player/autoplay/cancel' } }, function() end)
  end
  if quit_after then mp.commandv('quit') else redraw() end
end

local function launch_upnext()
  local next = state.upnext
  if not next or not next.token then return end
  if state.upnext_timer then state.upnext_timer:kill(); state.upnext_timer = nil end
  local body = utils.format_json({ token = next.token })
  next.launching = true
  redraw()
  mp.command_native_async({ name = 'subprocess', playback_only = false,
    args = { 'curl', '--silent', '--show-error', '--max-time', '15', '-X', 'POST',
      '-H', 'Content-Type: application/json', '--data', body,
      'http://127.0.0.1:8484/api/player/autoplay/launch' } }, function(success)
        if success then
          state.upnext = nil
          mp.commandv('quit')
        else
          next.launching = false
          next.failed = true
          next.remaining = 0
          redraw()
        end
      end)
end

local function render_upnext(a, L)
  rrect(a, '000000', 0x3C, 0, 0, L.w, L.h, 0)
  local panel_w = math.floor(math.min(L.w * 0.56, 920 * L.scale))
  local panel_h = math.floor(330 * L.scale)
  local x0 = math.floor((L.w - panel_w) / 2)
  local y0 = math.floor((L.h - panel_h) / 2)
  local pad = math.floor(38 * L.scale)
  rrect(a, COLORS.bg, 0x02, x0, y0, x0 + panel_w, y0 + panel_h, math.floor(28 * L.scale))
  text(a, x0 + pad, y0 + pad, 7, math.floor(22 * L.scale), COLORS.accent, 0, true, 'UP NEXT')
  text(a, x0 + pad, y0 + pad + math.floor(48 * L.scale), 7, math.floor(38 * L.scale), COLORS.text, 0, true,
    state.upnext.title or 'Up next')
  local copy = state.upnext.failed and 'Could not start this video' or
    (state.upnext.launching and 'Starting…' or
      string.format('%s  ·  Playing in %d', state.upnext.reason or 'Recommended for you', state.upnext.remaining or 0))
  text(a, x0 + pad, y0 + pad + math.floor(98 * L.scale), 7, math.floor(24 * L.scale), COLORS.dim, 0, false, copy)
  local by = y0 + panel_h - math.floor(76 * L.scale)
  local bw = math.floor(190 * L.scale)
  local gap = math.floor(22 * L.scale)
  for i, label in ipairs({ state.upnext.failed and 'Close' or 'Play now', 'Cancel' }) do
    local bx = x0 + pad + (i - 1) * (bw + gap)
    local focused = state.upnext_sel == i
    rrect(a, focused and COLORS.white or COLORS.surface2, focused and 0 or 0x20,
      bx, by, bx + bw, by + math.floor(54 * L.scale), math.floor(27 * L.scale))
    text(a, bx + bw / 2, by + math.floor(27 * L.scale), 5, math.floor(23 * L.scale),
      focused and COLORS.black or COLORS.text, 0, true, label)
  end
end

local function request_upnext()
  if is_live or player_meta.autoplay == false or state.upnext then return end
  local body = utils.format_json({
    content_id = player_meta.content_id or os.getenv('TVOS_CONTENT_ID') or '',
    track_id = player_meta.track_id or os.getenv('TVOS_TRACK_ID') or '',
    next_track_id = player_meta.next_episode_id,
    title = title(), art = player_meta.art, domain = player_meta.domain,
  })
  mp.command_native_async({ name = 'subprocess', playback_only = false,
    args = { 'curl', '--silent', '--show-error', '--max-time', '12', '-X', 'POST',
      '-H', 'Content-Type: application/json', '--data', body,
      'http://127.0.0.1:8484/api/player/autoplay/candidate' } }, function(success, result)
        if not success or not result or not result.stdout then return end
        local candidate = utils.parse_json(result.stdout)
        if not candidate or not candidate.token then return end
        candidate.remaining = tonumber(candidate.countdown_seconds) or tonumber(player_meta.autoplay_delay_seconds) or 10
        state.upnext = candidate
        state.upnext_sel = 1
        state.visible = true
        state.opacity = 1
        state.target = 1
        state.upnext_timer = mp.add_periodic_timer(1, function()
          if not state.upnext or state.upnext.launching or state.upnext.failed then return end
          state.upnext.remaining = math.max(0, state.upnext.remaining - 1)
          if state.upnext.remaining <= 0 then launch_upnext() else redraw() end
        end)
        redraw()
      end)
end

local function render_menu(a, L)
  local w, h = L.w, L.h
  rrect(a, '000000', 0x48, 0, 0, w, h, 0)

  local rows = state.menu.rows
  local title_map = { settings = 'Settings', subs = 'Captions', audio = 'Audio', enhance = 'Enhance', speed = 'Playback speed', quality = 'Stream quality' }
  local pw = math.floor(math.min(w * 0.42, 720 * L.scale))
  local px = w - pw - L.edge_x
  local py = L.edge_y + math.floor(20 * L.scale)
  local ph = h - py - L.edge_y - math.floor(20 * L.scale)
  local pad = math.floor(28 * L.scale)
  local row_h = math.floor(62 * L.scale)

  rrect(a, COLORS.bg, 0x06, px, py, px + pw, py + ph, math.floor(22 * L.scale))
  text(a, px + pad, py + pad, 7, math.floor(30 * L.scale), COLORS.text, 0, true,
    title_map[state.menu.kind] or 'Settings')

  local start_y = py + pad + math.floor(52 * L.scale)
  local avail = ph - (start_y - py) - pad
  local max_rows = math.max(1, math.floor(avail / row_h))
  local first = math.max(1, math.min(state.menu_sel - math.floor(max_rows / 2), math.max(1, #rows - max_rows + 1)))
  local last = math.min(#rows, first + max_rows - 1)

  for i = first, last do
    local r = rows[i]
    local y = start_y + (i - first) * row_h
    local focused = i == state.menu_sel
    if focused then
      rrect(a, COLORS.white, 0, px + math.floor(pad / 2), y, px + pw - math.floor(pad / 2), y + row_h - math.floor(8 * L.scale), math.floor(12 * L.scale))
    end
    local label_col = focused and COLORS.black or (r.selected and COLORS.accent or COLORS.text)
    local cy = y + math.floor(row_h * 0.46)
    local marker = r.selected and '✓  ' or ''
    text(a, px + pad, cy, 4, math.floor(26 * L.scale), label_col, 0, focused or r.selected, marker .. r.label)
    if r.sub and r.sub ~= '' then
      text(a, px + pw - pad, cy, 6, math.floor(20 * L.scale), focused and COLORS.black or COLORS.dim, 0, false, r.sub)
    end
  end
end

-- ---------------------------------------------------------------------------
-- thumbnail preview (thumbfast)
-- ---------------------------------------------------------------------------

local function request_thumb()
  if not thumb_info or thumb_info.disabled then hide_thumb(); return end
  if state.menu or not state.visible or state.row ~= 1 then hide_thumb(); return end
  local L = state.layout
  if not L then return end
  local dur = mp.get_property_number('duration') or 0
  if dur <= 0 then hide_thumb(); return end
  local pos = mp.get_property_number('time-pos') or 0
  local tw = thumb_info.width or 0
  local th = thumb_info.height or 0
  if tw == 0 or th == 0 then return end
  local hx = L.sx0 + L.sbar_w * (pos / dur)
  local tx = math.floor(hx - tw / 2)
  tx = math.max(L.edge_x, math.min(L.w - L.edge_x - tw, tx))
  local ty = math.floor(L.sy - th - math.floor(28 * L.scale))
  mp.commandv('script-message-to', 'thumbfast', 'thumb', tostring(pos), tostring(tx), tostring(ty))
  thumb_shown = true
end

-- ---------------------------------------------------------------------------
-- redraw
-- ---------------------------------------------------------------------------

redraw = function()
  local w, h = mp.get_osd_size()
  if not w or w == 0 then return end
  local L = layout(w, h)
  state.layout = L
  local a = assdraw.ass_new()
  if state.upnext then
    render_upnext(a, L)
  elseif state.menu then
    render_menu(a, L)
  elseif state.visible then
    render_controls(a, L)
  end
  if not state.overlay then state.overlay = mp.create_osd_overlay('ass-events') end
  state.overlay.res_x = w
  state.overlay.res_y = h
  state.overlay.data = a.text
  state.overlay:update()
  request_thumb()
end

-- ---------------------------------------------------------------------------
-- input
-- ---------------------------------------------------------------------------

local function on_key(key)
  if state.upnext then
    if key == 'LEFT' then state.upnext_sel = 1
    elseif key == 'RIGHT' then state.upnext_sel = 2
    elseif key == 'ENTER' or key == 'SPACE' then
      if state.upnext_sel == 1 and not state.upnext.failed then launch_upnext() else cancel_upnext(true) end
    elseif key == 'ESC' or key == 'BS' then cancel_upnext(true)
    end
    redraw()
    return
  end
  if state.menu then
    if key == 'UP' then state.menu_sel = math.max(1, state.menu_sel - 1)
    elseif key == 'DOWN' then state.menu_sel = math.min(#state.menu.rows, state.menu_sel + 1)
    elseif key == 'ENTER' or key == 'SPACE' then
      local before = state.menu
      local row = state.menu.rows[state.menu_sel]
      if row and row.apply then row.apply() end
      if state.menu == before then close_menu() end
    elseif key == 'ESC' or key == 'BS' then close_menu()
    end
    show()
    redraw()
    return
  end

  if not state.visible or state.opacity == 0 then
    if key == 'LEFT' then mp.commandv('seek', '-10', 'relative')
    elseif key == 'RIGHT' then mp.commandv('seek', '10', 'relative')
    elseif key == 'ESC' or key == 'BS' then mp.commandv('quit')
    else show() end
    redraw()
    return
  end

  if key == 'LEFT' and state.row == 1 then mp.commandv('seek', '-10', 'relative')
  elseif key == 'RIGHT' and state.row == 1 then mp.commandv('seek', '10', 'relative')
  elseif key == 'LEFT' or key == 'RIGHT' or key == 'UP' or key == 'DOWN' then move_focus(key:lower())
  elseif key == 'ENTER' or key == 'SPACE' then activate()
  elseif key == 'ESC' or key == 'BS' then state.target = 0; start_fade()
  end
  show()
  redraw()
end

local keys = { 'LEFT', 'RIGHT', 'UP', 'DOWN', 'ENTER', 'SPACE', 'ESC', 'BS' }
for _, key in ipairs(keys) do
  mp.add_forced_key_binding(key, 'tvos-player-' .. key, function() on_key(key) end, { repeatable = true })
end

mp.add_forced_key_binding('c', 'tvos-player-subs', function() open_menu('subs'); redraw() end)
mp.add_forced_key_binding('v', 'tvos-player-audio', function() open_menu('audio'); redraw() end)
mp.add_forced_key_binding('u', 'tvos-player-enhance', function() open_menu('enhance'); redraw() end)
mp.add_forced_key_binding('e', 'tvos-player-enhance-toggle', function() mp.commandv('script-binding', 'upscaler/tvos-enhance-toggle'); show(); redraw() end)
mp.add_forced_key_binding('n', 'tvos-player-enhance-next', function() mp.commandv('script-binding', 'upscaler/tvos-upscaler-next'); show(); redraw() end)
mp.add_forced_key_binding('j', 'tvos-player-subvis', function() mp.commandv('cycle', 'sub-visibility'); show(); redraw() end)
mp.add_forced_key_binding('TAB', 'tvos-player-toggle', function()
  if state.visible and state.opacity > 0 then state.target = 0; start_fade() else show() end
  redraw()
end)
mp.add_forced_key_binding('m', 'tvos-player-show', function() show(); redraw() end)

mp.register_script_message('thumbfast-info', function(json)
  thumb_info = utils.parse_json(json) or thumb_info
end)

mp.observe_property('pause', 'bool', function() show(); redraw() end)
mp.observe_property('time-pos', 'number', function() if state.visible then redraw() end end)
mp.observe_property('duration', 'number', function() if state.visible then redraw() end end)
mp.observe_property('track-list', 'native', function() if state.visible then redraw() end end)
mp.observe_property('sid', 'native', function() if state.visible then redraw() end end)
mp.observe_property('aid', 'native', function() if state.visible then redraw() end end)
mp.observe_property('osd-dimensions', 'native', redraw)

-- SponsorBlock segments are fetched by the daemon using the privacy-preserving
-- hash-prefix endpoint. A skipped segment can be restored from Settings.
local sponsor_seen = {}
mp.add_periodic_timer(0.25, function()
  if is_live then return end
  local pos = mp.get_property_number('time-pos')
  if not pos then return end
  for _, segment in ipairs(player_meta.sponsorblock_segments or {}) do
    local key = tostring(segment.start) .. ':' .. tostring(segment['end'])
    if not sponsor_seen[key] and pos >= segment.start and pos < segment['end'] - 0.1 then
      sponsor_seen[key] = true
      sponsor_undo_pos = math.max(0, segment.start)
      mp.set_property_number('time-pos', segment['end'])
      mp.osd_message('Sponsor skipped · Settings to undo', 3)
      break
    end
  end
end)

mp.register_event('file-loaded', function()
  show()
  mp.add_timeout(1.0, redraw)
end)

mp.register_event('end-file', function(event)
  if event and event.reason == 'eof' then request_upnext() end
end)
