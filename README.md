# TV OS — Phases 1–6

A couch interface for a Linux gaming PC: one place for all your games (Steam,
Epic, retro), movies and shows (Stremio-compatible stream addons), with
content-aware AI upscaling and a local recommender — navigated entirely with
a gamepad or TV remote. See [PLAN.md](PLAN.md) for the full design.

## Install as an app (CachyOS / Arch)

Run it like any normal application — it appears in your app menu and opens in
its own window. From a clone of this repo:

```sh
sudo pacman -S --needed rust nodejs npm curl mpv chromium   # gamescope ffmpeg unzip recommended
./install.sh
```

That builds everything and installs **TV OS** into your application menu
(user-level, no root). Launch it from the menu, or from a terminal:

```sh
tvos-app        # windowed app
tvos-app --tv   # fullscreen big-screen mode via gamescope (controller/FSR/HDR)
```

Uninstall with `./uninstall.sh` (add `--purge` to also delete your config).

**Prefer a real pacman package?** `cd packaging && makepkg -si` builds and
installs `tvos` as a system package (remove with `sudo pacman -R tvos`).

Everything works the same in app mode as on a dedicated TV box: games and
emulators open as their own windows, video plays fullscreen in mpv through
the upscaler, and the controller drives the interface. Optional tools degrade
gracefully — no mpv yet means video won't play, but the rest of the UI runs.

## What works

- **Boot to TV** (phase 1): a "TV OS" login session where gamescope is the
  display server — no desktop, just the shell.
- **Controller + remote navigation**: gamepad (d-pad / left stick / A / B / Y)
  and keyboard arrows. HDMI-CEC remotes that present as keyboards work as-is.
