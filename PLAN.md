# TV OS — The Plan (v2, simplified)

A Linux TV OS for a gaming PC. One interface, one design language, one rule:

> **Everything is an addon.** Video sources, game sources, retro sources, catalogs, metadata — all speak the same small protocol, exactly like Stremio addons but for *all* media types. The core OS stays tiny; sources are plug-in and community-extendable.

---

## 1. The one core idea

### 1.1 Content model
Every item on screen is the same thing:

```
ContentItem {
  id            // tmdb:603, igdb:1942, rom:snes/...
  kind          // movie | series | game
  title, art, metadata
}
```

Games, retro games, movies, shows — one search, one home screen, one details page. No separate "emulation section". A SNES game and a Steam game and a movie are just items with different *sources*.

### 1.2 Addon protocol (Stremio model, extended to games)
An addon is a tiny HTTP service (local or remote) declaring capabilities in a manifest:

| Capability | Input → Output | Examples (shipped) |
|---|---|---|
| `catalog` | browse/search → ContentItems | TMDB, IGDB, "Trending", store catalogs |
| `meta` | id → full metadata/art | TMDB, IGDB, ScreenScraper, fanart.tv |
| `stream` | video id → playable stream URLs | YouTube, Jellyfin-as-addon, free/legal catalogs; compatible with the existing Stremio addon ecosystem |
| `install` | game id → installable + runner hint | Steam, Epic (legendary), GOG (gogdl), Amazon (nile), itch.io, homebrew ROM repos, your NAS (SMB/WebDAV) |

- `install` returns *what to download* and *how to run it*: `{files | store-handoff, runner: proton | native | emulator(core)}`. A GOG installer, a Steam appid, and a ROM are the same shape to the OS.
- Community addons: paste a manifest URL (or scan QR from phone) → installed. **Legal defaults shipped; what users add is on them** — same stance for video streams and game sources.
- Resolver auto-ranks results (resolution/codec/health for streams; price/ownership/preferred-store for installs) and **just plays/installs the best one**. "More sources" is one level deeper for power users.

### 1.3 Storage policy
- **Video: streamed, zero local storage.** "Download" is an opt-in button per item.
- **Games: installed via download manager** (queue, checksums, auto-placement, auto-scrape) — never a manual file transfer. ROMs verified against No-Intro/Redump.

---

## 2. System layers (only four)

```
┌─────────────────────────────────────────────┐
│  Shell  (React, fullscreen, controller/CEC)  │
├─────────────────────────────────────────────┤
│  tvosd  (Rust daemon: addons, library,       │
│          resolver, downloads, recommender)   │
├─────────────────────────────────────────────┤
│  Runners: gamescope+Proton | mpv+VapourSynth │
│           | RetroArch & standalone emulators │
├─────────────────────────────────────────────┤
│  Base: Bazzite HTPC image (atomic updates,   │
│        HDR, drivers, autologin → shell)      │
└─────────────────────────────────────────────┘
```

- **Base**: Bazzite (immutable Fedora) — controllers, HDR, VRR, gamescope session, OTA updates already solved. Boot → splash → shell. No desktop, ever (hidden dev exit).
- **tvosd**: the only brain. Hosts addons, owns the SQLite library + event log, runs the resolver, download manager, recommender, upscaler-profile picker. gRPC/WebSocket API.
- **Shell**: dumb, fast, beautiful renderer of what tvosd serves.
- **Runners**: every launch goes through gamescope (per-item profiles: resolution, FSR, HDR, frame cap). Emulators ship pre-configured EmuDeck-style — hotkeys, save paths, shaders done for you.

No proprietary streaming services (Netflix etc.) — their DRM can't enter mpv, and dropping them deletes an entire subsystem. The only embedded web page in the OS is a store checkout.

---

## 3. UX: the simplicity rules

