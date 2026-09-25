#let ink = rgb("#17212B")
#let muted = rgb("#64717D")
#let rule = rgb("#D9E0E5")
#let paper = rgb("#F5F7F8")
#let accent = rgb("#C43D32")
#let accent-soft = rgb("#FBEDEA")
#let teal = rgb("#16756F")
#let teal-soft = rgb("#E7F4F2")
#let amber = rgb("#A76514")
#let amber-soft = rgb("#FFF4DF")
#let code-bg = rgb("#1E2932")

#let authors = (
  "ljlVink",
  "gpt-5.6-sol-xhigh",
  "gpt-6-astra-xhigh",
  "deepseek-v4-flash-max",
)

#set document(title: "Huawei Fastboot", author: authors)

#set page(
  paper: "a4",
  margin: (x: 18mm, top: 20mm, bottom: 19mm),
  header: align(right)[
    #text(font: "Microsoft YaHei", size: 7.5pt, weight: "medium", fill: muted)[
      Haucet Document
    ]
  ],
  footer: context align(center)[
    #counter(page).display("1")
  ],
)
#set text(
  font: "Microsoft YaHei",
  size: 9.5pt,
  fill: ink,
  lang: "zh",
)
#set par(justify: true, leading: 0.72em)
#set heading(numbering: "1.1")
#set list(indent: 1.2em, body-indent: 0.55em, spacing: 0.45em)
#set table(inset: (x: 7pt, y: 6pt), stroke: rule)
#show table.cell: set par(justify: false)

#show raw: set text(
  font: ("Consolas", "Microsoft YaHei"),
  size: 8pt,
)

#show raw.where(block: true): it => block(
  width: 100%,
  fill: code-bg,
  stroke: none,
  radius: 4pt,
  inset: 10pt,
  above: 6pt,
  below: 10pt,
  text(fill: rgb("#EEF3F5"), it),
)

#show heading.where(level: 1): it => block(
  width: 100%,
  above: 18pt,
  below: 9pt,
  stroke: (bottom: 1.2pt + accent),
  inset: (bottom: 5pt),
)[
  #text(size: 17pt, weight: "bold", fill: ink)[#it.body]
]

#show heading.where(level: 2): it => block(
  above: 13pt,
  below: 6pt,
)[
  #text(size: 12pt, weight: "bold", fill: teal)[#it.body]
]

#show heading.where(level: 3): it => block(
  above: 9pt,
  below: 4pt,
)[
  #text(size: 10pt, weight: "bold", fill: ink)[#it.body]
]

#let tag(body, color: accent, background: accent-soft) = box(
  fill: background,
  radius: 2pt,
  inset: (x: 6pt, y: 2.5pt),
)[#text(size: 7.5pt, weight: "bold", fill: color)[#body]]

#let callout(title, body, kind: "info") = {
  let palette = if kind == "warn" {
    (amber, amber-soft)
  } else if kind == "danger" {
    (accent, accent-soft)
  } else {
    (teal, teal-soft)
  }
  block(
    width: 100%,
    breakable: false,
    fill: palette.at(1),
    radius: 3pt,
    inset: (x: 10pt, y: 8pt),
    above: 7pt,
    below: 9pt,
  )[
    #text(size: 8.5pt, weight: "bold", fill: palette.at(0))[#title]
    #v(3pt)
    #text(size: 8.7pt)[#body]
  ]
}

#let stat(value, label) = block(
  width: 100%,
  fill: paper,
  radius: 4pt,
  inset: 9pt,
)[
  #text(size: 13pt, weight: "bold", fill: accent)[#value]
  #v(2pt)
  #text(size: 7.5pt, fill: muted)[#label]
]

#align(left)[
  #v(11pt)
  #text(size: 30pt, weight: "bold", fill: ink)[Huawei Fastboot]
  #v(12pt)
  #line(length: 42mm, stroke: 3pt + accent)
  #v(12pt)
  #text(size: 9pt, fill: muted)[作者：#authors.join(", ")]
]

#v(15mm)

#callout(
  [Summary],
  [部分BD Firmware 私有Fastboot协议],
)

#pagebreak()

= 快速索引

