# soundAr Design System

## For Claude Code: apply these specs across ALL screens and components

---

## 1. Design Principles

1. **Quiet confidence** — the UI should feel calm and precise, never busy. Every element earns its place.
2. **Content density without clutter** — show information efficiently but with enough whitespace to breathe.
3. **One accent, used sparingly** — blue is the action color. Everything else stays warm neutral.
4. **Flat surfaces** — no gradients, no drop shadows, no glow. Depth comes from layering background shades.
5. **Consistent rhythm** — 4px base grid. Every measurement is a multiple of 4.
6. **Warm light palette** — cream/off-white backgrounds, not pure white. Easy on the eyes.

Reference mood: Notion, Linear (light mode), Vercel dashboard.

---

## 2. Color Tokens

### 2.1 Full Palette

```python
COLORS = {
    # ── Backgrounds (layered warm cream palette) ──
    "bg_base":        "#f3f1ec",   # Sidebar — warm cream
    "bg_primary":     "#faf9f6",   # Main content area — off-white
    "bg_raised":      "#ffffff",   # Cards, model rows, elevated surfaces
    "bg_input":       "#f0efe9",   # Input fields, dropdowns, interactive controls
    "bg_hover":       "#eae8e3",   # Hover state on rows and cards
    "bg_active":      "#e2e0db",   # Active/pressed state, slider tracks

    # ── Borders ──
    "border_subtle":  "#e8e6e1",   # Faint separation lines, row dividers
    "border_default": "#d9d7d2",   # Default border on inputs, cards, containers
    "border_strong":  "#c4c2bd",   # Focused inputs, emphasized borders

    # ── Text hierarchy (use all five levels deliberately) ──
    "text_primary":   "#1a1a1a",   # Headings, model names, important values
    "text_secondary": "#4b5563",   # Body text, labels, dropdown values
    "text_tertiary":  "#6b7280",   # Metadata, GPU info, muted labels
    "text_ghost":     "#9ca3af",   # Placeholders, descriptions, timestamps
    "text_faint":     "#c4c2bd",   # File sizes, row footers, disabled text

    # ── Accent — Blue ──
    "accent":         "#3b82f6",   # Primary buttons, active nav indicator, links
    "accent_hover":   "#2563eb",   # Button hover state
    "accent_pressed": "#1d4ed8",   # Button pressed state
    "accent_muted":   "#eff6ff",   # Badge backgrounds, selection highlights
    "accent_text":    "#2563eb",   # Accent text on light bg, active nav text

    # ── Semantic colors (only for their specific meaning) ──
    "success":        "#16a34a",   # Downloaded, connected, healthy
    "success_muted":  "#f0fdf4",   # Success badge background
    "success_text":   "#15803d",   # Success badge text

    "warning":        "#d97706",   # VRAM warning, slow performance
    "warning_muted":  "#fffbeb",   # Warning badge background
    "warning_text":   "#b45309",   # Warning badge text

    "error":          "#dc2626",   # Failed downloads, OOM, critical errors
    "error_muted":    "#fef2f2",   # Error badge background
    "error_text":     "#b91c1c",   # Error badge text

    # ── Task type badge colors ──
    "stt_badge_bg":   "#eff6ff",   # Light blue tint for STT badges
    "stt_badge_text": "#2563eb",   # Blue text for STT badges
    "tts_badge_bg":   "#fffbeb",   # Light amber tint for TTS badges
    "tts_badge_text": "#b45309",   # Amber text for TTS badges

    # ── Sidebar specific ──
    "sidebar_bg":         "#f3f1ec",   # Sidebar background
    "sidebar_active_bg":  "#e8e6e1",   # Active nav item background
    "sidebar_text":       "#6b7280",   # Nav item text default
    "sidebar_active_text":"#2563eb",   # Nav item text active (blue)
}
```

### 2.2 Color Usage Rules

