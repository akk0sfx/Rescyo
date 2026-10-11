# One Slint interface everywhere; Liquid Glass only as the window material on macOS

The GUI is written once in Slint for every platform. On macOS 26 the sidebar and toolbar regions of the Slint window are transparent and the system's Liquid Glass (`NSGlassEffectView`, with `NSVisualEffectView` on macOS 11–15) is placed behind them; Windows 11 uses its Mica backdrop the same way. Controls are drawn by Slint in a glass-like style rather than as native AppKit glass controls. A separate SwiftUI shell for macOS was rejected: it would give fully native glass on every control, but double the interface code and its translations, and the Windows 7 software-rendered build would drift from the macOS one.

## Consequences

Glass appears on the window frame only; dense data (tables, hex, previews) always sits on opaque surfaces. Platform material code lives in a small per-OS module behind the GUI crate and falls back to solid tokens when transparency is reduced or unavailable.
