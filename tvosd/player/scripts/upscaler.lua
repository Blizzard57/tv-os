-- TV OS — realtime upscaler switching, with a self-contained, controller- and
-- remote-navigable on-screen menu (drawn with ASS, styled like the TV OS
-- shell). Edit freely: tvosd/player/scripts/upscaler.lua.
--
-- The daemon writes the available presets for this playback to
-- ~~/upscalers.json: { active, presets:[{name, hint, shaders}] } where `shaders`
-- is a ':'-joined list of .glsl paths ("" = off). Switching is live — mpv
-- rebuilds the GLSL chain on the fly, no reload.
--
--   u / controller Y → open the menu (↑↓ to move, OK to apply)
--   n                → cycle to the next upscaler
--   e / controller X → quick A/B (current ⇄ off)
--
-- PLAN §5 "live GPU budget": if the renderer keeps dropping frames while a
-- chain is active, we automatically step down (Quality → Fast → Off) and say
-- so, instead of letting playback stutter.

local mp = require 'mp'
local utils = require 'mp.utils'
local assdraw = require 'mp.assdraw'

-- TV OS palette (ASS colors are &HBBGGRR& — see shell/src/styles.css).
local ACCENT = 'F65C8B'  -- #8B5CF6 (violet)
local BG = '120A0B'      -- #0B0A12
local SURFACE = '1F1517' -- #17151F
local TEXT = 'F8F3F4'    -- #F4F3F8
local DIM = 'A68F92'     -- #928FA6

local function load_config()
  local path = mp.find_config_file('upscalers.json')
  if not path then return nil end
  local file = io.open(path, 'r')
  if not file then return nil end
  local text = file:read('*a')
  file:close()
  return utils.parse_json(text or '')
end

local config = load_config() or { active = 'Off', presets = {} }

