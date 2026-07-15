# TV OS — full UI design prompt

> Paste everything below the line into Claude. It is self-contained.

---

You are designing the complete visual system for **TV OS**, a living-room operating-system shell that already exists as a working React app. This is a **redesign of an implemented product**, not a greenfield concept: every screen, state, and control listed below is real and currently rendered by code. Your job is to make all of it look like a modern, cinematic, first-party TV interface — coherent, restrained, and beautiful at 10 feet.

## Deliverable

A **single self-contained HTML file** (inline `<style>`, no external assets, no build step, no JS frameworks; a few lines of vanilla JS only for the theme toggle and page switcher). It must:

- Render **every screen and every state** in the inventory below as a separate full-viewport section, reachable from a small fixed dev-only switcher in the corner (the switcher is scaffolding, not part of the design).
- Support **both themes** via `data-theme="dark"` / `data-theme="light"` on `<html>`, driven entirely by CSS custom properties. Dark is the default and the primary target; light must be genuinely designed, not an inversion.
- Use the **exact class names and DOM structure** from the "Component contract" section, so the design can be ported back into the React shell by replacing CSS, not by rewriting components.
- Use placeholder artwork via inline SVG data-URIs or CSS gradients — no external image URLs, no CDN fonts. Assume `-apple-system, BlinkMacSystemFont, "SF Pro Display", Inter, system-ui, sans-serif`.
- Include, at the top of the `<style>` block, a commented **design-rule preamble** stating the principles you committed to, so future edits stay consistent.

## What TV OS is

One private, local library that unifies things that normally live in separate apps:

- **Games** — Steam, Epic, GOG, and retro/ROM titles. Owned, installed, or purchasable.
- **Movies and Shows** — metadata from TMDB, played through a local player, streamed from user-configured sources.
- **Live** — IPTV channels and sports fixtures (live now or upcoming with a countdown).
- **Video** — YouTube channels the user follows.

A local Rust daemon (`tvosd`) serves a flat list of rows; the shell filters them into destinations. Nothing is a web page — this is a system shell that boots to the couch.

## Non-negotiable constraints

These come from the code and the hardware. Violating any of them breaks the product.

1. **Ten-foot readability.** Nothing below 24px. Body copy ~26px. Assume 1920×1080 minimum, viewed from 8–10 feet. Scale type and geometry from viewport units (e.g. `clamp()`) so a 1080p panel and a 4K panel read identically.
2. **No cursor, no hover.** `cursor: none` is set globally. **Hover states do not exist.** Every affordance must be carried by the **focus** state alone. Any design that depends on hovering is wrong.
3. **Focus is the only selection.** Real DOM focus moves by geometry (spatial navigation) from a d-pad, gamepad stick, or arrow keys. The focused element must be unmistakable from across a room — the current system uses a bone-white ring with a dark outer ring plus a card shadow, and a gentle `1.05` scale lift (not a big zoom). Design the focus treatment deliberately for every focusable control type: cards, nav items, buttons, list rows, toggles, swatches, keyboard keys.
4. **Overscan safe areas.** Many TVs crop ~5% of each edge. Left/right safe margin `max(80px, env(safe-area-inset-left))`, top/bottom `max(60px, ...)`. Nothing important — text, focus rings, badges — may sit outside it.
5. **No pure white text on dark.** Peak luminance hurts in a dark room. Labels are a warm off-white (`#f5f5f7`), secondary labels are translucent.
6. **Vertical moves between shelves, horizontal browses within one shelf. Always.** The navigation model must be visually obvious.
7. **Content is the interface.** Chrome recedes when not engaged. The top navigation is dormant until summoned.
8. **Accessibility flags are real and must be styled.** `<html>` carries `data-reduce-motion`, `data-reduce-transparency`, `data-high-contrast`, `data-bold-text` (all `"true"`/`"false"`). Provide working overrides for each: no transitions/scale under reduced motion, opaque fills replacing translucency under reduce-transparency, stronger separators and label contrast under high-contrast, heavier weights under bold-text.
9. **Accent is user-chosen at runtime.** `--accent` and `--on-accent` are set by JS from a profile preference (default `#0a84ff`; presets include violet, pink, red, amber, green, teal). **Never hardcode the accent** — every accent use must go through the variable and must still work if the accent is amber or pink. Accent is used *sparingly*: progress, selection, active status. It is not a decoration.
10. **Own branding only.** Inspired by the polish of first-party TV interfaces; no copied assets, logos, wordmarks, or trade dress.

