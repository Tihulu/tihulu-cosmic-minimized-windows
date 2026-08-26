# v0.4.0

First stable release of Tihulu COSMIC Minimized Windows.

Highlights:
- stability-first minimized-window grouping for COSMIC
- external `tihulu-previewd` preview daemon with bounded capture/cache behavior
- external `tihulu-mediad` MPRIS daemon with track metadata, artwork, controls, volume and seek buttons
- click/pinned rich previews with compact per-window fallback
- Safe Core fallback mode
- manual `Reload backends` recovery action in Settings
- hover remains experimental and disabled by default
- verified real-COSMIC runtime recovery without restarting `cosmic-panel`

Runtime validation included daemon stop/reload recovery for both preview and media backends while the `cosmic-panel` PID remained unchanged.
