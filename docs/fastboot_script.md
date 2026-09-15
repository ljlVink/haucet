# Haucet Flash Script Authoring Guide

[中文版 (Chinese version)](fastboot_script_cn.md)

A Haucet flash script (`haucet-flash.json`) is a declarative JSON document that describes a complete flashing session: HiSilicon VCOM loader uploads, fastboot partition flashes, waits, and user alerts, executed in order. The same script works in the GUI (One-click flash page) and the CLI (`haucet flash-script run`).

## Basic structure

```json
{
  "version": 1,
  "name": "My flashing session",
  "steps": [ ... ]
}
```

- `version`: currently fixed at `1`.
- `name`: optional, display only.
- `steps`: an ordered array, at least 1 and at most 256 steps. Execution is fail-fast — the first failing step aborts the run, and the error message carries the step number.
- Every file path resolves **relative to the directory containing the script**; absolute paths are used as-is. Missing files are reported during validation, before any device is touched.

## Step types

### wait_vcom — wait for a VCOM serial port

```json
{ "type": "wait_vcom", "timeout_secs": 60 }
```

Blocks until **at least one** VCOM serial port is present (DBAdapter / USB COM / PCUI, ...). Put this first if the cable is not plugged in yet.

### vcom_upload — upload a loader

```json
{ "type": "vcom_upload", "port": "auto", "address": "0x00023000", "file": "loader/usbldr.bin" }
```

- `port`: `auto` resolves automatically — a single port is used directly; with several ports execution pauses and the GUI shows a selection dialog while the CLI prompts for a number. A fixed name such as `"COM7"` also works (a missing port fails and lists what is available).
- `address`: a 32-bit hexadecimal load address, e.g. `0x80000000`, `0x00023000`.
- `file`: the loader file, relative to the script directory.
- The CLI renders a progress bar during upload; the GUI logs byte progress.

### wait_fastboot — wait for a fastboot device

```json
{ "type": "wait_fastboot", "timeout_secs": 30 }
```

Blocks until **exactly one** fastboot device enumerates. Use it after a VCOM loader makes the device re-enumerate as fastboot. Multiple fastboot devices abort immediately (connect only one).

### fastboot_assert — device check (strongly recommended first)

```json
{ "type": "fastboot_assert", "variable": "product", "value": "ABC" }
```

Reads `getvar <variable>` and aborts unless it equals `value`. Place it before any flash step so images cannot reach the wrong device model. Common variables: `product`, `serialno`.

### fastboot_flash — flash a partition

```json
{ "type": "fastboot_flash", "partition": "boot", "file": "images/boot.img" }
```

Uses the Ultraflash protocol when the device supports it and otherwise falls back to standard download/flash with automatic Android sparse splitting — identical to `haucet fastboot flash`, no configuration needed.

### fastboot_erase — erase a partition

```json
{ "type": "fastboot_erase", "partition": "userdata" }
```

### fastboot_oem — OEM command

```json
{ "type": "fastboot_oem", "command": "device-info" }
```

### fastboot_reboot — reboot / continue

```json
{ "type": "fastboot_reboot", "mode": "system" }
```

`mode` is one of `system`, `bootloader`, `fastboot`, `recovery`, or `continue`.

### alert — notify and pause

```json
{ "type": "alert", "message": "Re-plug the cable, then confirm" }
```

Shows the message and pauses: the GUI opens an in-app dialog and continues after you press Confirm; the CLI prints the message and continues after 5 seconds. Use it wherever human intervention is needed (re-plugging, cable swaps, entering a specific mode).

### sleep — fixed wait

```json
{ "type": "sleep", "millis": 500 }
```

## Complete example: VCOM boot chain + fastboot flash

```json
{
  "version": 1,
  "name": "Typical rescue session",
  "steps": [
    { "type": "wait_vcom", "timeout_secs": 60 },
    { "type": "vcom_upload", "port": "auto", "address": "0x00023000", "file": "loader/sec_usb_preloader.img" },
    { "type": "vcom_upload", "port": "auto", "address": "0x00300000", "file": "loader/sec_usb_xloader.img" },
    { "type": "alert", "message": "waiting to change low-level fastboot mode!" },
    { "type": "wait_fastboot", "timeout_secs": 60 },
    { "type": "fastboot_assert", "variable": "product", "value": "YOUR_PRODUCT" },
    { "type": "fastboot_flash", "partition": "fw_dtb", "file": "loader/sec_fwdtb.img" },
    { "type": "fastboot_flash", "partition": "teeos", "file": "loader/sec_trustedcore.img" },
    { "type": "fastboot_flash", "partition": "fastboot", "file": "loader/sec_BL33_AP_UEFI.fd" },
    { "type": "fastboot_reboot", "mode": "system" }
  ]
}
```

## Usage

**GUI**: One-click flash page → pick the script (validated automatically; missing files or malformed fields are reported immediately) → Run → confirmation dialog → execute. Step progress is shown while running; alerts and multi-port selection open dialogs. Cancel terminates at any time.

**CLI**:

```
haucet flash-script run script.json
```

The script is validated before execution (relative paths, file existence, field values); any failing step is reported with a `step N/M (kind)` prefix.

## Authoring notes

1. **Start fastboot sessions with `fastboot_assert`**; start VCOM sessions with `wait_vcom`.
2. **Add `wait_vcom` / `wait_fastboot` wherever the device re-enumerates** instead of hard-coded delays; reserve `sleep` for genuinely short pauses.
3. Every fastboot step opens the device independently — after a reboot or re-enumeration step just write the next step; connections are never reused.
4. **Risks**: flashing can erase data or brick a device; `fastboot_assert` is the last gate when files do not match the device. Repacked images are not re-signed with the vendor key, so secure-boot devices may reject them — that is a signing-chain limitation, independent of the script.
5. Cancelling mid-VCOM-upload may require manually re-entering the download mode before rerunning; cancelling during fastboot usually just means rerun — flashing is idempotent.
