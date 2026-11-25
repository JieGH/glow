# Glow

A macOS menu-bar app that gives you one brightness control from 0% to 200%.

- **0–100%** is your Mac's normal brightness.
- **100–200%** boosts HDR-capable screens (such as MacBook Pro Liquid Retina XDR or Pro Display XDR) past their normal maximum.
- **Synced dimming** dims every connected display together, including external monitors without their own brightness control.
- **Brightness keys** keep working: press brightness-up at full brightness to go past 100%.

See [docs/how-it-works.md](docs/how-it-works.md) for details.

## Requirements

- macOS 13 or later.
- An HDR-capable screen for the boost. Synced dimming works on any display.

## Install

1. Download `Glow.zip` from [Releases](https://github.com/JieGH/glow/releases) and unzip it.
2. Move `Glow.app` to `/Applications`.
3. Glow is ad-hoc signed, not notarized, so macOS blocks the first launch. To open it anyway,
   try to open it once, then go to **System Settings → Privacy & Security** and click
   **Open Anyway**. Or remove the download flag in Terminal:
   `xattr -dr com.apple.quarantine /Applications/Glow.app`

## Use

- **Left click** the sun icon to open the controls.
- **Right click** for a menu with Boost, Display Sync and Quit.
- **Quit** restores every screen.

## Build from source

Install [Rust](https://rustup.rs), then:

```sh
cargo test
scripts/build-app.sh   # builds and ad-hoc signs dist/Glow.app
open dist/Glow.app
```

## A note on private APIs

Glow reads and sets the system brightness through `DisplayServices`, a private macOS framework.
Apple may change it in any macOS update, and it rules Glow out of the Mac App Store. If a macOS
update breaks brightness control, please open an issue.

## License

[MIT](LICENSE)
