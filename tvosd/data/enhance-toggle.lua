-- A/B comparison for the Enhance shader chain: press "e" during playback to
-- flip the upscaler off and on. tvosd passes this script to mpv whenever a
-- profile with shaders is active.
local saved = nil

mp.add_key_binding("e", "tvos-enhance-toggle", function()
    local current = mp.get_property_native("glsl-shaders")
    if current and #current > 0 then
        saved = current
        mp.set_property_native("glsl-shaders", {})
        mp.osd_message("Enhance: OFF (original)", 1.5)
    elseif saved then
        mp.set_property_native("glsl-shaders", saved)
        mp.osd_message("Enhance: ON", 1.5)
    end
end)