## Design tokens (already defined — extend, don't replace)

The appearance layer (`native.css`) owns these. Keep the names; refine the values if you can justify it.

```
Dark:  --canvas #000 · --canvas-elevated #1c1c1e · --surface-secondary #2c2c2e
       --label #f5f5f7 · --label-secondary rgba(235,235,245,.68) · --label-tertiary rgba(235,235,245,.42)
       --separator rgba(84,84,88,.65) · --glass rgba(28,28,30,.7) · --glass-strong rgba(28,28,30,.9)
       --input rgba(118,118,128,.24) · --focus-fill #f5f5f7 · --focus-label #050505
Light: --canvas #f5f5f7 · --canvas-elevated #fff · --label #1d1d1f
       --label-secondary rgba(60,60,67,.72) · --separator rgba(60,60,67,.18)
       --glass rgba(255,255,255,.72) · --accent #007aff
Status: --status-live #ff453a · --status-success #30d158 · --status-warning #ffd60a · --status-error #ff453a
Artwork (text sitting on media): --artwork-label #fff · --artwork-secondary rgba(255,255,255,.72) · --artwork-scrim rgba(0,0,0,.82)
Geometry: --safe-x · --safe-top · --safe-bottom · --top-nav-h 76px · --page-top
          --radius-control 16px · --radius-card 18px · --radius-panel 26px
          --card-w clamp(340px, 23.5vw, 456px) · --card-gap · --shelf-gap · --strip-pad · --card-lift 1.05
Depth: --shadow-card · --shadow-panel · --focus-outline
Motion: --ease-out cubic-bezier(.2,.8,.2,1)
```

Surfaces are tagged in the DOM with `data-surface="canvas" | "glass" | "media"`. Use that as a real semantic layer: **canvas** is the app background, **glass** is translucent chrome floating over content, **media** is a surface where artwork is the background and text needs a scrim. Text on `media` uses the artwork tokens, never the label tokens.

## Data model (what the pixels are made of)

```ts
ContentItem { id; kind: 'game'|'video'|'movie'|'series'|'live'; title; art?;
              action: 'play'|'install'|'none'; note?;
              presentation?: 'artwork'|'channel-logo'|'event';
              live?: { state: 'live'|'upcoming'|'channel'; starts_at?; carrier?; group? };
              game?: { store: 'steam'|'epic'|'gog'|'retro'; ownership: 'installed'|'owned'|'unowned' };
              artwork?: { portrait?; landscape?; background?; logo? } }
Row  { title; items: ContentItem[] }
Meta { kind; title; poster?; background?; logo?; description?; release_info?; rating?; runtime?;
       developer?; publisher?; genres[]; tags?; screenshots?; episodes: Episode[] }
InstallJob { id; title; status: 'running'|'done'|'failed'; progress 0–100; detail }
Profile { id; name; avatar_color; preferences: { theme; accent; reduced_motion; ... } }
```

Cards carry these as data attributes, and the design should use them as styling hooks: `data-presentation`, `data-live-state`, `data-store`, `data-ownership`, `data-primary`, `data-item-id`.

## Component contract (keep these class names)