1. **Two presses to play.** Home row item → Play. Resolver handles source choice silently; mid-buffer fail-over to the next stream.
2. **One details page** for everything: full-bleed art, logo titles, muted trailer on focus, one primary button (Play / Resume / Install / Buy).
3. **One player** for everything: mpv overlay with thumbnail scrubber, next-episode countdown, skip-intro, subtitle picker, single "Enhance: Auto/Quality/Performance/Off" toggle.
4. **One "Continue" row** mixing your suspended game and half-watched episode.
5. **Zero-config onboarding**: pick profile → QR-sign-in to stores → pick a few tastes → everything else (addons, emulators, upscaler models) configures itself in the background.
6. **Feel**: 60fps UI, parallax focus, haptics, ambient color from artwork, dark default, theming SDK.
7. **Input**: gamepad (SDL) + TV remote via HDMI-CEC (libcec / Pulse-Eight USB-CEC) + phone app. CEC power sync: TV on/off = PC wake/sleep, console-style instant resume.

---

## 4. Buying games
Real purchases must use each store's checkout (ToS/payments). Aggregated store pages show every store's price (IsThereAnyDeal) + deals + wishlist alerts; **Buy** opens that store's checkout in a controller-navigable webview, then the library auto-syncs. GOG/itch (DRM-free) are fully native: buy → download → play without leaving the shell.

---

## 5. Upscaling (auto, content-aware)

| Content | Default | Why |
|---|---|---|
| Video (all streamed/local) | **VapourSynth + TensorRT NN models** (AnimeJaNai/Real-ESRGAN-Compact for anime, compact live-action models from OpenModelDB; ArtCNN/FSRCNNX fallback; Anime4K on weak GPUs) | The actual best real-time AI upscaling on Linux. RTX VSR is Windows-only. |
| Modern PC games | **FSR4** on AMD (OptiScaler / `PROTON_FSR4_UPGRADE=1`; RDNA4 now, RDNA3 from FSR 4.1 in July 2026) · **DLSS4** on NVIDIA | Temporal, engine-integrated — best possible quality. **FSR4 can't do video**: it needs motion vectors/depth that only game engines provide. |
| Old games / emulators | Native internal-res 4K (Dolphin/PCSX2 etc.), RetroArch shaders (CRT-Royale) for 2D, **gamescope FSR1** otherwise | Rendering at 4K beats any upscaler. |

**Auto-selection**: resolver picks the video chain from content class (anime vs live-action, from metadata + cheap frame classifier), source res/bitrate, live GPU budget (auto-degrades if frames drop), and panel target. User sees one toggle + an A/B split preview.

**GPU choice**: NVIDIA RTX = best video upscaling (TensorRT) + DLSS4. AMD RDNA4 = first-class FSR4 for games, lighter Vulkan models for video. Video-first → NVIDIA; games-first → either.

---

## 6. Recommender (local, private)
- SQLite event log: plays, watches, completions, abandons, time-of-day, searches.
- One embedding space over games *and* video (sentence-transformer on tags/descriptions, ONNX on-box) → cross-domain rows ("binged Edgerunners → Cyberpunk 2077").
- Ranking = recency-weighted item-item similarity + time-of-day prior + small exploration so rows stay fresh. Household profiles, "who's watching" on boot, cold-start taste picker + Steam/Trakt import.

---

## 7. Extras worth shipping
Sunshine/Moonlight (stream the rig anywhere), RetroAchievements, Trakt sync, voice search (local Whisper), phone companion (remote + keyboard + "play on TV"), MangoHud perf overlay, parental/kid profiles, automatic save backup, music/podcasts row, Twitch/YouTube live rows (through the upscaler!).

---

## 8. Roadmap (each phase usable)

| Phase | Deliverable |
|---|---|
| 1. Couch shell | Bazzite + gamescope session, controller/CEC nav, launch Steam games + play a video in mpv. *De-risks input + session model.* |
| 2. Addon core | tvosd + addon protocol; Steam/Epic/GOG/itch install addons; TMDB/IGDB catalogs; download manager. |
| 3. Retro | Emulator runners auto-configured, ROM-source addons, scraping, save sync. |
| 4. Player & upscaling | libmpv + VapourSynth/TensorRT chain, auto-profile resolver, A/B preview; FSR4/DLSS defaults for games. |
| 5. Streams & store | Stremio-compatible stream addons + resolver/ranker; aggregated store pages + checkout webview. |
| 6. Recommender & polish | Event log → embeddings → home rows; profiles; CEC power sync; phone app; addon catalog UI; theming SDK. |