- **Never hardcode hex in widget code.** Always reference token names from the COLORS dict.
- **Layered depth model:** sidebar on `bg_base` (warm cream), content area on `bg_primary` (off-white), cards and rows on `bg_raised` (white). This three-layer system creates depth without shadows.
- **Blue accent is for actions only:** primary buttons, active sidebar indicator, clickable links, progress bar fills. Never as a decorative background.
- **STT = blue badge, TTS = amber badge.** These use solid light-tinted backgrounds (`accent_muted` / `warning_muted`), not transparent overlays.
- **Semantic colors are reserved for their meanings.** Green = success/installed. Amber = warning. Red = error/danger.

### 2.3 Text Hierarchy Guide

Five levels, each with a specific role:

| Level | Token | Hex | Use it for |
|-------|-------|-----|------------|
| 1 | `text_primary` | #1a1a1a | Page titles, model names, transcript text, important values |
| 2 | `text_secondary` | #4b5563 | Body labels, dropdown text, control labels, secondary info |
| 3 | `text_tertiary` | #6b7280 | GPU stats, metric values, muted metadata |
| 4 | `text_ghost` | #9ca3af | Input placeholders, descriptions, timestamps |
| 5 | `text_faint` | #c4c2bd | File sizes, footer text, disabled text |

---

## 3. Typography

### 3.1 Font Stack

```python
FONT_FAMILY = '-apple-system, "Inter", "SF Pro Display", "Segoe UI", sans-serif'
FONT_MONO   = '"JetBrains Mono", "SF Mono", "Fira Code", "Cascadia Code", monospace'
```

### 3.2 Type Scale

| Token | Size | Weight | Line Height | When to use |
|-------|------|--------|-------------|-------------|
| `title_lg` | 20px | 500 | 28px | Page titles: "Model hub", "Speech to text" |
| `title_sm` | 16px | 500 | 24px | Section headers, dialog titles, card group labels |
| `body` | 14px | 400 | 20px | General body text, transcript content |
| `body_medium` | 14px | 500 | 20px | Model names in lists, emphasized inline text |
| `caption` | 13px | 400 | 18px | Search input text, dropdown text, nav labels |
| `detail` | 12px | 400 | 16px | Metadata lines, descriptions, progress labels, button text |
| `micro` | 11px | 400 | 14px | Badge text, status bar text, timestamps, footer counts |
| `mono` | 12px | 400 | 16px | File paths, model IDs, metric readouts, VRAM values |

### 3.3 Typography Rules

- **Two font weights only: 400 (regular) and 500 (medium).** Never use 600, 700, or bold.
- **Sentence case everywhere.** "Model hub" not "Model Hub". Never ALL CAPS except for section labels in sidebar ("NAVIGATION", "SYSTEM") and acronyms ("STT", "TTS").
- **No mid-sentence bold.** Medium weight (500) is for standalone labels and model names.
- **Model names** are always `body_medium` (14px, weight 500).
- **Metadata lines** below model names are always `detail` (12px, weight 400, `text_ghost` color).

---

## 4. Spacing System

Base unit: **4px**. Every spacing value is a multiple of 4.

| Token | Value | Use case |
|-------|-------|----------|
| `xs` | 4px | Gap between badge and adjacent text, between icon and its label |
| `sm` | 8px | Between inline elements, vertical pill padding |
| `md` | 12px | Between rows in a group, between adjacent filter dropdowns |
| `base` | 16px | Standard card padding vertical, gap between card sections |
| `lg` | 20px | Card padding horizontal, gap between major inline groups |
| `xl` | 24px | Between page sections, between search bar and model list |
| `2xl` | 28px | Page title area to first content block |
| `3xl` | 32px | Content area left/right padding |

### 4.1 Key Spacing Rules

- **Content area padding:** 28px top, 32px left and right.
- **Model row internal padding:** 10px vertical, 20px horizontal.
- **Between model rows:** 1px hairline divider using `border_subtle`.
- **Between sections on any tab:** 24px vertical gap.
- **Sidebar nav items:** 42px height, 2px vertical gap between items.
- **Search bar to model list:** 20px.
- **Page title block to search bar:** 28px.

---

## 5. Component Specifications

### 5.1 App Shell Layout