```
.app.apple-shell[data-surface="canvas"][data-page="<tab>"]   (+ .nav-is-open when nav engaged)
  .top-nav[data-surface="glass"] (+ .top-nav-engaged)
    .top-nav-profile > .profile-orb
    .profile-switcher > .profile-switcher-title, .profile-choice(.active) > .profile-choice-orb, .profile-manage
    .top-nav-destinations > .top-nav-item(.top-nav-item-active) > span, .nav-badge(.nav-badge-error)
    .top-nav-utilities > .top-nav-item.top-nav-utility
  .hero[data-surface="media"] (+ .hero--fallback, .hero-empty)
    .hero-bg.hero-bg-image · .hero-scrim
    .hero-content > .hero-kind, .hero-logo | .hero-title, .hero-sub, .hero-desc, .hero-hint > .key
  .home-rows
    .game-source-notice
    .shelf > .shelf-title(.shelf-title-button > span "See All") , .shelf-strip
      .card[tabindex=0] (+ .card--game)
        .card-thumb (+ .card-thumb--logo, .card-thumb--game)
          .card-art (+ .card-art--logo, .card-art--contain) | .card-placeholder(.card-monogram)
          .card-badge (.badge-installed|.badge-owned|.badge-buy|.badge-live)
          .card-progress > .card-progress-fill
        .card-label > .card-title, .card-subtitle
  .downloads > .downloads-title, .download > .download-row > .download-name/.download-pct,
               .download-bar > .download-fill(.download-fill-failed)
  .toast
```

Page layers (each `.page-layer[data-surface]` covers the shell):

```
.details[data-surface="media"][data-page="details"] (+ .details--game-fallback)
  .details-bg · .details-scrim · .details-back.btn
  .details-body > .details-head > .details-info
    .details-kind · .details-title-logo | .details-title · .details-sub > .details-chip.chip-*
    .game-stats > .game-stat > .game-stat-label
    .details-desc · .details-quick-actions > .details-round-action(.active) > .round-action-icon
  .details-box-panel > .details-box-art
  .details-actions > .row-item.action-button(.disabled)
  .resume-btn > .resume-play, .resume-label, .resume-detail
  .episodes > .season-tabs > .season-tab(.season-tab-active) ; .ep-list > .ep-item > .ep-num, .ep-text > .ep-title, .ep-overview
  .streams > .streams-head, .source-picker > .best-source > .best-play, .best-text > .best-line > .best-label,
             .source-chip, .stream-badge.badge-(direct|youtube|external|torrent), .best-detail
           , .source-toggle, .stream-list > .stream-item > .stream-badge, .stream-text > .stream-name > .best-tag, .stream-detail
  .details-facts > .details-fact-panel > .details-facts-head, .details-facts-list > .fact-row ; .details-tags > .details-tag
  .shots · .details-hint

.collection-page.page-layer > .page-header > .round-back, (.eyebrow, h1, p) ; .collection-grid > .card
.downloads-page.page-layer > .page-header ; .downloads-content > .apple-empty | .download-group > h2 > span,
  .download-list > .download-card.download-(running|failed|done) > .download-symbol,
  .download-copy > .download-title-line, .apple-progress > span, .download-cancel
.search-scrim[data-surface="canvas"] > .search > .search-input, .search-busy > .loading-bar > .loading-bar-fill,
  .search-top > .osk > .osk-row > .osk-key(.osk-key-wide, .osk-key-accent), .search-hint > .key, .key
  .search-grid | .search-sections > .search-section > .search-section-head > .search-section-count, .search-section-strip
  .search-card > .search-art, .search-card-title
.settings-* : .settings-nav-list > .settings-nav-item(.settings-nav-active) > .settings-nav-icon,
  .settings-nav-label, .settings-nav-detail, .settings-nav-enter ; .settings-version-bar
  .settings-detail-pane > .settings-detail-head > .settings-detail-back, .settings-kicker, h2 ; .settings-detail-scroll
  .settings-section > .settings-section-heading, .settings-muted, .setting-row > strong/small/b,
  .segmented-setting > button(.selected), .accent-grid > .accent-swatch(.active), .status-pill, .save-indicator
  .osk-scrim > .osk-modal > .osk-modal-label, .osk-modal-value > .osk-caret, .osk-row-actions
.launch-layer[data-surface="media"][data-player-state="starting"|"failed"] (+ .launch-layer--failed)
  .launch-art · .launch-content > .launch-spinner, .launch-kicker, h2, p
  .recovery-dialog > .recovery-icon, h2, p, .recovery-actions > button[data-primary]
.home-loading > .skeleton.skeleton-hero, .skeleton-copy > span, .skeleton-row > span
.welcome-home > .welcome-glow, .welcome-mark, .eyebrow, h1, .welcome-copy, .apple-button.primary, .setup-features > span
.destination-empty > .empty-icon, h2, p, .apple-button.primary
.screen-message > p, .tv-retry
```