- **Source registry** (phase 2): every kind of content comes from a `Source`
  (the compiled-in seed of the plan's addon protocol). Sources are detected at
  startup; their rows merge into one home screen:
  - **Steam** — installed games read from Steam's own library files, art from
    the Steam CDN. A to launch.
  - **Epic** — via the [legendary](https://github.com/derrod/legendary) CLI:
    installed games launch; owned-but-not-installed games appear in a
    "Ready to Install" row. A starts a **tracked download job** with live
    progress (downloads panel + card badge); when it finishes, the game moves
    into "Games" automatically. Run `legendary auth` once to sign in.
  - **Retro** (phase 3) — ROMs in `~/ROMs/<system>/` (EmuDeck layout;
    override with `TVOS_ROM_DIR`) join the same "Games" row — retro is not a
    separate world. A launches the ROM in RetroArch (native or flatpak),
    picking the best installed core per system (NES, SNES, GB/GBC/GBA,
    Genesis, N64, PSX); errors tell you exactly which core to download.
    Box art is scraped from the libretro thumbnail CDN by file name.
  - **ROM catalog** (phase 3) — a "Homebrew & Retro" row of downloadable,
    freely licensed homebrew (built-in manifest), installable from the couch:
    A downloads straight into the ROM tree with live progress, and the game
    moves into "Games" ready to play. Add your own catalogs with
    `TVOS_ROM_SOURCES=/path/a.json,/path/b.json` (same manifest format as
    `tvosd/data/homebrew.json`) — the seed of the plan's ROM-source addons.
  - **Videos** — files in `~/Videos` plus built-in sample streams, played
    fullscreen in mpv through the Enhance pipeline (below).
- **Player** — video plays in mpv dressed up as a real 10-foot player:
  `system/get-player.sh` installs **uosc** (a modern, Stremio-like on-screen
  UI — seek bar, subtitle/audio/chapter menus, settings) and **thumbfast**
  (hover/seek thumbnail previews). It's driven by a private `MPV_HOME` that
  tvosd writes a fresh `mpv.conf` into on every play (good streaming cache,
  subtitle defaults, and the resolved Enhance shaders), so the same player +
  upscaler apply to direct streams *and* torrents. Without uosc, mpv's
  built-in OSC is used, so there's always a seek bar.
- **Enhance — content-aware upscaling** (phase 4): every video launch goes
  through a resolver that picks the best mpv chain from the user's mode
  (**Auto / Quality / Performance / Off**, cycled with X / E, shown in the
  corner chip, persisted), the GPU (NVIDIA / AMD / none — Auto degrades
  gracefully), the content class (anime via Anime4K Mode A HQ, live action
  via FSRCNNX), and the source resolution (4K sources skip the chain;
  ffprobe is used for local files when present). `system/get-shaders.sh`
  fetches the shaders; missing shaders degrade to mpv's high-quality
  scalers, never break playback. During playback, press **e** in mpv for an
  instant A/B against the unenhanced original. Env overrides: `TVOS_GPU`
  (nvidia/amd/none), `TVOS_SHADER_DIR`.
  The VapourSynth/TensorRT NN models from PLAN.md slot in behind this same
  resolver later.
- **Game upscaling defaults** (phase 4): Epic launches export
  `PROTON_FSR4_UPGRADE=1` on AMD (needs a Proton with FSR4-upgrade support,
  e.g. Proton-CachyOS) or `PROTON_ENABLE_NVAPI=1` on NVIDIA so engine-level
  FSR4/DLSS — better than any video upscaler — is on by default where games
  support it. Steam games: set per game in Steam's own properties.
- **Stremio-compatible stream addons** (phase 5): tvosd speaks the open
  [Stremio addon protocol](https://github.com/Stremio/stremio-addon-sdk).
  Install any addon by its `manifest.json` URL; its catalogs become home
  rows, and pressing play asks every stream-capable addon for streams,
  **ranks them** (resolution from the label, then https over http) and plays
  the best one in mpv — through the Enhance upscaling pipeline, which is the
  thing Stremio's own player can't do. Torrent-only entries (bare infoHash)
  are skipped: there is no torrent engine; debrid-backed addons that return
  direct URLs work. YouTube (`ytId`) streams play via mpv's yt-dlp hook.
  Addons persist (with their manifests) in `~/.config/tvos/addons.json`, so
  the home screen works offline at boot. See **Addons** below.
- **Recommender** (phase 6): fully local, no cloud. Every successful launch
  is appended to `~/.config/tvos/events.jsonl`; the home screen leads with
  two personalized rows built from it: **Continue** (most recent distinct
  items — games and video mixed) and **Recommended for You** (frequency ×
  14-day-half-life recency decay, boosted for items you usually use at this
  time of day, excluding the item already leading Continue). Embedding-based
  similarity can replace the scorer later behind the same row contract.
  - **TMDB** — optional "Trending Movies" discovery row; set `TVOS_TMDB_KEY`
    (free API key) to enable. Becomes playable when stream sources land.
- **Light & dark mode**: Y on the gamepad / T on a keyboard toggles; the
  choice persists, defaulting to the system preference.

## Layout

| Path | What it is |
|---|---|
| `tvosd/` | Rust daemon: serves the UI + JSON API, hosts sources, runs download jobs |
| `tvosd/src/sources/` | The `Source` trait + steam / epic / retro / videos / tmdb sources |
| `tvosd/src/install.rs` | Download manager: CLI-wrapping jobs and direct-download jobs, with live progress |
| `tvosd/data/homebrew.json` | Built-in ROM catalog (freely licensed homebrew with direct URLs) |
| `tvosd/src/upscale.rs` | Enhance resolver: mode × GPU × content class × source res → mpv chain |
| `tvosd/src/recommend.rs` | Local recommender: event log → Continue / Recommended rows |
| `system/package.sh` | Builds the self-contained distributable package |
| `tvosd/src/settings.rs` | Persisted user settings (single store shared across the daemon) |
| `system/get-shaders.sh` | Fetches Anime4K v4 + FSRCNNX shader packs |
| `system/get-player.sh` | Installs the mpv player UI (uosc + thumbfast) |
| `shell/` | React UI: rows, focus engine, input, theming, downloads panel |
| `system/` | Session scripts, SDDM/wayland session files, installer |

## API

| Endpoint | Meaning |
|---|---|
| `GET /api/library` | Home rows; each item carries its `action` (`play` / `install` / `none`) |
| `GET /api/sources` | Which sources were detected |
| `POST /api/launch {"id"}` | Play/run an item (`steam:620`, `epic:Sugar`, `rom:gb/Game.gb`, `video:…`) |
| `POST /api/install {"id"}` | Start a download job |
| `GET /api/installs` | All jobs with status + progress |
| `GET / PUT /api/settings` | User settings (enhance, Steam creds, TMDB key), persisted to `~/.config/tvos/` |
| `GET /api/steam/status` | Tests saved Steam creds → `{connected, count}` or `{error}` |
| `GET / POST /api/addons` | List installed addons / install one (`{"url": "…/manifest.json"}`) |
| `POST /api/addons/remove` | Uninstall an addon (`{"url": …}`) |
| `GET /api/meta?id=` | Details-page metadata: summary + episode list (series) |
| `GET /api/streams?id=` | Every source for a title/episode (direct, torrent, external, youtube) |
| `POST /api/play` | Play a chosen `{stream, item}` |
| `POST /api/open {"url"}` | Open a link with the system (WatchHub apps, addon Configure pages) |
| `GET /api/version` | Daemon version (handy for testing packages) |

`POST /api/launch` optionally takes `title`, `kind`, `art` alongside `id`;
when present, a successful launch is recorded for the recommender (the shell
always sends them).

## What's deliberately still open

Profiles ("who's watching"), HDMI-CEC power sync, the phone companion app,
an on-screen keyboard for controller-only text entry, the aggregated store
pages + checkout webview, and the VapourSynth/TensorRT upscaling backend.
Each slots behind an existing seam (the `Source` trait, the Enhance resolver,
the recommender's row contract) — none require rearchitecting.

Item ids are launchable strings whose prefix names the owning source — the
HTTP addon protocol generalizes exactly this in a later phase.

## Develop (any desktop, including macOS for the UI)

```sh
# terminal 1 — daemon on 127.0.0.1:8484
cd tvosd && cargo run

# terminal 2 — UI with hot reload, proxies /api to the daemon
cd shell && npm install && npm run dev
```

Open the printed Vite URL, navigate with arrow keys or a connected gamepad.
`cargo test` covers Steam/Epic/TMDB parsing and install-progress parsing.

To exercise the whole Epic install flow without an Epic account, put a fake
`legendary` script on the daemon's PATH that prints the same JSON and
progress lines the real one does (see the real CLI's output, or
`tvosd/src/sources/epic.rs` for exactly what gets parsed).

## Package it / test it anywhere

```sh
./system/package.sh
```

produces `dist/tvos-<version>-<os>-<arch>.tar.gz` — a self-contained app:
the release daemon, the built UI, session files, the shader fetcher, the
sample addon, and two entry points:

- **`./run-demo.sh`** — runs the whole OS from the extracted folder in a
  sandbox (config/ROMs/shaders in a temp dir, sample stream addon
  auto-installed). Works on any Linux or macOS box with nothing installed —
  open `http://127.0.0.1:8484` in a browser and drive it with arrows or a
  gamepad. Set `TVOS_DEMO_DIR=/some/path` to keep demo state between runs.
- **`./install.sh`** — installs the prebuilt package properly (no toolchains
  needed on the TV box). Note the binary matches the machine that ran
  `package.sh` — build on the gaming PC, or cross-compile.

## Install on the gaming PC (Bazzite recommended)

Requirements: `gamescope`, `mpv`, Steam, a Chromium-family browser, Rust and
Node toolchains to build (or build elsewhere and copy the artifacts).

```sh
./system/install.sh
```

Then either:

- **Test inside your desktop**: `TVOS_WINDOWED=1 tvos-session` — runs the
  whole thing nested in a window; or
- **Go full TV**: log out, pick the **TV OS** session on the login screen.
  For console-like boot, enable autologin to that session
  (System Settings → Users on KDE, or an `/etc/sddm.conf.d/` autologin entry).

## Settings panel

Press **S** (or the gamepad **Start** button, or click the **⚙ SETTINGS**
chip) to open it. It's themed to the app and reachable even on a fresh, empty
install. Four sections:

- **Steam account** — paste a [Web API key](https://steamcommunity.com/dev/apikey)
  and your SteamID64 (or just your profile name — it's resolved automatically),
  then **Connect & sync games**. Your *entire owned library* is pulled via the
  Steam Web API and merged with anything installed locally into the Games row;
  pressing play launches via `steam://`, so an owned-but-uninstalled game
  prompts Steam to install it. (Your profile's game details must be public.)
- **Movies & TV (TMDB)** — paste a free TMDB API key to fill the Trending
  Movies and Shows rows. Playing one maps the title to its IMDb id and resolves
  a stream through your installed addons.
- **Stremio addons** — paste any addon's `manifest.json` URL to install it,
  see what's installed, and remove with one click.
- **Appearance & playback** — theme and Enhance (upscaling) mode.

Everything you launch — Steam games, addon streams, TMDB titles — is recorded
locally and surfaces in the **Continue** and **Recommended for You** rows at the
top of the home screen. Credentials are stored in `~/.config/tvos/settings.json`
on your machine; nothing leaves the box.

## Addons

### Game sources

| Store | What to do |
|---|---|
| **Steam** | Open Settings → Steam and connect your account (above), **or** just install Steam locally — installed games are found automatically (multi-disk libraries included). |
| **Epic** | Install [legendary](https://github.com/derrod/legendary) (`pip install legendary-gl` or the distro package) and run `legendary auth` once. Installed games launch; owned games appear in "Ready to Install". |
| **Retro** | Drop ROMs into `~/ROMs/<system>/` (nes, snes, gb, gbc, gba, genesis, n64, psx) **or** install from the built-in "Homebrew & Retro" row. Install RetroArch (`flatpak install flathub org.libretro.RetroArch`) and the cores you need. |
| **More ROM catalogs** | Write a manifest like `tvosd/data/homebrew.json` and point `TVOS_ROM_SOURCES=/path/one.json,/path/two.json` at it. |
| GOG / Amazon | Planned — same `Source` seam (gogdl / nile). |

### Stream addons (Stremio-compatible)

Install any addon from its manifest URL — same URLs the Stremio community
publishes:

```sh
# the official Cinemeta catalog (popular movies & series rows):
curl -X POST http://127.0.0.1:8484/api/addons \
     -H 'Content-Type: application/json' \
     -d '{"url": "https://v3-cinemeta.strem.io/manifest.json"}'

# list / remove:
curl http://127.0.0.1:8484/api/addons
curl -X POST http://127.0.0.1:8484/api/addons/remove \
     -H 'Content-Type: application/json' -d '{"url": "…"}'
```

…or just paste the URL into **Settings → Stremio addons**. New rows appear on
the next home-screen load. All four stream types are supported:

| Stream type | Example addon | How it plays |
|---|---|---|
| Direct / debrid URL | Torrentio + RealDebrid, others | our mpv + the Enhance upscaler (full seeking) |
| Torrent (magnet) | **Torrentio** (no debrid) | streamed via `webtorrent-cli`'s `--mpv` mode (seekable, with the upscaler). Install it: `npm install -g webtorrent-cli` — tvosd finds it even off-PATH |
| External link | **WatchHub** | opens the streaming service's app/site (Netflix, SkyShowtime…) |
| YouTube | trailers, channels | mpv via yt-dlp |

Notes:

- **Catalog-only addons** (like Cinemeta) give you rows to browse and the
  episode lists / summaries; you also need a **stream-capable** addon
  (Torrentio, WatchHub, …) to play. Cinemeta also powers the details page.
- **Details page**: pressing A on any entry opens a summary page — poster,
  description, genres, rating. Series show a **season/episode list** you
  launch individually; movies and episodes show a **picker of every source**
  (Torrentio qualities + WatchHub apps), each launchable on its own.
- **Configuring addons**: in Settings, a configurable addon (e.g. Torrentio)
  shows a **Configure** button that opens its setup page — add a debrid key
  there, then paste the configured manifest URL it gives you. Debrid streams
  come back as direct URLs (full seeking + upscaler).
- What you install is your responsibility — stick to addons that serve
  content legally.
- Try it end-to-end with the bundled sample addon:
  `python3 tools/sample-addon.py &` then install
  `http://127.0.0.1:7100/manifest.json` — a "Blender Films" row appears and
  plays through the upscaler. The script is ~80 lines and is the template
  for writing your own addon.

### Playback troubleshooting

- **A stream won't play** → read `~/.local/share/tvos/mpv/mpv.log` (mpv's own
  log) and `~/.local/share/tvos/mpv/play.log` (webtorrent's output). The
  daemon also prints a `playing […]` line per launch.
- **"No space" / nothing plays** → check `df -h`; free up disk. Torrents
  download to `~/.cache/tvos/torrents` (override with `TVOS_TORRENT_DIR`) —
  clear it with `rm -rf ~/.cache/tvos/torrents/*` to reclaim space.
- **Torrents need peers** → an unhealthy torrent (few seeders) may stall;
  pick another source, or use a debrid-configured addon for direct URLs.
- Requires `mpv` installed (`sudo pacman -S mpv`); torrents also need
  `webtorrent-cli` (`npm install -g webtorrent-cli`).

## Controls

| Action | Gamepad | Keyboard / remote |
|---|---|---|
| Move | D-pad / left stick | Arrow keys |
| Play / install | A | Enter |
| Back | B | Esc |
| Settings | Start | S |
| Enhance mode | X | E |
| Light/dark mode | Y | T |
| A/B compare (during playback) | — | E in mpv |

## Design notes (for review)

- The shell is a dumb renderer; all state that matters (library, launching)
  lives in the daemon. UI tech can be swapped later without logic changes.
- `tvos-shell` starts the daemon *inside* gamescope, so windows spawned by
  the daemon (Steam, mpv) inherit the session display and appear on the TV.
- Steam discovery reads `libraryfolders.vdf` + `appmanifest_*.acf` directly —
  no Steam API keys, works offline, and covers multi-disk libraries.
- Content ids (`steam:…`, `video:…`) are deliberately launchable strings:
  phase 2's addon protocol turns the prefix into a registered source type.