```
┌─────────────────────────────────────────────────┐
│  Title bar (window controls)                     │
├──────────┬──────────────────────────────────────┤
│          │                                      │
│ SIDEBAR  │   Content area — bg_primary          │
│  200px   │                                      │
│          │   [Page title]        [GPU pill]      │
│ bg_base  │   [Subtitle]                         │
│ (cream)  │                                      │
│          │   [Search bar  | Task ▼ | Sort ▼]    │
│ Logo+    │                                      │
│ text     │   ┌────────────────────────────┐     │
│ labels   │   │ Row — bg_raised (white)    │     │
│          │   ├────────────────────────────┤     │
│ NAV      │   │ Row — bg_raised            │     │
│ items    │   ├────────────────────────────┤     │
│          │   │ Row — bg_raised            │     │
│          │   └────────────────────────────┘     │
│          │                                      │
│ SYSTEM   │   [Footer — text_ghost]              │
│ Settings │                                      │
└──────────┴──────────────────────────────────────┘

Window properties:
  Default size: 1400 × 900
  Minimum size: 1000 × 700
  Title: "soundAr | Local Speech Model Workbench"
```

### 5.2 Sidebar

```
Width: 200px (fixed, never collapses)
Background: bg_base (#f3f1ec) — warm cream
Right border: 1px solid border_subtle (#e8e6e1)

Logo row:
  36 × 36px gradient icon (blue gradient #2563eb → #60a5fa)
  Text: "sA", 15px, weight 600, white, centered
  App name: "soundAr", 16px, weight 500, text_primary
  Margin-bottom: 24px to first section label

Section labels:
  Font: 10px, weight 500, text_ghost (#9ca3af)
  Uppercase, letter-spacing 1px
  Padding-left: 16px
  Labels: "NAVIGATION", "SYSTEM"

Nav items:
  Height: 42px
  Padding-left: 16px
  Icon area: 20 × 20px (icons drawn centered in 18px logical box)
  Icon stroke: width 1.6, round cap, round join, fill none
  Text: 14px, weight 400, sidebar_text (#6b7280)
  Gap between icon and text: 12px
  Gap between items: 2px

  Default: text color sidebar_text, background transparent
  Hover: background bg_hover (#eae8e3), rounded 8px
  Active: background sidebar_active_bg (#e8e6e1), rounded 8px
         text color sidebar_active_text (#2563eb)
         3px blue accent bar on left edge
         text weight 500

Nav order:
  NAVIGATION section:
    1. Model hub (magnifying glass)
    2. Speech to text (microphone)
    3. Text to speech (speaker)
    4. Live transcription (waveform)
    5. Compare (split columns)
  --- flex spacer ---
  SYSTEM section:
    6. Settings (gear) — anchored to bottom
```

### 5.3 Page Header

```
Layout: flex row, space-between, align-items flex-start
Margin-bottom: 28px

Left:
  Title: 20px, weight 500, text_primary (#1a1a1a)
  Subtitle: 13px, weight 400, text_ghost (#9ca3af), margin-top 6px

Right:
  GPU status pill (see section 5.8)
```

### 5.4 Search Bar

```
Height: 38px
Background: bg_raised (#ffffff)
Border: 1px solid border_default (#d9d7d2)
Border-radius: 8px
Padding: 8px 14px
Font: 13px, weight 400

Placeholder: text_ghost (#9ca3af)
Input text: text_primary (#1a1a1a)
Selection highlight: #bfdbfe (light blue)

Focus state: border changes to accent (#3b82f6).
```

### 5.5 Filter Dropdowns

```
Height: 38px (set via QSS min-height and programmatically)
Min-width: 120px
Background: bg_raised (#ffffff)
Border: 1px solid border_default (#d9d7d2)
Border-radius: 8px
Padding: 8px 14px
Text: 13px, weight 400, text_secondary (#4b5563)
Chevron: 5px triangle, text_ghost (#9ca3af)

Hover: border_color changes to border_strong (#c4c2bd)

Dropdown popup:
  Background: bg_raised (#ffffff)
  Border: 1px solid border_default (#d9d7d2)
  Border-radius: 8px
  Item hover: accent_muted (#eff6ff)
  Selected item: text_primary color
```

