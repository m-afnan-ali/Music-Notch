# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [1.0.0] - 2026-11-16

First public release.

### Added
- Notch-style now-playing widget: cover art, title and artist, always on top, never steals focus.
- Hover to expand, showing previous / play-pause / next and a One UI 9-style timeline where translucent, album-colored hills sway with the music above the played part, with a ring knob at the playhead. A classic bar is also available.
- Collapsed view option: song name (scrolling when long, with a mini equalizer) or a live hills visualizer. Each new song shows its name for 4 seconds first.
- Visuals react to the real output level, read from Windows' endpoint peak meter. Nothing is recorded.
- Placement anywhere: a notch along the top, flush in a top corner, or along the bottom edge. In the lower half of the screen it expands upward.
- Drag to move, with snapping to edges, corners and the center. Multi-monitor and per-monitor DPI aware.
- Swipe left or right to skip when the notch is locked and hover-expand is off.
- Custom settings window: position, monitor, size, opacity, collapsed view, timeline style, behavior toggles, and start with Windows.
- System tray icon with quick actions, restored automatically if Explorer restarts.

### Performance
- Native Rust + Win32 + software Direct2D: about 11.5 MB of memory, and a ~350 KB executable.
- Event-driven media updates; the frame timer only runs while something moves; idle working-set trimming.
- Partial redraws: bars-only and scrolling-title frames repaint just their strip, and the visualizer is rasterized straight into the bitmap.