#table(
  columns: (1.05fr, 2.2fr, 2.5fr),
  fill: (x, y) => if y == 0 { paper },
  table.header(
    [*能力*], [*线上命令*], [*用途*]
  ),
  [`oem`], [`oem <command...>`], [执行厂商扩展命令; 具体命令与实测结果见后文.],
  [`getvar`], [`getvar:<name>`], [查询标准或厂商扩展变量; 部分 `rescue_*` 变量可能产生副作用.],
  [`ultraflash`], [`ultraflash:<partition>`], [大分区流式刷写],
  [`upload_storage`], [`upload_storage:<offset>:<length>`], [从当前已选择的存储介质读取原始字节.],
  [`upload_memory`], [`upload_memory:<address>:<length>`], [按物理地址读取允许访问的内存区域.],
)

== Fastboot 响应模型

部分私有命令沿用 Fastboot 的四字节响应前缀, 但上传命令的数据阶段有一个关键区别：设备先回 `OKAY`, 随后发送调用方预先声明长度的裸数据, 最后再回一次 `OKAY`.

#table(
  columns: (0.8fr, 1.35fr, 3.3fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*前缀*], [*方向*], [*含义*]),
  [`INFO` / `TEXT`], [设备 → 主机], [过程消息; 主机应继续读取后续响应.],
  [`DATA`], [设备 → 主机], [标准下载握手, 后接八位十六进制长度.上传命令不使用它.],
  [`OKAY`], [设备 → 主机], [阶段成功.上传命令在裸数据前后各出现一次.],
  [`FAIL`], [设备 → 主机], [失败, 后续 ASCII 文本是原因, 例如 `Not Ready`.],
)


= ultraflash


`ultraflash` 是私有流式刷写状态.它把目标分区选择、标准 `download` 数据传输和显式收尾组合为一次会话, 适用于 `system`、`vendor` 等大镜像.目标不支持时退回标准 Fastboot `download` + `flash`.

固件的 ultraflash 会话不适用于 `oeminfo`, haucet 在协议层对该分区特判：不发送 `ultraflash:oeminfo`, 始终走标准 `download` + `flash` 路径.


#table(
  columns: (0.55fr, 2.2fr, 1.4fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*Step*], [*Send*], [*Resp*],),
  [1], [`ultraflash:<partition>`], [`OKAY`],
  [2], [`download:<8-hex-size>`], [`DATA<8-hex-size>`],
  [3], [镜像裸数据], [`OKAY`],
  [4], [`ultraflash`], [`OKAY`],
)

在 USB 2.0 环境下, 大分区刷写通常可比普通路径快约 20%-30%.


= upload_storage


该命令用于从 Fastboot 环境读取 UFS/eMMC 的原始范围.

必须先查询一个确定存在的分区：

```bash
haucet fastboot get-var storage:oeminfo
# storage:oeminfo: 0000000001000000:0000000006000000
```

#callout(
  [`getvar storage` 必须先于 `upload_storage` 调用],
  [设备端 `upload_storage` 不自己解析 GPT, 而是复用一个缓存的分区表条目（GptAdaEntry 全局变量）来选择读取介质.该缓存只在 `getvar storage:<name>` 触发 `FindPartition` 时才会被填充.未初始化时实测有两种表现：直接返回 `Not Ready`, 或者读到错误的 LUN（首次测试在未初始化状态下从偏移 0 读到的是 xloader 分区的证书链，而非 GPT）.初始化用的分区名只要真实存在即可， `oeminfo` 是常规选择；返回的范围值本身可以丢弃.],
)

=== 固件侧初始化原理

`FastbootApp.efi` 中 `CmdUploadStorage` 的读取路径：`getvar storage:<name>` 先经 GPT Adapter 协议（GUID `5347B303-75BC-4964-938C-D7CD740D42F4`）执行 `FindPartition(name)`, 把找到的 128 字节 GptAdaEntry（内含该分区所在 LUN 的 BlockIo/Media 信息）写入全局缓存；随后所有 `upload_storage:<offset>:<length>` 都通过该条目的 BlockIo 以 `LBA = offset / block_size` 读盘.

众所周知xloader一定不在同一个lun。所以`haucet fastboot analyse-storage`中是看不到xloader的。

=== 参数规则

#table(
  columns: (1.15fr, 1.5fr, 3.15fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*参数*], [*编码*], [*语义*]),
  [`offset`], [十六进制字节偏移], [相对于由 `getvar storage:<partition>` 选中的物理介质/LUN, 不是相对于该分区起点.],
  [`length`], [十六进制字节长度], [设备发送的裸数据字节数; 必须非零.当前 CLI 将其限制为 `u32`.],
)

