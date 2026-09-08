<p align="center">
  <img src="assets/logo.png" width="150" height="150" alt="Haucet" />
</p>


<h1 align="center">Haucet</h1>

Haucet is a Rust CLI and desktop GUI for Huawei HarmonyOS firmware analysis and maintenance. It unpacks HarmonyOS update packages, extracts ext2/ext4 partitions, inspects and rebuilds EROFS and ramdisk images, edits CPIO archives and NVE data, inspects OEMINFO and partition metadata, flashes images via Fastboot, and uploads loaders via HiSilicon VCOM.

## Examples

```sh
haucet unpack update.bin --out images
haucet unpack UPDATE.APP --list
haucet unpack UPDATE.APP --out app-work --partition boot --partition super
haucet unpack update_full_base.zip --out work
haucet unpack update_full_base.zip --out work --partition system --partition vendor
haucet unpack update_full_base.zip --out work --all-erofs
haucet erofs unpack system.img --out system-work
haucet erofs repack system-work --output new-system.img
haucet ext4 unpack preload.img --out preload-work
haucet partition-info ramdisk.img # also system.img
haucet ramdisk unpack ramdisk.img --out ramdisk-work
haucet ramdisk repack ramdisk-work ramdisk.img --out new-ramdisk.img
haucet partition-info rvt.img
haucet partition-info ptable.img
haucet oeminfo oeminfo.img
haucet oeminfo oeminfo.img --preview
haucet cpio ramdisk.cpio ls --recursive /
haucet fastboot devices
haucet fastboot flash updater updater_vendor.img
haucet fastboot getvar product
haucet fastboot extract-part oeminfo oeminfo.img
haucet fastboot reboot
haucet fastboot oem device-info
haucet vcom devices
haucet vcom flash COM3 0x80000000 loader.bin
```

## Build

```sh
cargo build --release
```

## Signing Warning

Repacking changes filesystem or ramdisk bytes. `haucet` preserves the original HVB certificate but cannot cryptographically re-sign it without the device/vendor signing key. A rebuilt image may be rejected by secure boot even when its filesystem and wrapper are structurally valid.

The initial release rebuilds partition images. It does not create a newly signed `update.bin` or `update_full_base.zip`.

## References

- [boardswarm/fastboot-rs](https://github.com/boardswarm/fastboot-rs) - upstream of the Rust fastboot implementation used by `hm-fastboot`.
- [sekaiacg/erofs-tools](https://github.com/sekaiacg/erofs-tools) and [erofs/erofs-utils](https://github.com/erofs/erofs-utils) - EROFS extraction/repacking tools and filesystem implementation.
- [ext4-view](https://github.com/nicholasbishop/ext4-view-rs) - pure Rust read-only ext2/ext4 filesystem parser.
- [ext4-rs](https://github.com/FauxFaux/ext4-rs) - pure Rust ext4 inode and extended-attribute parser.
- [kitsuned/Potato.ImageFlasher](https://github.com/kitsuned/Potato.ImageFlasher) - image-flashing workflow reference reimplemented in Rust.
- [ljlVink/ramdisk-tools](https://github.com/ljlVink/ramdisk-tools) and [topjohnwu/Magisk](https://github.com/topjohnwu/Magisk) - ramdisk formats and the `init_early` patch layout.
- [OpenHarmony update_packaging_tools](https://gitcode.com/openharmony/update_packaging_tools) - HarmonyOS/OpenHarmony update package format behavior.
- [OpenHarmony startup_hvb](https://gitcode.com/openharmony/startup_hvb) - HVB header, certificate, and footer format behavior.
- [R0rt1z2/hisi-nve](https://github.com/R0rt1z2/hisi-nve) - Huawei NVE layout and update behavior.
- [ud3v0id/huawei-oeminfo-tool](https://github.com/ud3v0id/huawei-oeminfo-tool) - OEMINFO block layout and payload classification reference.
- [Huawei HarmonyOS Sans](https://developer.huawei.com/consumer/cn/design/resource/) ([archived source package](https://github.com/ajacocks/harmonyos-sans-font)) - GUI font; its license is separate from the program license.

## License

The workspace is distributed under `GPL-3.0-only`. See [LICENSE](LICENSE) for
the project license.

Copyright (C) 2026 ljlVink.

Bundled tools, fonts, adapted upstream code, and Rust dependencies retain
their own licenses. See [THIRD_PARTY_NOTICES.md](LICENSES/THIRD_PARTY_NOTICES.md) and
the accompanying texts in [LICENSES/](LICENSES/). The root project license
does not relicense those third-party components.