## Screen inventory — design every one of these

### 1. Home shell (six destinations: For you · Live · Movies · Shows · Games · Library)

Identical structure, different content. A full-bleed **hero** (backdrop of the currently focused item, scrimmed left and bottom, with kind eyebrow, title-treatment logo *or* text title, a `·`-joined meta line of release/runtime/rating/genres, a 2–3 line synopsis, and a "Press A / Enter to open" hint that appears only when focus is in the rows), with **shelves** of cards below it that scroll under the hero. The hero updates as focus moves, so the transition between backdrops must be handled — design it as a deliberate cross-fade, not a flash.

Design each destination's identity through content and density, not through different chrome. Show at least: **For you** (mixed kinds), **Live** (channel-logo tiles + fixture cards), **Games** (game cards + the source-notice banner), **Library** (Continue watching + Owned games).

### 2. Card system — the single most important object

One landscape 16:9 card is the default across every shelf, grid, and strip, so the whole shell reads as one grid. Design all variants:

- **artwork** — cover-cropped 16:9 backdrop/banner/thumbnail, title + subtitle below the thumb.
- **channel-logo** (`.card-thumb--logo`) — a *contained* logo on a deterministic per-channel gradient tint. Logo-less channels fall back to a two-letter **monogram** on that tint; it must look intentional, never like a broken image.
- **game** (`.card--game`) — portrait key art preferred, landscape banner contained as fallback, gradient tint underneath.
- **Badges**: `LIVE` (uses `--status-live`, and only this element may pulse), `PLAY`, `OWNED`, `BUY`, `UPCOMING`, `INSTALL`. They must be readable over any artwork.
- **Install progress**: a percentage badge plus a bottom-edge progress fill on the thumb.
- **Subtitle lines** are terse and generated: `Installed · STEAM`, `Buy · GOG`, `On Sky Sports · Starts in 2h`, `Live channel`, `Movie`, `TV Show`.
- **Focus**: ring + lift + shadow, and it must survive against both a bright artwork and a black one. Reserve the vertical padding so the lift never clips against the shelf above or below.

### 3. Top navigation

Two states. **Dormant**: minimal, non-competing, focus lives in the content. **Engaged** (`.top-nav-engaged`, summoned by Back/Menu or by pressing Left at the leading edge, and the shell gets `.nav-is-open`): profile orb, six destination labels, three icon-only utilities (Search, Downloads, Settings) with an optional count badge — plus an error-tinted badge variant when a download failed. Also design the **profile switcher** popover ("Who's watching?"): colored initial orbs, a check on the active profile, and a "Manage profiles" row.

### 4. Details page

The richest screen. It's a media surface: backdrop, scrim, and a stacked body that scrolls. Design it for all four content types, plus these parts:

- **Header**: kind eyebrow, logo or title, chips (rating/year/runtime/genres), synopsis.
- **Game stats** (games only): Played time, Story / Story + extras / Completionist times, Achievements.
- **Quick actions**: round icon buttons — watched, add, like, dislike — with an `.active` state.
- **Box art panel**: portrait key art to the side.
- **Primary action**: Play / Install / "Not available yet" (disabled).
- **Resume**: a distinct wide button — play glyph, "Resume", `12:34 · same source`.
- **Episodes** (series): season tabs + an episode list with number, title, and overview.
- **Source picker**: a "best source" hero row (play glyph, "Play now", quality chips, a kind badge — `direct` / `youtube` / `external` / `torrent`), a toggle to expand, and the full stream list with a `BEST` tag on the first.
- **Facts panels**: a definition list ("More information") and a tag cloud.
- **Screenshots** strip.
- States: **loading** ("Loading…"), **metadata failed** (with "Press B / Esc to go back"), **no sources found**.

### 5. Collection page

"See All" from a shelf title. Header with a `COLLECTION` eyebrow, the row title, and an "N titles" count; back affordance; a uniform grid of the same cards, virtualizing as you go.

### 6. Search

A full overlay. Two phases:

- **Keyboard phase**: a controller-driven on-screen keyboard (letter grid + wide action keys, one accent key for "search everything"), the query field, and a hint explaining that Enter searches the *entire* space — actors, themes, "k drama", "time travel" — not just titles.
- **Results phase**: sectioned results with per-section counts and horizontal strips, plus a busy state (indeterminate loading bar), a no-match hint, and the compact `.search-card`.

Also design the standalone **OSK modal** used by Settings for text entry: label, value with a blinking caret, masked mode for secrets, action row, and a `A Type · B/Esc Cancel · Done to save` legend.

### 7. Downloads

**Page**: header, and three groups — Active, Needs attention, Completed — each a focusable card with a status symbol, title, percentage/`Failed`/`Complete`, a detail line, a progress bar, and a cancel button on running jobs. Plus the empty state.
**Floating panel**: the small persistent bottom-corner progress summary shown on home while jobs run.

### 8. Settings

A two-pane panel: a left category list (Profiles · Accounts & tracking · Games & libraries · Video & Live TV · Playback · Appearance · Display · Accessibility · System & diagnostics — each with icon, label, and a one-line detail), and a right detail pane with a kicker + heading and scrolling sections. Design the full control kit, all of it focus-only:

- **Setting row**: strong label, small explanatory line, value on the right.
- **Segmented control** (Enhance: Auto / Quality / Performance / Off).
- **Preference toggle**: title, detail, on/off.
- **Accent swatch grid** with a check on the active swatch.
- **Status pill**, **save indicator** (idle / saving / saved / error), **source health** rows.
- Secret fields (Steam key, TMDB key, tokens) shown as configured/not-configured rather than as raw values.

### 9. System states — design all of them, they are not afterthoughts

- **Loading home**: skeleton hero + copy lines + a card row. It must feel like the product, not like a wireframe.
- **First run / Welcome**: `WELCOME TO TV OS`, "Everything you love, ready from the sofa.", a glow, a sparkle mark, a "Set up TV OS" button, and three feature chips (Games & retro · Movies & shows · Live channels).
- **Empty destination**: icon, "No games here yet", the tab's specific copy, "Open Settings".
- **Error / offline**: "You're offline. Check the network connection and try again." with a focused **Retry**.
- **Game source notice**: an inline banner — "Some game libraries need attention" — with reasons and an "Open Settings" action.
- **Launch layer — starting**: blurred art of the item, spinner, "Starting live playback", title, "Connecting to the stream…".
- **Launch layer — failed**: a recovery dialog — "Couldn't start playback", the reason, and Retry / Open Details / Cancel with the primary pre-focused.
- **Toast**: transient, bottom, for "Switched to Alex", "Light mode", "Enhance: Quality".

### 10. Input legend

Design the `.key` chip used inline in hints. The real bindings are: **A / Enter** confirm · **B / Esc / Backspace** back · **X** cycle enhance · **Y / T** toggle theme · **Start / S** open navigation · **/** search · d-pad or left stick to move.

## How to judge your own work

- Take a card, a nav item, a settings toggle, and a keyboard key. Their focus states must clearly belong to one family, and each must be legible from 10 feet against the worst-case background.
- Set the accent to amber and to pink. Nothing may become illegible; nothing may look broken.
- Switch to light. It must look designed for daylight, not like dark with the colors flipped.
- Turn on `data-reduce-transparency` and `data-high-contrast`. The layout must not shift; only the materials change.
- Cover an item's artwork with a plain black rectangle, then a plain white one. Titles, badges, and scrims must still hold.
- If a screen only looks good with perfect artwork and a long synopsis, it is not finished — the fallbacks are the design.

## Do not

- Do not invent features, tabs, or data the daemon doesn't serve. The inventory above is the whole product.
- Do not add hover styles, cursors, mouse affordances, or tooltips.
- Do not use decorative motion. Motion exists to explain focus movement and surface transitions; everything must respect reduced motion.
- Do not paint the accent everywhere, and never hardcode it.
- Do not use external fonts, images, icon libraries, or CSS frameworks.
- Do not copy any real platform's logos, wordmarks, or icon set.