设备端会按介质逻辑块大小计算 `LBA = offset / block_size`, 并处理块内余数.因此协议实现可读取非块对齐范围; 工程使用中仍建议保持块对齐, 并验证 `offset + length` 不越过目标介质.

== 举例: OEMINFO 完整读取


```bash
haucet fastboot get-var storage:oeminfo
haucet fastboot upload-storage 0x1000000:0x6000000 oeminfo.img
haucet oeminfo oeminfo.img
```


本机实测的 64 KiB 样本已被解析为两个 32 KiB bank, 并找到一个有效 `OEM_INFO` block.小样本只证明偏移与格式正确, 不代表包含完整 OEMINFO 数据.


== GPT 的 4 KiB 逻辑块

在初始化后的用户 LUN 上, 保护 MBR 位于 LBA 0（实测尾部 `55AA`, 其余字节全零）, GPT 主头位于字节偏移 `0x1000`.这意味着该介质的逻辑块大小是 4096 字节：

#table(
  columns: (1.2fr, 1.15fr, 1.5fr, 2fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*结构*], [*LBA*], [*字节偏移*], [*实测内容*]),
  [保护 MBR], [`0`], [`0x0000`], [仅 `0x55AA` 结束标记有效],
  [GPT Header], [`1`], [`0x1000`], [`EFI PART`, HeaderCRC/EntryCRC 校验通过],
  [Partition Entries], [`2`], [`0x2000`], [`128 × 128` 字节, 91 个已用],
  [空闲间隙], [`0xA`–`0x21`], [`0xA000`–`0x22000`], [全零, 至 first usable LBA],
)

```bash
haucet fastboot get-var storage:oeminfo
haucet fastboot upload-storage 0x0:0x30000 user-lun-head.bin
haucet fastboot analyse-storage   # 直接解析并打印分区表
```

若解析器把逻辑块固定为 512 字节, 它会错误地到 `0x400` 查找分区项, 从而报告“不是 GPT”或得到空表.解析 GPT 头时可用 `header_offset / current_lba` 推断块大小； `haucet` 的 `analyse-storage` 与 `partition-info` 均按此逻辑自动识别.


= upload_memory

该命令按地址读取内存.设备端解析 `ADDRESS:LENGTH`, 判断地址所属的安全类型, 再选择直接发送或通过固定缓冲区分块复制.它具有明显的固件和安全状态依赖性.

#callout(
  [实测范围与风险],
  [已实测根据 `oem ddrdump` 返回的地址和长度导出部分日志区域, 其他内存区域尚未逐项验证; 不正确的地址或长度仍可能导致设备重启.],
  kind: "warn",
)



```bash
haucet fastboot upload-memory 0x<address>:0x<length> memory.bin
```

Dump UEFI!!

```sh
fastboot upload-memory 0x3b400000:0x600000 UEFI
```

#table(
  columns: (1.35fr, 2.2fr, 2.3fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*检查*], [*设备端行为*], [*失败响应*]),
  [地址对齐], [起始地址必须至少 4 字节对齐.], [`FAILParams error`],
  [长度], [长度必须非零; 主机按声明长度收包.], [`FAILParams error`],
  [内存类型], [内部分类只接受固件认可的 secure / non-secure 路径.], [`FAILinvalid memory type!`],
  [中转缓冲], [non-secure 路径最多按 `0x1400000` 字节分块复制.], [可能提前结束并记录固件日志],
)

== 通过 ddrdump 获取内存范围

先执行 `haucet fastboot oem ddrdump`, 获取设备列出的内存区域.`base` 是起始地址, `size` 是字节长度, `mem` 是区域名称.将 `base:size` 作为 `upload-memory` 的参数, 即可尝试导出对应区域; CLI 使用连字符 `upload-memory`, 线上协议命令为 `upload_memory`.

