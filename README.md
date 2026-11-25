# Glow

<img src="macos/icon.png" width="96" align="right" alt="Glow icon">

Make your Mac screen brighter than it normally goes, and dim all your screens together. Glow lives in the menu bar.

## What it does

- **Brighter than 100%.** On Macs with an XDR or HDR screen, slide past 100% for extra brightness.
- **Dim everything together.** Turn down your laptop and external monitors with one slider.
- **Your brightness keys still work.** Turn on the switch at the top of the panel, and pressing brightness-up at full brightness goes brighter. It is off by default, so nothing changes until you choose it. Handy in a bright room.

## What it looks like

Click the sun in the menu bar. A small panel opens under it:

- A big percentage, such as **150%**.
- One slider from 0% to 200%.
- A list of your screens, each with its current state.
- Two switches: **Sync all displays** and **Show on change**.
- A **Quit** button. Quitting puts every screen back to normal.

Right-click the sun for a short menu.

## Requirements

- macOS 13 or later.
- An XDR or HDR screen for the extra brightness. Dimming works on any display.

## Install

1. Download `Glow.zip` from [Releases](https://github.com/JieGH/glow/releases) and unzip it.
2. Move `Glow.app` to `/Applications`.
3. Glow is ad-hoc signed, not notarized, so macOS blocks the first launch. To open it anyway,
   try to open it once, then go to **System Settings → Privacy & Security** and click
   **Open Anyway**. Or remove the download flag in Terminal:
   `xattr -dr com.apple.quarantine /Applications/Glow.app`

## Use

- **Click** the sun to open the panel.
- **Right-click** the sun for the menu.

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
