# Haucet 刷机脚本编写教程

Haucet 刷机脚本(`haucet-flash.json`)是一份声明式 JSON 文档,描述一次完整的刷机流程:HiSilicon VCOM loader 上传、fastboot 分区刷写、等待、提醒等步骤按顺序执行。同一份脚本即可在 GUI(一键刷机页)和 CLI(`haucet flash-script run`)中使用。

## 基本结构

```json
{
  "version": 1,
  "name": "我的刷机流程",
  "steps": [ ... ]
}
```

- `version`:目前固定为 `1`。
- `name`:可选,仅用于显示。
- `steps`:步骤数组,至少 1 步,最多 256 步,按顺序执行,**任何一步失败立即中止**(错误信息会带步骤序号)。
- 脚本中所有文件路径相对于**脚本文件所在目录**解析;绝对路径原样使用。引用的文件不存在时,校验阶段就会报错,不会碰设备。

## 步骤类型

### wait_vcom — 等待 VCOM 串口

```json
{ "type": "wait_vcom", "timeout_secs": 60 }
```

阻塞直到系统出现**至少一个** VCOM 串口(DBAdapter / USB COM / PCUI 等)。插线前先跑这一步即可。

### vcom_upload — 上传 loader

```json
{ "type": "vcom_upload", "port": "auto", "address": "0x00023000", "file": "loader/usbldr.bin" }
```

- `port`:`auto` 表示自动选口——恰好一个口时直接用;多个口时暂停,GUI 弹窗让你选,CLI 列编号让你输。也可以写死口名,如 `"COM7"`(不存在会报错并列出可用口)。
- `address`:32 位十六进制加载地址,如 `0x80000000`、`0x00023000`。
- `file`:loader 文件,相对路径相对脚本目录。
- 上传过程 CLI 显示进度条,GUI 显示字节进度日志。

### wait_fastboot — 等待 fastboot 设备

```json
{ "type": "wait_fastboot", "timeout_secs": 30 }
```

阻塞直到枚举到**恰好一个** fastboot 设备。VCOM 上传完 loader、设备重枚举为 fastboot 后用它衔接。多个 fastboot 设备会直接报错(要求只连一台)。

### fastboot_assert — 设备校验(强烈推荐首步)

```json
{ "type": "fastboot_assert", "variable": "product", "value": "ABC" }
```

读取 `getvar <variable>`,与 `value` 不相等立即中止。放在刷写步骤之前,防止把镜像刷到错误型号的设备上。常用变量:`product`、`serialno`。

### fastboot_flash — 刷写分区

```json
{ "type": "fastboot_flash", "partition": "boot", "file": "images/boot.img" }
```

设备支持 Ultraflash 时自动走 Ultraflash 协议,否则自动回退标准 download/flash,Android sparse 镜像自动分片——与 `haucet fastboot flash` 命令行为一致,无需关心细节。

### fastboot_erase — 擦除分区

```json
{ "type": "fastboot_erase", "partition": "userdata" }
```

### fastboot_oem — OEM 命令

```json
{ "type": "fastboot_oem", "command": "device-info" }
```

### fastboot_reboot — 重启/继续

```json
{ "type": "fastboot_reboot", "mode": "system" }
```

`mode` 可选:`system`(重启进系统)、`bootloader`、`fastboot`、`recovery`、`continue`(继续开机)。

### alert — 提醒并暂停

```json
{ "type": "alert", "message": "请重新插线后确认" }
```

显示消息并暂停:GUI 弹应用内窗口,点「确认」后继续;CLI 打印消息等 5 秒自动继续。用于需要人工干预的节点(拔插、换线、进特定模式)。

### sleep — 固定等待

```json
{ "type": "sleep", "millis": 500 }
```

## 完整示例:VCOM 引导 + fastboot 刷写

```json
{
  "version": 1,
  "name": "典型救援流程",
  "steps": [
    { "type": "wait_vcom", "timeout_secs": 60 },
    { "type": "vcom_upload", "port": "auto", "address": "0x00023000", "file": "loader/sec_usb_preloader.img" },
    { "type": "vcom_upload", "port": "auto", "address": "0x00300000", "file": "loader/sec_usb_xloader.img" },
    { "type": "alert", "message": "waiting to change low-level fastboot mode!" },
    { "type": "wait_fastboot", "timeout_secs": 60 },
    { "type": "fastboot_assert", "variable": "product", "value": "你的设备型号" },
    { "type": "fastboot_flash", "partition": "fw_dtb", "file": "loader/sec_fwdtb.img" },
    { "type": "fastboot_flash", "partition": "teeos", "file": "loader/sec_trustedcore.img" },
    { "type": "fastboot_flash", "partition": "fastboot", "file": "loader/sec_BL33_AP_UEFI.fd" },
    { "type": "fastboot_reboot", "mode": "system" }
  ]
}
```

## 使用方式

**GUI**:一键刷机页 → 选择脚本(自动校验,文件缺失/格式错误会立即提示)→ 运行 → 确认弹窗 → 执行。执行中显示步骤进度;alert 与多口选择会弹窗等待。取消按钮随时可终止。

**CLI**:

```
haucet flash-script run 脚本.json
```

运行前自动校验(相对路径、文件存在性、字段合法性),任何一步失败都会以 `step N/M (类型)` 前缀报出。

## 编写建议与注意事项

1. **首步用 `fastboot_assert` 校验设备**,再开始刷写;VCOM 流程则先 `wait_vcom`。
2. **设备重枚举处加 `wait_vcom` / `wait_fastboot`**,不要依赖固定秒数;`sleep` 只用于确有必要的短暂间隔。
3. 每个 fastboot 步骤都是独立打开设备,重启/重枚举步骤之后无需(也不能)复用连接,直接写下一步即可。
4. **风险**:刷机可能清数据、变砖;文件与设备不匹配时 `fastboot_assert` 是最后一道闸。重打包(repack)过的镜像未经厂商密钥重签,安全启动设备可能拒绝启动——这与脚本本身无关,是签名链限制。
5. 中途取消(VCOM 上传中)可能需要手动让设备重新进入下载模式再重跑;fastboot 阶段取消通常直接重跑即可,刷写是幂等的。