```bash
haucet fastboot oem ddrdump
Using device PCIROOT(0)#PCI(1400)#USBROOT(0):13 ()
base:0x000000002FC20000, size:0x00020000 mem:bl31_log
base:0x0000000011B97000, size:0x0000C000 mem:hhee_log
base:0x0000000010954000, size:0x00010000 mem:bl2
base:0x0000000010900000, size:0x00040000 mem:fastbootlog
base:0x000000002F080000, size:0x00580000 mem:hifi_unsec_mem
base:0x000000001E000000, size:0x00E00000 mem:share_nsro
base:0x000000001EE00000, size:0x00200000 mem:share_unsec
base:0x000000001F000000, size:0x00400000 mem:modem_dump
base:0x00000000A0000000, size:0x0E100000 mem:modem_ddr
base:0x0000000012840000, size:0x000C0000 mem:lpmcu_image
OEM command completed

haucet.exe fastboot upload-memory 0x10900000:0x40000 fastbootlog.bin
Using device PCIROOT(0)#PCI(1400)#USBROOT(0):13 ()
Uploaded memory range 0x10900000:0x40000 to fastbootlog.bin (262144 bytes)
```

== BL33 运行时内存图

#table(
  columns: (1.55fr, 1.35fr, 2.7fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*范围*], [*内容*], [*说明*]),
  [`0x3B400000–0x3B5FC000`], [BL33 FV1（解密后）], [FFS2 `_FVH`, `FvLength` 4 MiB; `+0x00` 零向量区被替换为 AArch64 分支（华为定制入口）.头部 ~1.9 MiB: PrePi/SEC 模块 XIP（LzmaCustomDecompressLib、PrePi.c 等）+ LZMA 压缩的 FVMAIN; 其余至 4 MiB 为 `0xFF` 填充.],
  [`0x3B800000–0x3B840000`], [BL33 FV2], [FFS3 `_FVH`, `FvLength` 256 KiB.],
  [`0x3B840000–0x3BA00000`], [窗口尾部], [1.75 MiB 无字符串二进制, 非 PE/TE 镜像.],
  [`0x3BBE5000–0x3BD00000`], [*FastbootApp.efi 加载副本*], [`MZ` 头 + `.text` 与磁盘逐字节一致; 尺寸 `0x11B000`; 跨重启地址确定.旧资料“6 MiB dump 出模块树”对应的是非工厂 BL 状态, 工厂 BL 下本窗口为压缩态.],
  [`0x3BCDE000–0x3BCFE000`], [FastbootApp `.data`], [命令/变量注册模板表、getvar 响应缓冲等],
  [`0x3BA00000–0x3BE00000`], [DXE 堆（部分）], [`0x3BA00000–0x3BBE5000` 稀疏数据, 无其他 PE/TE 镜像.],
  [`0x50431000+`], [伪页表], [`0xAFAFAFAF`],
  [`0x50480000–0x507E0000`], [Runtime 驱动群], [ReportStatusCodeRouter / Capsule / Variable / Runtime / OpenPlatform 等已重定位的 EfiRuntime 镜像（MZ 页对齐）.],
  [`0x47C00000–0x50800000`], [*DXE 世界（解压 FV + 驱动镜像群 + 池）*], [密度 60–100%.解压 DXE FV 约 `0x47C00000–0x4A400000`; 已加载 MZ 镜像群簇分三片: `0x48CBF000–0x48ED7000`（含两个含 `FastbootCtrl` 串的镜像）、`0x4B000000–0x4B800000`、`0x4E000000–0x4E800000`.另有多份 FastbootApp 族模块，散布 `0x48b2xxxx`/`0x48c4xxxx`/`0x4ed9xxxx`/`0x4f9exxxx`, 全部可写但均为惰性副本.],
  [`0x50800000–0x8F000000`], [空 DRAM], [440 MiB 全量扫描无页表、无镜像, 仅零星稀疏数据.],
  [`0x0A000000–0x0F800000`], [空 DRAM], [全零, 可读.],
)

PrePi 侧依据（`PeiUniCore.te` XIP, 均可从 `0x3B400000` 窗口直接读出）:

- DDR 段表运行时全局 `@0x3B40E138`（计数 `@0x3B40E538`）: seg0 `0–0x50000000`, seg1 `0x50000000–0xE0000000`, seg2 `0x800000000–0x820000000`, seg3 `0x100000000–0x200000000`.
- 分配器水位全局 `@0x3B40E140` 附近: 自由区 `[0x50000000, 0x90000000)`; 分配自顶向下; 上下文挂 `TPIDR_EL0`（`sub_3B4073E0` = AllocatePages, `sub_3B40B958` = 读 TPIDR）.
- BL33 窗口 `0x3B400000+0x600000` 由 PrePi 从资源 HOB 中整段预留.

