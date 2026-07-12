-- TV OS — game controller support for the player.
--
-- mpv (built with sdl2-gamepad) emits GAMEPAD_* keys when input-gamepad=yes,
-- which tvosd sets in mpv.conf. We translate them into the ordinary keyboard
-- keys that the TV overlay, the upscaler menu, and input.conf already handle,
-- so a controller behaves exactly like the remote.
--
-- Edit the layout here (tvosd/player/scripts/controller.lua).

local mp = require 'mp'

-- gamepad button  ->  keyboard key it acts as
local layout = {
  GAMEPAD_DPAD_UP = 'UP',
  GAMEPAD_DPAD_DOWN = 'DOWN',
  GAMEPAD_DPAD_LEFT = 'LEFT',
  GAMEPAD_DPAD_RIGHT = 'RIGHT',
  GAMEPAD_LEFT_STICK_UP = 'UP',
  GAMEPAD_LEFT_STICK_DOWN = 'DOWN',
  GAMEPAD_LEFT_STICK_LEFT = 'LEFT',
  GAMEPAD_LEFT_STICK_RIGHT = 'RIGHT',

  -- Face buttons (Xbox; PlayStation cross/circle/square/triangle align)
  GAMEPAD_ACTION_DOWN = 'ENTER', -- A / ✕ : confirm · play/pause
  GAMEPAD_ACTION_RIGHT = 'ESC',  -- B / ○ : back · close menu · quit
  GAMEPAD_ACTION_LEFT = 'e',     -- X / ▢ : Enhance on/off
  GAMEPAD_ACTION_UP = 'u',       -- Y / △ : Upscaler menu

  GAMEPAD_LEFT_SHOULDER = 'Shift+LEFT',
  GAMEPAD_RIGHT_SHOULDER = 'Shift+RIGHT',
  GAMEPAD_START = 'SPACE',
  GAMEPAD_BACK = 'ESC',
}

for button, key in pairs(layout) do
  mp.add_forced_key_binding(button, 'tvos-gp-' .. button, function()
    mp.commandv('keypress', key)
  end, { repeatable = true })
end
