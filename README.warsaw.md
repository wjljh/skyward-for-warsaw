# Skyward for the REDMI K90 Ultra (warsaw)

This is an unofficial fork of [kirklin/skyward](https://github.com/kirklin/skyward)
that brings the recovery up on the REDMI K90 Ultra (warsaw), which ships
HyperOS OS4.0.8.0.XHPCNXM. It is based on upstream c5e09f6.

## What was wrong, and what fixed it

| Symptom | Cause | Fix |
|---|---|---|
| The data partition never opens | The vendor eSE HAL only powers the secure element up inside init(), and Skyward asked isCardPresent() first | skyward-omapi: connect before asking |
| The secure element is still not found | ro.product.product.name was adopted from the installed system (miproduct_warsaw), and the vendor HAL then takes a model-specific path that cannot power the eSE up in a recovery | Keep the installed properties, but set ro.product.product.name to skyward |
| WLAN never comes up | The config asked for a wlan_mac.bin this phone does not have | Removed that firmware entry |
| The touchscreen answers nothing after the screen goes off | The panel is rested with the backlight off, and this phone's goodix controller does not come back from that in a recovery | Leave the panel on with the backlight off, and write the device's touch settings again when it comes back |
| The vendor HALs cannot load their libraries | The runtime APEX is not mounted and ld.config.txt has no [odm] section | A [[service]] one-shot unpacks the APEX, copies the libraries, writes ld.config.txt and sets up the nodes the HALs need |

## What this fork adds

- **Unlock first**: the device config's unlock_first opens on the decrypt page and holds every other page until the data partition opens.
- **Several packages at once**: choosing files in internal storage picks them for a queue; they install in turn, each with what became of it, and a package that fails leaves the rest a tap away.
- **Flash a partition image**: a page of the partitions an image goes into (boot, init_boot, vendor_boot, recovery, dtbo, vbmeta) before the image is looked for.
- **Back that knows where it is**: the bar's back goes to the page a screen belongs under, while a swipe still goes one step.
- **Backup completion** says what the backup takes on disk against what it holds.
- **About** names who adapted it, what was fixed, and opens the updater from the system version.

## Building

```sh
export ANDROID_NDK_HOME=/path/to/android-ndk-r29
export CARGO_HOME=/path/to/cargo RUSTUP_HOME=/path/to/rustup

cargo ndk -t arm64-v8a --platform 30 build --release -p skyward
cargo ndk -t arm64-v8a --platform 30 build --release -p skyward-system-init

./target/release/skyward-pack recovery \
  --stock stock-recovery.img \
  --binary target/aarch64-linux-android/release/skyward \
  --system-init target/system-init/aarch64-linux-android/release/system-init \
  --config devices/warsaw-4.0.8.0.toml \
  --font resources/fonts/MiSansVF.ttf \
  --mono resources/fonts/KirkMono-Regular.ttf \
  --mono-license resources/fonts/KirkMono-OFL.txt \
  --out skyward-warsaw.img
```

The image is 100 MiB and holds no kernel: it boots the boot partition's kernel,
as the phone's own recovery does.

## Flashing

```sh
fastboot flash recovery skyward-warsaw.img
fastboot reboot recovery
```

Only the recovery partition is written. Nothing else on the phone is touched.

## Known limitations

- **USB mass storage** is parked. This phone's data partition is
  credential-encrypted, so a computer reading it as a disk reads ciphertext,
  and a raw export has to unmount /data, which once left the file system
  damaged. Copy files with adb pull, or with the MTP switch in Settings.
- **Touch**: the driver is not reloaded when the touchscreen stops answering.
  Reloading it while the panel is in use leaves a kernel trace on this phone and
  a touchscreen that answers nothing, so Skyward only writes the diagnostic line
  to the kernel log.
- **MTP transfer progress** cannot be shown: MTP runs in the phone's own
  process, not in Skyward.

## Credits

Skyward is by [Kirk Lin](https://github.com/kirklin) and is GPL-3.0.
The warsaw adaptation is by wjljh and DeepSeek Harness.
