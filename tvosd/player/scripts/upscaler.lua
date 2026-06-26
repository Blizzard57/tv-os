-- TV OS — realtime upscaler switching.
--
-- The daemon writes the available upscaler presets for this playback to
-- ~~/upscalers.json (MPV_HOME/upscalers.json): a list of { name, hint, shaders }
-- where `shaders` is a ':'-joined list of .glsl paths ("" = off). This script
-- lets the user switch between them live — mpv reloads the GLSL chain on the
-- fly, no playback restart — via:
--
--   * a uosc menu (the "Enhance" button, or `u`)   — pick any preset
--   * `n`                                           — cycle to the next preset
--   * `e`                                           — quick A/B (current ⇄ off)
--
-- Edit behaviour here (tvosd/player/scripts/upscaler.lua); edit *which* presets
-- exist in tvosd/src/upscale.rs.

local mp = require 'mp'
local utils = require 'mp.utils'

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
    for s in string.gmatch(shaders, '([^:]+)') do
      list[#list + 1] = s
    end
  end
  return list
end

local function current_shaders()
  return table.concat(mp.get_property_native('glsl-shaders') or {}, ':')
end

-- Apply a preset instantly. Setting glsl-shaders makes mpv rebuild the chain
-- without reloading the file, so the picture changes in place.
local function apply(preset)
  mp.set_property_native('glsl-shaders', to_list(preset.shaders))
  config.active = preset.name
  mp.osd_message('Enhance: ' .. preset.name, 2)
end

local function find(name)
  for _, p in ipairs(config.presets) do
    if p.name == name then return p end
  end
end

-- Index of the preset matching the live chain (so "next" steps from reality).
local function current_index()
  local now = current_shaders()
  for i, p in ipairs(config.presets) do
    if p.shaders == now then return i end
  end
  return 0
end

-- ---- uosc menu ----

local function open_menu()
  if #config.presets == 0 then
    mp.osd_message('No upscalers available — run system/get-shaders.sh', 3)
    return
  end
  local now = current_shaders()
  local items = {}
  for _, p in ipairs(config.presets) do
    items[#items + 1] = {
      title = p.name,
      hint = p.hint,
      active = p.shaders == now,
      value = { 'script-message-to', mp.get_script_name(), 'set-upscaler', p.name },
    }
  end
  local menu = { type = 'tvos-upscaler', title = 'Upscaler', items = items }
  mp.commandv('script-message-to', 'uosc', 'open-menu', utils.format_json(menu))
end

mp.register_script_message('set-upscaler', function(name)
  local preset = find(name)
  if preset then apply(preset) end
end)

mp.add_key_binding(nil, 'tvos-upscaler-menu', open_menu)

-- ---- cycle to next preset ----

mp.add_key_binding(nil, 'tvos-upscaler-next', function()
  if #config.presets == 0 then return end
  local next_i = (current_index() % #config.presets) + 1
  apply(config.presets[next_i])
end)

-- ---- quick A/B toggle ----

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
    local preset = find(config.active)
    if preset then apply(preset) end
  end
end)