== 保护情况

#table(
  columns: (1.5fr, 0.9fr, 3.05fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*目标*], [*结果*], [*证据/机制*]),
  [RW DRAM / SRAM 写入], [#tag([成功], color: teal, background: teal-soft)], [`oem write 0x10CFC0@0x11451419`（SRAM, 回读一致）; `write__: 0.4` 4 × u32 写入 `.data` 响应缓冲 `0x3BCF2130`, 回读逐字节一致.],
  [镜像 `.text` 写入], [#tag([崩溃])], [GCC/EDK2 把 rodata 并入 `.text`; DxeCore 镜像保护将该段映射 RO.写会导致整机重启.读不受影响.],
  [改页表翻 AP 位], [#tag([不可行])], [真页表不在 NS 可见 DRAM（全量扫描证实, 见下行）; 且无 TLBI 原语 —— dTLB 旧 RO 表项不受描述符改写影响.],
  [真页表定位], [#tag([NS 不可见])], [全量扫描 `0x0A000000–0x8F000000`（含 440 MiB 逐块）未发现任何真页表; 推测位于 TZ/HHEE 保护域.页表翻转路线在 NS fastboot 上下文内彻底不可行.],
  [4 GB 以上], [#tag([NS 不可见])], [PrePi ctx 数学指向 `0x7F8000000+`; 实测首探即崩.],
  [惰性字符串副本改写], [#tag([无效])], [],
)


= Fastboot OEM / Getvar 命令

OEM 扩展命令与 Getvar 变量.

#callout(
  [命令格式],
  [OEM 命令使用 `haucet fastboot oem <command...>`; 变量查询使用 `haucet fastboot get-var <name>`.下表仅列出末尾的子命令或变量名.],
)

== OEM 命令速查

#table(
  columns: (2.05fr, 0.82fr, 2.75fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*OEM 子命令*], [*状态*], [*当前观测*]),
  [`ddrdump`], [#tag([实测], color: teal, background: teal-soft)], [列出内存区域的 `base`、`size` 和 `mem`; 已配合 `upload-memory` 导出 `fastbootlog`, 详见内存读取章节.],
  [`read`], [#tag([实测], color: teal, background: teal-soft)], [读取地址 `0x10CFC0`, 返回地址及其当前值; 详见地址读写实测.],
  [`write`], [#tag([实测], color: teal, background: teal-soft)], [向地址 `0x10CFC0` 写入 `0x11451419`, 随后通过 `read` 确认读回一致.],
  [`get-bsn`], [#tag([实测], color: teal, background: teal-soft)], [返回设备序列号（SN）.],
  [`get-sn`], [#tag([失败])], [设备返回错误, 未附带可用信息.],
  [`sram_dhry_stone`], [#tag([已响应], color: teal, background: teal-soft)], [返回 `OKAY`, 无附加输出; 实际测试效果仍需确认.],
  [`lock-state info`], [#tag([实测], color: teal, background: teal-soft)], [分别返回 Fastboot 锁与用户锁状态, 详见下页.],
  [`cert_key_info`], [#tag([实测], color: teal, background: teal-soft)], [返回 Flash cert key 与 Empower cert key 信息.],
  [`get-bootinfo`], [#tag([实测], color: teal, background: teal-soft)], [返回 `locked` 或 `unlocked`.],
  [`hwdog certify enc begin` / `hwdog certify close`], [#tag([WIP], color: amber, background: amber-soft)], [`hm-fastboot` 中的客户端支持尚未完成.],
  [`frp-unlock` / `frp-erase`], [#tag([?])], [涉及 FRP 状态修改.],
)

= oem read / write

#table(
  columns: (1.15fr, 1.85fr, 2.9fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*命令*], [*语法*], [*语义*]),
  [`oem read`], [`read <addr>`], [裸读该地址 1 × u32, 无任何分类器/白名单.],
  [`oem read` 批量], [`read <addr>@<size>`], [`@` 分隔地址与字节数.无 `@` 时 size 输出为 `-1` 并归零, 恰好 1 个 u32; 有 size 时每条 `INFO 0x%08x:` 行最多 4 个 `0x%08x`（16 字节）, 内层上限 `size <= 0 ? 1 : 4`, 地址递增 16, 取完回 `OKAY`.批量粒度 = 16 B/响应行.],
  [`oem write`], [`write <addr>@<value>`], [地址 u64, 值 u32.解析成功即先回 `OKAY` 再执行裸 `*(u32*)addr = value`; 值为负（最高位）报 `FAILInput param error.`],
)

OEM `read` / `write` 完成“读取原值 → 写入测试值 → 再次读取”的验证.地址 `0x10CFC0` 初始返回 `0x00000000`, 写入后返回 `0x11451419`; 不仅写入命令显示完成, 后续读回值也与测试值一致.

```text
haucet fastboot oem read 0x10CFC0
Using device PCIROOT(0)#PCI(1400)#USBROOT(0):21 ()
 0x0010CFC0: 0x00000000
OEM command completed

haucet fastboot oem write 0x10CFC0@0x11451419
Using device PCIROOT(0)#PCI(1400)#USBROOT(0):21 ()
OEM command completed

haucet fastboot oem read 0x10CFC0
Using device PCIROOT(0)#PCI(1400)#USBROOT(0):21 ()
 0x0010CFC0: 0x11451419
OEM command completed
```

#callout(
  [地址写入风险],
  [该地址的具体用途尚未确认, 不应将其视为通用安全测试地址.写入未知地址可能破坏设备状态或导致设备异常; 不要直接在其他设备或固件上照抄本例.],
  kind: "warn",
)

== 锁状态

```text
$ haucet fastboot oem lock-state info
FB LockState: UNLOCKED
USER LockState: LOCKED
```

#table(
  columns: (1.25fr, 1fr, 2.7fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*字段*], [*实测值*], [*含义*]),
  [`FB LockState`], [`UNLOCKED`], [Fastboot 锁状态.],
  [`USER LockState`], [`LOCKED`], [用户锁状态.],
)


== Root 完整性状态

```text
$ haucet fastboot oem check-rootinfo
status        : RS
version       : v.
current status: RS
old status    : RS
change_time   : 1783572984
item: fblock, status: RS, credible: Y
item: userlock, status: SF, credible: Y
```

`RS`、`SF` 的精确定义仍需结合固件实现确认.当前只记录原始值, 不对状态码作推断; 样本中的 `credible` 字段均为 `Y`.

== Root mode

```text
$ haucet fastboot oem get-rootmode
ROOTMODE: NO
```

`rootmode` 由 OEMINFO 中经过签名的证书材料验证, 因此常见返回为 `NO`.该结果反映认证状态, 不等同于 `FB LockState` 或 `USER LockState`.

= Getvar 扩展变量

Getvar 是查询路径, 但个别 `rescue_*` 变量可能触发模式切换或重启.未知变量先按有副作用处理, 不应在生产设备上批量探测.

#table(
  columns: (1.65fr, 0.85fr, 1.55fr, 2fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*变量名*], [*状态*], [*实测返回*], [*备注*]),
  [`dongle_info`], [#tag([实测], color: teal, background: teal-soft)], [`RSA-4096-PSS` + 若干十六进制字段], [字段含义与顺序仍需确认.],
  [`rescue_version`], [#tag([实测], color: teal, background: teal-soft)], [`rescue0.9`], [返回 Rescue 环境版本字符串.],
  [`rescue_phoneinfo`], [#tag([待确认], color: amber, background: amber-soft)], [—], [尚无稳定的返回样本.],
  [`rescue_enter_recovery`], [#tag([?])], [`start to hisuite mode`], [观测到设备随后重启, 因果与目标模式仍需复测.],
  [`rescue_get_updatetoken`], [#tag([待确认], color: amber, background: amber-soft)], [—], [返回格式与用途待研究.],
)

```text
$ haucet fastboot get-var dongle_info
dongle_info: RSA-4096-PSS,0x????,0x????,0x????,0x????,0x????

$ haucet fastboot get-var rescue_version
rescue_version: rescue0.9
```

== 待验证清单

#table(
  columns: (1.6fr, 3.8fr),
  fill: (x, y) => if y == 0 { paper },
  table.header([*项目*], [*下一步*]),
  [`oeminforead-*`], [确认完整命令格式、允许的字段与返回编码.],
  [其他 OEM command], [从固件分发表建立命令清单, 再逐项标注前置条件与副作用.],
)

#pagebreak()

= 附录

Powered by ljlVink. The reference code is located in the `hm-fastboot` crate.