### 5.6 Model Row

The primary repeating component. Used in Hub, model selectors, and settings.

```
Layout: flex row, align-items center, gap 16px
Background: bg_raised (#ffffff)
Padding: 10px 20px
Cursor: pointer
Hover: bg_hover (#eae8e3)

Structure: [Info area (flex:1)] [Details btn] [Action stack (Download btn | Inline progress)]

Info area:
  Line 1: flex row, align-items center, gap 8px
    Model name: 14px, weight 500, text_primary (#1a1a1a)
    Task badge: see section 5.7
    Optional status badge: "recommended"
  Line 2: margin-top 4px
    Metadata: 12px, weight 400, text_ghost (#9ca3af)
    Format: "{engine} · {languages} · {tier} · {summary}"

Action stack (QStackedWidget, fixed 28px height):
  Index 0 — Download/Installed button (see section 5.9)
  Index 1 — Inline progress: [bar 60px, 4px height] [pct label, 11px] [Cancel btn]
    Cancel button: transparent bg, error (#dc2626) text, 1px border, 11px font
    Cancel hover: border error, background error_muted (#fef2f2)

List container (wraps multiple rows):
  Background: bg_raised (#ffffff)
  Border-radius: 12px
  Border: 1px solid border_subtle (#e8e6e1)
  Rows separated by 1px hairline dividers (border_subtle)
```

### 5.7 Badges

Small categorical labels. Always small, never dominant.

**Task badges:**
```
Padding: 2px 10px
Border-radius: 4px
Font: 11px, weight 500

STT: background #eff6ff, color #2563eb
TTS: background #fffbeb, color #b45309
```

**Status badges:**
```
Same dimensions as task badges.

"recommended": background #f0fdf4, color #15803d
```

### 5.8 GPU Status Pill

Always visible in page header, top-right corner.

```
Padding: 6px 12px
Border-radius: 8px
Background: bg_raised (#ffffff)
Border: 1px solid border_default (#d9d7d2)
Font: 12px, weight 400, text_tertiary (#6b7280)
Layout: flex row, align-items center, gap 6px

Status dot: 8px circle, border-radius 50%
  Green (#16a34a): VRAM < 60%
  Amber (#d97706): VRAM 60–85%
  Red (#dc2626): VRAM > 85%

Text: "{GPU name} · {used} / {total} GB"
```

### 5.9 Buttons

**Primary (Download, Transcribe, Synthesize):**
```
Padding: 6px 16px
Border-radius: 6px
Background: accent (#3b82f6)
Border: none
Font: 12px, weight 500, color white (#ffffff)

Hover: accent_hover (#2563eb)
Pressed: accent_pressed (#1d4ed8)
Disabled: color text_faint (#c4c2bd), background bg_input (#f0efe9)
```

**Secondary (Details, Export):**
```
Padding: 6px 16px
Border-radius: 6px
Background: transparent
Border: 1px solid border_default (#d9d7d2)
Font: 12px, weight 400, text_secondary (#4b5563)

Hover: background bg_input (#f0efe9), border border_strong (#c4c2bd)
Pressed: background bg_active (#e2e0db)
```

**Installed indicator:**
```
Padding: 6px 16px
Border-radius: 6px
Background: success_muted (#f0fdf4)
Border: 1px solid border_default (#d9d7d2)
Font: 12px, weight 400, color success (#16a34a)
Text: "✓ Installed"
```

**Danger (Delete, Unload):**
```
Same as secondary but:
  Text: error (#dc2626)
  Hover: border error, background error_muted (#fef2f2)
```

**Large action button (Record, Transcribe, Synthesize — centered):**
```
Padding: 10px 28px
Border-radius: 8px
Background: accent (#3b82f6)
Font: 14px, weight 500, white
```

### 5.10 Progress Bar

