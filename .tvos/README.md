# TV OS portable profile

This marker makes `system/tvos-app` use `.tvos/profile/` for TV OS state when
the app is launched from this repo.

The profile contains settings, addons, CloudStream manifests, resume positions,
watch/recommendation history, the browser profile, mpv state, logs, and torrent
cache. It may contain account tokens and cookies, so keep the repo private if
you sync it between machines.