-- "/a.glsl:/b.glsl" -> { "/a.glsl", "/b.glsl" };  "" -> {}
local function to_list(shaders)
  local list = {}
  if shaders and shaders ~= '' then
    for s in string.gmatch(shaders, '([^:]+)') do list[#list + 1] = s end
  end
  return list
end

local function current_shaders()
  return table.concat(mp.get_property_native('glsl-shaders') or {}, ':')
end

-- Apply a preset instantly (mpv rebuilds the chain without reloading the file).
local function apply(preset)
  mp.set_property_native('glsl-shaders', to_list(preset.shaders))
  config.active = preset.name
  mp.osd_message('Enhance: ' .. preset.name, 2)
end

-- Index of the preset matching the live chain (so the menu opens on it).
local function current_index()
  local now = current_shaders()
  for i, p in ipairs(config.presets) do
    if p.shaders == now then return i end
  end
  return 1
end

-- ---------- the on-screen menu ----------

local menu = { open = false, sel = 1, overlay = nil }
local close_menu -- forward declaration

local function redraw()
  if not menu.open then return end
  local w, h = mp.get_osd_size()
  if not w or w == 0 then return end
  local items = config.presets

  local fs = math.floor(h * 0.028)
  local title_fs = math.floor(h * 0.040)
  local hint_fs = math.floor(fs * 0.72)
  local pad = math.floor(h * 0.035)
  local row_h = math.floor(fs * 2.1)
  local radius = math.floor(h * 0.015)
  local pw = math.max(math.floor(w * 0.42), 560)
  local title_h = math.floor(title_fs * 1.9)
  local footer_h = math.floor(hint_fs * 2.6)
  local ph = pad * 2 + title_h + #items * row_h + footer_h
  local px = math.floor((w - pw) / 2)
  local py = math.floor((h - ph) / 2)

  local a = assdraw.ass_new()
  -- dim the whole screen
  a:new_event()
  a:append('{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H000000&\\1a&H60&\\p1}')
  a:draw_start(); a:rect_cw(0, 0, w, h); a:draw_stop()
  -- rounded panel, near-opaque shell background
  a:new_event()
  a:append(string.format('{\\an7\\pos(%d,%d)\\bord0\\shad0\\1c&H%s&\\1a&H14&\\p1}', px, py, BG))
  a:draw_start(); a:round_rect_cw(0, 0, pw, ph, radius); a:draw_stop()
  -- title: small accent kicker + heading, like the shell's details page
  a:new_event()
  a:append(string.format('{\\an1\\pos(%d,%d)\\fs%d\\b1\\bord0\\shad0\\fsp3\\1c&H%s&}ENHANCE',
    px + pad, py + pad + hint_fs, hint_fs, ACCENT))
  a:new_event()
  a:append(string.format('{\\an1\\pos(%d,%d)\\fs%d\\b1\\bord0\\shad0\\1c&H%s&}Upscaler',
    px + pad, py + pad + title_h - math.floor(title_fs * 0.25), title_fs, TEXT))

  local live = current_shaders()
  for i, p in ipairs(items) do
    local y = py + pad + title_h + (i - 1) * row_h
    if i == menu.sel then
      -- selected row: rounded surface fill + accent edge, like a focused card
      a:new_event()
      a:append(string.format('{\\an7\\pos(%d,%d)\\bord0\\shad0\\1c&H%s&\\1a&H20&\\p1}',
        px + math.floor(pad / 2), y, SURFACE))
      a:draw_start(); a:round_rect_cw(0, 0, pw - pad, row_h, math.floor(radius * 0.7)); a:draw_stop()
      a:new_event()
      a:append(string.format('{\\an7\\pos(%d,%d)\\bord0\\shad0\\1c&H%s&\\p1}',
        px + math.floor(pad / 2), y + math.floor(row_h * 0.18), ACCENT))
      a:draw_start()
      a:round_rect_cw(0, 0, math.floor(h * 0.004), math.floor(row_h * 0.64), math.floor(h * 0.002))
      a:draw_stop()
    end
    local col = (i == menu.sel) and TEXT or DIM
    local marker = (p.shaders == live) and string.format('{\\1c&H%s&}●{\\1c&H%s&}  ', ACCENT, col) or ''
    a:new_event()
    a:append(string.format('{\\an1\\pos(%d,%d)\\fs%d\\bord0\\shad0\\1c&H%s&%s}%s%s',
      px + pad, y + math.floor(row_h * 0.68), fs, col, (i == menu.sel) and '\\b1' or '', marker, p.name))
    a:new_event()
    a:append(string.format('{\\an3\\pos(%d,%d)\\fs%d\\bord0\\shad0\\1c&H%s&}%s',
      px + pw - pad, y + math.floor(row_h * 0.66), math.floor(fs * 0.78), DIM, p.hint or ''))
  end

  -- footer hints
  a:new_event()
  a:append(string.format('{\\an1\\pos(%d,%d)\\fs%d\\bord0\\shad0\\1c&H%s&}OK Apply    ·    Back Close    ·    X  A/B original',
    px + pad, py + ph - math.floor(footer_h * 0.35), hint_fs, DIM))

  if not menu.overlay then menu.overlay = mp.create_osd_overlay('ass-events') end
  menu.overlay.res_x = w
  menu.overlay.res_y = h
  menu.overlay.data = a.text
  menu.overlay:update()
end

local MENU_KEYS = {
  { 'UP', function() menu.sel = math.max(1, menu.sel - 1); redraw() end },
  { 'DOWN', function() menu.sel = math.min(#config.presets, menu.sel + 1); redraw() end },
  { 'ENTER', function() local p = config.presets[menu.sel]; if p then apply(p) end; close_menu() end },
  { 'ESC', function() close_menu() end },
  { 'BS', function() close_menu() end },
}

close_menu = function()
  if not menu.open then return end
  menu.open = false
  if menu.overlay then menu.overlay:remove() end
  for _, k in ipairs(MENU_KEYS) do
    mp.remove_key_binding('tvos-up-' .. k[1])
  end
end

local function open_menu()
  if #config.presets == 0 then
    mp.osd_message('Upscalers are still downloading — try again in a minute', 3)
    return
  end
  if menu.open then close_menu(); return end
  menu.open = true
  menu.sel = current_index()
  for _, k in ipairs(MENU_KEYS) do
    mp.add_forced_key_binding(k[1], 'tvos-up-' .. k[1], k[2], { repeatable = true })
  end
  redraw()
end

mp.observe_property('osd-dimensions', 'native', function() if menu.open then redraw() end end)

mp.add_key_binding(nil, 'tvos-upscaler-menu', open_menu)

mp.register_script_message('set-upscaler', function(name)
  for _, p in ipairs(config.presets) do
    if p.name == name then apply(p); return end
  end
end)

-- ---------- cycle + A/B toggle ----------

mp.add_key_binding(nil, 'tvos-upscaler-next', function()
  if #config.presets == 0 then return end
  local next_i = (current_index() % #config.presets) + 1
  apply(config.presets[next_i])
end)

local saved = nil
mp.add_key_binding(nil, 'tvos-enhance-toggle', function()
  local cur = mp.get_property_native('glsl-shaders')
  if cur and #cur > 0 then
    saved = cur
    mp.set_property_native('glsl-shaders', {})
    mp.osd_message('Enhance: Off (original)', 1.5)
  elseif saved then
    mp.set_property_native('glsl-shaders', saved)
    mp.osd_message('Enhance: On', 1.5)
  else
    for _, p in ipairs(config.presets) do
      if p.name == config.active then apply(p); return end
    end
  end
end)

-- ---------- live GPU budget: auto-degrade when frames drop ----------

-- Checked every 2s; two consecutive strained windows (so a seek spike doesn't
-- trigger it) step the chain down: "… Quality" → its "… Fast" sibling → Off.
local DROP_WINDOW = 2.0
local DROP_LIMIT = 12
local strained = 0
local last_drops = nil

local function lighter_preset()
  local live = current_shaders()
  local name = nil
  for _, p in ipairs(config.presets) do
    if p.shaders == live then name = p.name end
  end
  if name then
    local fast = name:gsub('Quality', 'Fast')
    if fast ~= name then
      for _, p in ipairs(config.presets) do
        if p.name == fast and p.shaders ~= '' then return p end
      end
    end
  end
  for _, p in ipairs(config.presets) do
    if p.shaders == '' then return p end
  end
  return nil
end

mp.add_periodic_timer(DROP_WINDOW, function()
  if mp.get_property_native('pause') then
    last_drops, strained = nil, 0
    return
  end
  local drops = mp.get_property_number('frame-drop-count')
  if not drops then return end
  local delta = last_drops and (drops - last_drops) or 0
  last_drops = drops
  if current_shaders() == '' then
    strained = 0
    return
  end
  strained = (delta >= DROP_LIMIT) and strained + 1 or 0
  if strained >= 2 then
    strained = 0
    local p = lighter_preset()
    if p then
      apply(p)
      mp.osd_message('GPU under load — Enhance stepped down to ' .. p.name, 3)
    end
  end
end)

-- ---------- startup hint: which profile the resolver picked ----------

mp.register_event('file-loaded', function()
  mp.add_timeout(1.2, function()
    mp.osd_message(string.format('Enhance: %s   ·   Y/U menu · X original',
      config.active or 'Off'), 4)
  end)
end)