```
Container width: ~180px

Track:
  Height: 4px
  Background: bg_active (#e2e0db)
  Border-radius: 2px

Fill:
  Background: accent (#3b82f6)
  Border-radius: 2px

Label below:
  Font: 11px, weight 400, text_tertiary (#6b7280)
  Margin-top: 6px
  Format: "{downloaded} / {total} GB"
```

### 5.11 Status Footer

```
Layout: flex row, space-between
Margin-top: 16px
Font: 11px, text_ghost (#9ca3af)
Left: "5 models · 1 installed"
```

---

## 6. PyQt6 QSS Stylesheet

Complete stylesheet in `ui/theme.py`. Apply via `app.setStyleSheet(get_theme_stylesheet())`.

See `ui/theme.py` for the full implementation — the stylesheet follows all specifications above with the warm cream light palette.

---

## 7. Icons

Inline-painted SVG-style strokes, no fill. No icon fonts, no image files.

```
Sidebar nav icons: 18px logical in 20px area, stroke-width 1.6, center-aligned
Inline icons: 14 × 14px, stroke-width 1.5
Badge icons: 12 × 12px, stroke-width 2.5
All: stroke-linecap round, stroke-linejoin round, fill none

Colors:
  Default: sidebar_text (#6b7280)
  Active: sidebar_active_text (#2563eb)
  Disabled: text_faint (#c4c2bd)

Required set:
  Nav:     search, microphone, speaker, waveform, columns, gear
  Actions: play, pause, stop, record, download, delete, copy, export,
           checkmark, chevron-down, close, refresh
```

---

## 8. Animations

Only these are allowed:

```
Hover: background-color + border-color, 150ms ease
Progress fill: width, 200ms ease
Loading skeleton: opacity 0.4 → 1.0, 1.5s ease-in-out (no spinners)
Everything else: instant (no page transitions, no slide-ins)
```

---

## 9. Screen Layouts

### Hub
```
[Header: "Model hub" + GPU pill]
[Search | Task filter | Sort filter]
[Model list container — rounded, white bg, hairline dividers]
[Footer]
```

### STT
```
[Header: "Speech to text" + GPU pill]
[Model | Language | VAD checkbox]
[Split: Left(drop zone, waveform, player, transcribe btn) | Right(transcript)]
```

### TTS
```
[Header: "Text to speech" + GPU pill]
[Model | Language | Speaker]
[Split: Left(text input, char count, synthesize btn) | Right(waveform, player, save)]
```

### Realtime
```
[Header: "Live transcription" + GPU pill]
[Model | Language | Mic device]
[Live waveform — full width 80px]
[Record/Stop — centered large]
[Transcript — full width expanding]
[Footer: Copy | Export | Clear + stats]
```

### Compare
```
[Header: "Compare models" + GPU pill]
[STT/TTS toggle]
[Input area — full width]
[Split: Column A(selector, result, metrics) | Column B(same)]
[Run comparison — centered large]
```

### Settings
```
[Header: "Settings"]
[Card: General settings]
[Card: Downloaded models table]
[Card: System info]
```

---

## 10. Widget Object Names

```python
# Labels
"title"         # 20px/500 page titles
"subtitle"      # 13px subtitle text
"sectionTitle"  # 16px/500 section headers
"metadata"      # 12px description text
"modelName"     # 14px/500 model names
"faint"         # 12px de-emphasized text
"accentLabel"   # 12px blue text
"successLabel"  # 12px green text
"errorLabel"    # 12px red text
"monoLabel"     # 12px monospace text

# Buttons
"primary"       # Blue filled action
"danger"        # Red text destructive action
"large"         # Large centered action

# Containers
"sidebar"       # Sidebar widget
"card"          # Card frame with raised bg
"modelRow"      # Model list row
```

---

## 11. Responsive Rules

```
Min window: 1000 × 700
Below 1200px: two-column layouts stack vertically
Sidebar: always 200px, never hides
Model names: truncate with "..." — never wrap
Waveform: stretches horizontally, fixed height (120px default, 80px realtime)
Transcript: stretches both directions, scrolls vertically
```
