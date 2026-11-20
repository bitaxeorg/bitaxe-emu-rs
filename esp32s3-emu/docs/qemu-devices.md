[← rax README](../../README.md)

# ESP32-S3 peripheral behaviour in Espressif QEMU: register-level spec

Source: `upstream/qemu-espressif`, branch `esp-develop`, commit `7ca512d1f1`.
This file describes what QEMU's models *do*. It does not say what the silicon does. Where the two differ in ways firmware will notice, the difference is flagged with **QUIRK** (QEMU does something the silicon does not) or **UNMODELLED** (the register exists on silicon but QEMU ignores it).

Conventions used throughout:

* All offsets are from the device base. All registers are 32-bit and little-endian unless stated otherwise.
* "Reads 0 / writes ignored" is the default for every offset a model does not handle explicitly.
* "Virtual time" means `QEMU_CLOCK_VIRTUAL` in nanoseconds. Every counter below is derived from it. None counts instructions (unless QEMU runs with `-icount`).
* Interrupt sources are interrupt-matrix input numbers (section 8). "Level" means the model drives the line high while the condition holds and low otherwise.
* Reset domains matter (section 15.4). Most devices are sysbus devices and are reset by *every* QEMU system reset. The UARTs and the interrupt matrix are reset only when the SoC reset includes peripherals. RTC_CNTL is never reset.

---

## 0. Quick reference

### 0.1 Memory map as built by `hw/xtensa/esp32s3.c`

| Range | Size | What |
|---|---|---|
| 0x20000000 | ESP_RGB_MAX_VRAM_SIZE | virtual framebuffer (QEMU-only) |
| 0x21000000 | – | virtual RGB display registers (QEMU-only) |
| 0x3C000000–0x3DFFFFFF | 32 MiB | DCache window → shared MMU (section 4) |
| 0x3FC80000–0x3FDEFFFF | 0x170000 | "DRAM", plain RAM |
| 0x3FF00000–0x3FF1FFFF | 0x20000 | DROM, alias of IROM offset 0x40000 |
| 0x40000000–0x4005FFFF | 0x60000 | IROM, one ROM copy **per CPU** (CPU-private address space) |
| 0x40370000–0x403EFFFF | 0x80000 | "IRAM", plain RAM, **separate from DRAM** (no I/D alias) |
| 0x42000000–0x43FFFFFF | 32 MiB | ICache window → the same MMU as DCache |
| 0x50000000–0x50001FFF | 8 KiB | RTC slow RAM |
| 0x60000000–0x600D0FFF | 0xD1000 | `esp32s3.iomem` catch-all (section 15.3). Real devices override it. |
| 0x600FE000–0x600FFFFF | 8 KiB | RTC fast RAM |

Devices inside the catch-all window. Size is the size of QEMU's MMIO region. The rest of the silicon range falls back to the catch-all, which reads 0.

| Base | Region size | Device | Section |
|---|---|---|---|
| 0x60000000 | 0x7C | UART0 | 9 |
| 0x60002000 | 0x400 | SPI1 (flash controller) | 3 |
| 0x60004000 | 0x1000 | GPIO (strap only) | 11 |
| 0x60007000 | 0x204 | eFuse | 5 |
| 0x60008000 | 0x200 | RTC_CNTL | 2 |
| 0x60009000 | 0x2000 | IO_MUX: `unimplemented-device` (reads 0) | 11 |
| 0x60010000 | 0x7C | UART1 | 9 |
| 0x60016000 | 0x1000 | RMT: `unimplemented-device` | – |
| 0x6001F000 | 0x100 | TIMG0 | 6 |
| 0x60020000 | 0x100 | TIMG1 | 6 |
| 0x60023000 | 0x100 | SYSTIMER | 7 |
| 0x60026000 | 0x400 | "APB_CTRL/SYSCON": **plain RAM** with two words preset | 15.2 |
| 0x60028000 | – | SDMMC (DWC), IRQ 30 | – |
| 0x6002B000 | – | TWAI, IRQ 37 | – |
| 0x6002E000 | 0x7C | UART2 | 9 |
| 0x6003507C | 0x4 | RNG data register | 13 |
| 0x60038000 | 0x84 | USB-Serial-JTAG (dummy) | 10 |
| 0x6003A000 | – | AES, IRQ 77 | – |
| 0x6003B000 | 0x100 | SHA | 12 |
| 0x6003C000 | – | RSA, IRQ 76 | – |
| 0x6003D000 | – | Digital Signature | – |
| 0x6003E000 | – | HMAC | – |
| 0x6003F000 | – | GDMA, IN ch n → IRQ 66+n, OUT ch n → IRQ 71+n | – |
| 0x600C0000 | 0xA4 (!) | SYSTEM ("clock") | 1 |
| 0x600C1000 | 0x1000 | SENSITIVE / PMS (RAM-like) | 14 |
| 0x600C2000 | 0x1000 | Interrupt matrix | 8 |
| 0x600C4000 | 0x1800 | EXTMEM registers (0x000–0xFFF) and MMU table (0x1000–0x17FF) | 4 |
| 0x600CC000 | – | XTS-AES (flash encryption) | – |
| 0x600CD000 | – | OpenCores Ethernet, only with a `-nic`. IRQ wired to source 0 | – |

Not modelled at all, so they read 0 through the catch-all: SPI0 (0x60003000), IO_MUX, RTC_IO (0x60008400), SENS (0x60008800), I2C, LEDC, PCNT, SPI2/3, I2S, LCD_CAM, SARADC, USB_WRAP, ASSIST_DEBUG (0x600CE000), WCL, the regi2c analog master (0x6000E000) apart from one word (15.3), and everything in WDEV other than the RNG word. Addresses between 0x600D1000 and 0x600FDFFF are unassigned: an access there raises an Xtensa PIF address-error exception (EXCCAUSE 14 for a fetch, 15 for a load or store).

### 0.2 Interrupt sources QEMU actually drives

| Source | Name | Driver |
|---|---|---|
| 0 | (WIFI_MAC slot) | OpenCores Ethernet, only with a NIC |
| 27, 28, 29 | UART0, UART1, UART2 | level, (INT_RAW & INT_ENA) ≠ 0 |
| 30 | SDIO host | DWC SDMMC |
| 36 | EFUSE | level, INT_ST ≠ 0 |
| 37 | TWAI | – |
| 50, 51, 52 | TG0 T0, T1, WDT | level, raw & ena |
| 53, 54, 55 | TG1 T0, T1, WDT | level, raw & ena |
| 57, 58, 59 | SYSTIMER TARGET0, 1, 2 | level, raw & ena |
| 66–70 | GDMA IN ch0–4 | – |
| 71–75 | GDMA OUT ch0–4 | – |
| 76, 77, 78 | RSA, AES, SHA | SHA: raised only by the DMA operations |
| 79–82 | FROM_CPU 0–3 | level = SYSTEM_CPU_INTR_FROM_CPU_n bit 0 |

Every other source (GPIO 16/17, RTC_CORE 39, USB_SERIAL_JTAG 96, SPI1 20, the cache and PMS sources, and so on) is never asserted.

---

## 1. SYSTEM / clock: `hw/xtensa/esp32s3_clk.c`

* Type `esp32s3.soc.clk`, base **0x600C0000**, region size **0xA4**. It ends just after SYSTEM_COMB_PVT_ERR_HVT_SITE3 at 0xA0. Offsets 0xA4–0xFFF, including SYSTEM_DATE at 0xFFC, fall through to the catch-all and read 0.
* Sysbus device, so it is reset by every system reset.
* Four IRQ outputs n = 0..3 wired to matrix sources **79 + n** (ETS_FROM_CPU_INTR0..3_SOURCE).

| Off | Name | Reset | Read | Write |
|---|---|---|---|---|
| 0x000 | CORE_1_CONTROL_0 | – | 0 | ignored. **UNMODELLED**: APP CPU clock gate, runstall and reset are not modelled, so the APP CPU runs from power-on (15.5). |
| 0x004 | CORE_1_CONTROL_1 | 0 | stored value | stored, all 32 bits. The APP CPU boot address. It has no effect in QEMU itself: the real ROM's APP-CPU path polls it. |
| 0x010 | CPU_PER_CONF | **0x00000000** | constant reset value | **ignored**. CPUPERIOD_SEL [1:0] = 0 (80 MHz) and PLL_FREQ_SEL [2] = 0 (480 MHz) always read back. |
| 0x028 | BT_LPCK_DIV_INT | 0x000000FF | stored | stored & 0xFFF |
| 0x02C | BT_LPCK_DIV_FRAC | 0x02001001 | stored | stored & 0x1FFFFFFF |
| 0x030 | CPU_INTR_FROM_CPU_0 | 0 | bit0 = current level | bit0 = 1 drives source 79 high, bit0 = 0 drives it low. Only bit0 matters; all other bits are dropped. |
| 0x034 | CPU_INTR_FROM_CPU_1 | 0 | same | same, source 80 |
| 0x038 | CPU_INTR_FROM_CPU_2 | 0 | same | same, source 81 |
| 0x03C | CPU_INTR_FROM_CPU_3 | 0 | same | same, source 82 |
| 0x04C | EXTERNAL_DEVICE_ENCRYPT_DECRYPT_CONTROL | 0 | stored | stored, 32 bits. XTS-AES reads it (bit0 ENABLE_SPI_MANUAL_ENCRYPT, bit1 DB_ENCRYPT, bit2 G0CB_DECRYPT, bit3 MANUAL_ENCRYPT). |
| 0x060 | SYSCLK_CONF | **0x000A8401** | constant reset value | **ignored** |

SYSCLK_CONF reset value decoded: PRE_DIV_CNT [9:0] = 1, SOC_CLK_SEL [11:10] = 1 (PLL), CLK_XTAL_FREQ [18:12] = 40, CLK_DIV_EN [19] = 1.

Notes:

* FROM_CPU semantics are pure level. Write 1 to assert, write 0 to deassert. There is no edge and no auto-clear. Reset lowers all four lines.
* **QUIRK**: clock-tree writes are dropped. Firmware that switches to 160 or 240 MHz still reads PLL / 80 MHz. ESP-IDF's `esp_clk_cpu_freq()` therefore reports 80 MHz.
* **QUIRK**: the Xtensa CCOUNT and CCOMPARE timers tick at **40 MHz of virtual time** (`clock_freq_khz = 40000` in `target/xtensa/core-esp32s3.c`), whatever these registers say.
* **UNMODELLED** (read 0, writes ignored): PERIP_CLK_EN0/1, PERIP_RST_EN0/1, CPU_PERI_CLK_EN/RST_EN, MEM_PD_MASK, RTC_FASTMEM_CONFIG / CRC, REDUNDANT_ECO, CLOCK_GATE, and the other clock and reset registers. Polling a reset "done" bit there sees 0.

---

## 2. RTC_CNTL: `hw/misc/esp32s3_rtc_cntl.c`

* Type `misc.esp32s3.rtc_cntl`, base **0x60008000**, size **0x200** (DATE at 0x1FC is the last word).
* It sits on the SoC's private `rtc_bus`, which is never reset. The model's reset hook, which rebases TIME, never runs. All state below lives from power-on and survives every reset, as an RTC domain should.
* Its sysbus IRQ (meant for ETS_RTC_CORE_INTR_SOURCE 39) is **not connected**.
* Named GPIO outputs: `dig-reset`, `cpu-reset[0..1]`, `cpu-stall[0..1]`, `clk-update`. Section 15.4 describes their effect.

| Off | Name | Power-on value | Read | Write |
|---|---|---|---|---|
| 0x000 | OPTIONS0 | 0 | stored value | See below. The value is stored with bits 31, 5 and 4 removed. |
| 0x00C | TIME_UPDATE | – | always **0x40000000** (bit30, which QEMU calls VALID) | Bit31 = 1 latches the RTC time: TIME = (now_ns − base_ns) × slow_freq / 1e9, with base_ns = 0, so counting starts at virtual time 0. The write value is not stored. |
| 0x010 | TIME0 | 0 | latched time bits [31:0] | ignored |
| 0x014 | TIME1 | 0 | latched time bits [63:32] | ignored |
| 0x038 | RESET_STATE | **0x00003041** | composed: RESET_CAUSE_PROCPU [5:0], RESET_CAUSE_APPCPU [11:6], APPCPU_STAT_VECTOR_SEL [12], PROCPU_STAT_VECTOR_SEL [13] | Only bits 13 and 12 are taken (vector select per CPU, used at the next reset of that core). Writes to the other bits are ignored. |
| 0x050–0x05C | STORE0–3 | 0 | stored | stored |
| 0x074 | CLK_CONF | **0x20000000** | composed: SOC_CLK_SEL [28:27], FAST_CLK_RTC_SEL [29], ANA_CLK_RTC_SEL [31:30]. All other bits read 0. | Those three fields are stored and the RTC clock rates recomputed (below). Other bits are lost. |
| 0x0BC | SW_CPU_STALL | 0 | stored | stored, then the stall state is re-evaluated |
| 0x0C0–0x0CC | STORE4–7 | 0 | stored | stored. ROM and IDF keep XTAL and APB frequency here as scratch. |

OPTIONS0 write actions, evaluated in this order:

1. Bit31 SW_SYS_RESET: reset cause of both CPUs = **3**, then the `dig-reset` line is pulsed. This makes a machine reset of both cores and the peripherals (15.4).
2. Bit4 SW_APPCPU_RESET: APP reset cause = **12**, then `cpu-reset[1]` is pulsed.
3. Bit5 SW_PROCPU_RESET: PRO reset cause = **12**, then `cpu-reset[0]` is pulsed.
4. The value without bits 31, 5 and 4 is stored, and the stall state is re-evaluated.

Stall logic, re-evaluated after every write to OPTIONS0 and SW_CPU_STALL. A CPU is stalled while the 8-bit value (C1 << 2) | C0 equals exactly **0x86**, that is C1 = 0x21 and C0 = 2.

* PRO CPU: C1 is SW_CPU_STALL [31:26] and C0 is OPTIONS0 [3:2].
* APP CPU: C1 is SW_CPU_STALL [25:20] and C0 is OPTIONS0 [1:0].

The stall level is driven to the SoC, which sets the Xtensa runstall. A stall persists across that core's resets.

RTC clock rates. These only affect the TIME counter. TIMG's calibration uses its own constants (section 6.5).

* Slow clock, selected by ANA_CLK_RTC_SEL: 0 → **150 000 Hz** (RC, the power-on value), 1 → 32 768 Hz, 2 → 31 250 Hz (8 MHz / 256). The value 3 indexes past the table, which is undefined behaviour.
* Fast clock: 0 → XTAL/4 = 10 MHz, 1 → 8 MHz (power-on).
* Power-on values: SOC_CLK_SEL = 0 (XTAL), FAST = 1, ANA = 0.
* Every CLK_CONF write pulses `clk-update`, which the SoC ignores.

Reset causes known to the model: 1 POWERON, 3 SW_SYS, 12 SW_CPU. Both CPUs start at 1, and nothing else ever sets a cause. A watchdog never resets anything (sections 6.4 and 15.4).

Notes:

* **QUIRK**: the RTC TIME counter runs at 150 kHz for the RC clock, while TIMG calibration reports RC_SLOW as 136 kHz. IDF RTC time derived from calibration × ticks therefore runs about 10 % fast.
* **QUIRK**: TIME_UPDATE bit30 always reads 1. That is the ESP32 TIME_VALID bit. On the S3, bit30 is something else.
* **UNMODELLED**: RTC WDT (WDTCONFIG0–4, WDTFEED, WDTWPROTECT), SWD, INT_ENA/RAW/ST/CLR, STATE0 and the sleep machinery, TIMER1/2, SLP_TIMER0/1, ANA_CONF, RESET_STATE clear bits, BROWN_OUT, the power and isolation registers, LOW_POWER_ST, DIAG0, and RTC_CNTL_DATE (reads 0). Nothing raises source 39.

---

## 3. SPI1 flash controller: `hw/ssi/esp32s3_spi.c` and the attached flash

* Type `ssi.esp32s3.spi`, base **0x60002000**, size **0x400**. No IRQ (ETS_SPI1_INTR_SOURCE 20 is unused).
* It owns an SSI bus with 3 active-low CS outputs: CS0 goes to the flash (if any), CS1 to the PSRAM (if `-m` > 0), and CS2 is unused.
* Every operation completes synchronously inside the write to SPI_MEM_CMD. There is no busy state.

### 3.1 Registers

| Off | Name | Reset | Read | Write |
|---|---|---|---|---|
| 0x000 | CMD | – | **always 0** (so every busy bit reads clear) | Triggers a transaction (3.2). The value is not stored. |
| 0x004 | ADDR | 0 | stored | stored |
| 0x008 | CTRL | 0 | stored | stored. Only FREAD_QIO [24] and FREAD_DIO [23] are used, for the dummy computation. |
| 0x00C | CTRL1 | 0x00000FFC (CS_HOLD_DLY_RES [11:2] = 0x3FF) | stored | stored |
| 0x010 | CTRL2 | 0 | stored | stored |
| 0x014 | CLOCK | 0x00030103 (CLKCNT_N = 3, H = 1, L = 3) | stored | stored, no effect |
| 0x018 | USER | 0x80000000 (USR_COMMAND) | stored | stored |
| 0x01C | USER1 | 0x5C000007 (ADDR_BITLEN [31:26] = 23, DUMMY_CYCLELEN [5:0] = 7) | stored | stored |
| 0x020 | USER2 | 0x70000000 (COMMAND_BITLEN [31:28] = 7) | stored | stored |
| 0x024 | MOSI_DLEN | 0 | stored | stored. USR_MOSI_DBITLEN [9:0]. |
| 0x028 | MISO_DLEN | 0 | stored | stored. USR_MISO_DBITLEN [9:0]. |
| 0x02C | RD_STATUS | 0 | stored | stored. RDSR writes only byte 0. WRSR sends byte 0. |
| 0x034 | MISC | 0x00000002 (CS1_DIS = 1, CS0_DIS = 0) | stored | stored. CS0_DIS [0] and CS1_DIS [1] gate the chip selects. |
| 0x03C | CACHE_FCTRL | 0 | stored | stored, no effect |
| 0x054 | FSM | 0 | **always 0**: the write is not handled | ignored |
| 0x058–0x094 | W0–W15 | 0 | stored | stored. The 64-byte data buffer, byte 0 of W0 first. |
| 0x0A4 | SUS_STATUS | 0 | stored | stored, no effect |
| 0x0E0 | DDR_CTRL | 0 | stored | stored, no effect |
| 0x0E8 | CLOCK_GATE | 0 | stored | stored, no effect |

Everything else reads 0 and ignores writes: TX_CRC 0x38, FLASH_WAITI_CTRL 0x98, FLASH_SUS_CTRL 0x9C, FLASH_SUS_CMD 0xA0, TIMING_CALI 0xA8, INT_ENA, CLR, RAW and ST at 0xC0–0xCC, DATE 0x3FC (reads 0).

Reset ORs the fields above into the existing register values: other bits of those registers survive a system reset. It also clears W0–W15.

### 3.2 CMD write

If bit18 (USR) is set, a **user transaction** runs (3.3). Otherwise the value with bits 18..0 discarded (`CMD & 0xFFF80000`) must equal **exactly one** of the bits below; any other value does nothing. FLASH_PE (bit17) is ignored.

| CMD bit | Flash opcode sent | Address | Data |
|---|---|---|---|
| 31 FLASH_READ | 0x03 | low (ADDR_BITLEN+1)/8 bytes of ADDR, MSB first | rx (MISO_DBITLEN+1)/8 bytes into W0.. |
| 30 FLASH_WREN | 0x06 | – | – |
| 29 FLASH_WRDI | 0x04 | – | – |
| 28 FLASH_RDID | 0x9F | – | rx 3 bytes into W0 bytes 0..2 (manufacturer in bits [7:0], then type, then capacity) |
| 27 FLASH_RDSR | 0x05 | – | rx 1 byte into RD_STATUS [7:0] |
| 26 FLASH_WRSR | 0x01 | – | tx RD_STATUS [7:0] (1 byte) |
| 25 FLASH_PP | 0x02 | **ADDR [23:0]**, MSB first (assumes a 3-byte address) | tx ADDR[31:24] bytes from W0.. |
| 24 FLASH_SE | 0x20 (4 KiB erase) | low (ADDR_BITLEN+1)/8 bytes of ADDR, MSB first | – |
| 23 FLASH_BE | 0xD8 (64 KiB erase) | same | – |
| 22 FLASH_CE | 0x60 | – | – |
| 21 FLASH_DP | 0xB9 | – | – |
| 20 FLASH_RES | 0xAB | – | rx 3 bytes into W0 |
| 19 FLASH_HPM | 0xA3 | – | rx 3 bytes into W0 (as dummy) |

No dummy cycles are sent for special commands.

### 3.3 User transaction (CMD bit18)

The phases run in order with CS low, then CS goes high.

1. **Command**, always sent. USR_COMMAND is ignored on purpose. Byte count = (USR_COMMAND_BITLEN+1)/8. The bytes are USR_COMMAND_VALUE [15:0], low byte first.
2. **Address**, only if USER.USR_ADDR [30]. Byte count n = (USR_ADDR_BITLEN+1)/8. For 1 ≤ n ≤ 4, the low n bytes of ADDR are sent MSB first (ADDR = 0x00ABCDEF with n = 3 sends AB CD EF). A larger n sends garbage.
3. **Dummy**, only if USR_ADDR **and** USR_DUMMY [29] are set. With f = USR_DUMMY_CYCLELEN (cycles − 1), the number of 0x00 bytes is:
   * f+1 cycles × 4 bits / 8 if CTRL.FREAD_QIO;
   * f+1 cycles × 2 bits / 8 if CTRL.FREAD_DIO;
   * (f + 7) / 8 otherwise. So 8 or 9 cycles give 1 byte and 18 cycles give 3 bytes.
4. **Data**: tx_bytes = (MOSI_DBITLEN+1)/8 if USR_MOSI [27], and rx_bytes = (MISO_DBITLEN+1)/8 if USR_MISO [28]. **QUIRK**: this is one full-duplex loop of max(tx, rx) byte transfers over W0..W15 viewed as a byte array, not a MOSI phase followed by a MISO phase.
   * Byte i sent = buffer[i] if tx_bytes > 0, else 0x00.
   * The reply is stored into buffer[i] only if the *value of the byte sent* is < rx_bytes. This is an index-versus-value bug.
   * Net effect: pure reads store every reply. Pure writes store nothing. Mixed transactions behave oddly.
   * Lengths above 64 bytes overrun the buffer (MISO up to 128 bytes, PP up to 255).

If XTS-AES manual encryption is enabled and its ciphertext is SPI-visible, a PP (opcode 0x02) user transaction takes its data, length and address from XTS-AES instead.

Chip selects. At the start: CS0 is driven low unless MISC.CS0_DIS, and CS1 low unless MISC.CS1_DIS (a disabled CS is driven high). At the end both are driven high. After reset only the flash is selected. Talking to the PSRAM needs CS0_DIS = 1 and CS1_DIS = 0.

### 3.4 Which flash chip: `esp32s3_init_spi_flash`

The model is picked from the size of the `-drive if=mtd` image:

| Image size | m25p80 part | JEDEC ID (RDID bytes) | Manufacturer class |
|---|---|---|---|
| 2 MiB | `w25x16` | EF 30 15 | Winbond |
| 4 MiB | `gd25q32` | C8 40 16 | GigaDevice |
| 8 MiB | `gd25q64` | C8 40 17 | GigaDevice |
| 16 MiB | `is25lp128` | 9D 60 18 | ISSI |

Any other size prints "Drive size error". No flash device is created, but the cache still maps the image (section 4), and SPI1 transfers then read 0. Without `-drive if=mtd` there is no flash and no cache backing. All four parts are 64 KiB sectors with the ER_4K flag only (no ER_32K), 256-byte pages, and no SFDP table.

### 3.5 m25p80 command behaviour for these parts

* **Timing**: every program and erase completes instantly. **WIP (SR bit0) is never set.** There is no busy time to poll.
* **Status register (RDSR 0x05)**: WEL [1], BP0 [2], BP1 [3], BP2 [4], SRWD [7]. For ISSI only, QE is reported in bit6. Neither Winbond nor GigaDevice reports QE in SR1. The byte repeats for as long as CS stays low.
* **WREN 0x06** sets WEL. **WRDI 0x04** clears it.
* **QUIRK**: WEL is **not** cleared after PP or erase. It is cleared only by WRDI, by a completed WRSR, by a 0x66/0x99 reset, or by device reset.
* **WRSR 0x01** is accepted only with WEL = 1 (WP# is high, so SRWD alone does not block it).
  * Winbond and GigaDevice collect 1 or 2 bytes, completing at the 2nd byte or at CS high. Byte0 holds SRWD and BP0..2. If 2 bytes are given, byte1 bit1 = QE.
  * ISSI takes 1 byte, with QE = byte0 bit6.
  * The command clears WEL.
* **WRSR2 0x31** (Winbond and GigaDevice only) needs WEL. It takes 1 byte, QE = bit1, and does not clear WEL.
* **0x35**: Winbond and GigaDevice return QE << 1, one byte. On ISSI it is an unknown command and returns 0.
* **JEDEC 0x9F**: returns the 3 ID bytes, then 3 bytes of 0x00. The model then goes idle, so further clocks are decoded as new opcodes.
* **READ 0x03**: 3 address bytes (4 in 4-byte mode), then a stream wrapping at chip size.
* **FAST_READ 0x0B**: address, then dummy *bytes*: GigaDevice 1, ISSI 1, **Winbond 8** (**QUIRK**: SPI1's dummy computation sends 1 byte, so FAST_READ through SPI1 on the 2 MiB model is misaligned).
* **DOR 0x3B and QOR 0x6B**: same dummy count as FAST_READ.
* **DIOR 0xBB**: address + 1 mode byte (all three vendors).
* **QIOR 0xEB**: GigaDevice address + 1 + 2, Winbond address + 1 + 4, ISSI address + 3.
* **PP 0x02** (and 0x32, 0xA2, 0x12, 0x3E): address, then each byte is ANDed into the array.
  * **QUIRK**: the address wraps at **chip size**, not at the 256-byte page.
  * The write is refused byte by byte if WEL = 0 or the byte is BP-protected (top protection, 2^(BP−1) sectors).
* **Erase**:
  * 0x20 erases 4 KiB, 0x52 32 KiB (logs "unsupported size" but still erases), 0xD8 64 KiB, 0x60 and 0xC7 the whole chip.
  * Erase needs WEL. It fills with 0xFF starting **at the exact address given**: **QUIRK**, the address is not aligned down to the erase size.
* **0xB7 / 0xE9**: enter or exit 4-byte address mode.
* **0x66 then 0x99**: soft reset. Clears WEL, QE and 4-byte mode.
* **0xAB, 0x90**: unsupported for these vendors. The model collects 3 address bytes and returns 0x00.
* **0x5A** (SFDP), **0xB9**, **0xA3**, and every other unknown opcode: return 0x00 for as long as CS stays low.
* **CS high** completes a variable-length WRSR, flushes the dirty page to the drive asynchronously, and returns to idle.
* **Device reset**, on every QEMU system reset (the flash is on SPI1's bus): SRWD, BP0..3, TB, WEL, QE and 4-byte mode are cleared. Status bits are therefore *volatile* in this model.

**QUIRK**: the flash array that m25p80 writes is not what the CPU reads through the cache. See section 4.3.

---

## 4. EXTMEM cache controller + MMU: `hw/misc/esp32s3_cache.c`

* Type `esp32s3.icache`, one MMIO region at **0x600C4000**, size **0x1800**. Offsets 0x000–0x1FF are registers, 0x1000–0x17FF the MMU table (0x600C5000).
* Two 32 MiB windows, **0x3C000000** (DCache) and **0x42000000** (ICache), are both aliases of **one** IOMMU region. They see the same 512 entries.
* No IRQ is ever raised.

### 4.1 MMU table: 512 entries × 4 bytes at 0x600C5000

Entry format:

| Bits | Field | Meaning |
|---|---|---|
| [13:0] | page | physical 64 KiB page number |
| [14] | INVALID | |
| [15] | TYPE | 0 = flash, 1 = PSRAM |
| [31:16] | reserved | forced to 0 on write |

Power-on and reset value: INVALID set, other bits unchanged from before. At power-on that gives **0x00004000**.

Read: the stored entry.

Write (only acts if the new value, with the reserved bits cleared, differs from the stored one):

1. The old mapping of virtual page `index` is unmapped, which flushes the TCG TLB for that 64 KiB.
2. If the **new** entry has INVALID set and the **old** entry was a flash entry, the 64 KiB of the flash mirror at old_page × 64 KiB is overwritten with the word 0xDEADBEEF. Every other virtual page that mapped the same physical page sees that too.
3. If the new entry is valid, TYPE = flash and a drive exists: 64 KiB are read from the **drive image** at page × 64 KiB into the RAM mirror. The page is decrypted in place if XTS-AES reports flash encryption enabled.
4. A valid PSRAM entry needs no copy.
5. The entry is stored.

### 4.2 Reads and fetches through the windows

For an access at window offset `o`:

* entry = MMU[o >> 16] and physical address = entry.page × 65536 + (o & 0xFFFF).
* TYPE 0 targets the flash mirror with **read-only** permission (none if there is no drive). TYPE 1 targets PSRAM data with read/write permission (none if no PSRAM).
* A write to a flash page, or any access with no permission, is a transaction failure. The Xtensa CPU takes a PIF address-error exception: EXCCAUSE 15 for a load or store, 14 for an instruction fetch.
* **QUIRK**: the INVALID bit is **not checked** on translation. An invalid entry still maps to its page field: zeros if that page was never loaded, stale data, or 0xDEADBEEF.
* **QUIRK**: ICACHE_CTRL and DCACHE_CTRL enables are not consulted. The windows translate whether the caches are "enabled" or not.

### 4.3 Coherence with SPI1 writes

**QUIRK**: the mirror for a page is filled from the drive file only when its MMU entry *changes value*. m25p80 keeps its own copy of the array and writes back to the drive asynchronously, page by page, when CS goes high. After a flash program or erase through SPI1, reading through an unchanged mapping returns **stale** data. IDF's cache invalidation does not refresh it; only rewriting the MMU entry to a different value and then back does.

### 4.4 EXTMEM registers

The model keeps a 128-word store for offsets 0x000–0x1FF. Writes there store the raw value unless noted. Writes to 0x200–0xFFF are dropped. **Reads return 0 for every offset not listed**, even after a write (for example SYNC_ADDR, SYNC_SIZE, LOCK_*, the PRELOAD and AUTOLOAD address and size registers, MMU_OWNER, CLOCK_GATE, and DATE at 0x3FC).

| Off | Name | Reset | Read | Write |
|---|---|---|---|---|
| 0x000 | DCACHE_CTRL | 0 | `dcache_enable` (0 or 1) | dcache_enable = bit0 |
| 0x004 | DCACHE_CTRL1 | 0 | `dcache_enable`, the same flag | dcache_enable = bit0. **QUIRK**: on silicon bit0 here is SHUT_IBUS. |
| 0x028 | DCACHE_SYNC_CTRL | 0 | Read side effects: the store's bit3 is set, then, if bit0 (INVALIDATE_ENA) is set, bit0 is cleared and bit1 set. Returns the store, so **bit3 (SYNC_DONE on S3) always reads 1**. | raw store |
| 0x030 | DCACHE_SYNC_SIZE | – | 0 | stored, never read back |
| 0x040 | DCACHE_PRELOAD_CTRL | **0x2** (DONE) | If bit0 ENA is set, bit0 is cleared and bit1 DONE set (read side effect). Returns the store. | raw store |
| 0x04C | DCACHE_AUTOLOAD_CTRL | **0x8** (DONE) | If bit2 ENA is set, it is cleared and bit3 DONE set. Returns the store. | raw store |
| 0x060 | ICACHE_CTRL | 0 | `icache_enable` | icache_enable = bit0 |
| 0x064 | ICACHE_CTRL1 | 0 | `icache_enable` | icache_enable = bit0 (same **QUIRK**) |
| 0x088 | ICACHE_SYNC_CTRL | 0 | If bit0 INVALIDATE_ENA is set, it is cleared and bit1 SYNC_DONE set. Returns the store. **No forced bit**: after writing 0, it reads 0. | raw store |
| 0x094 | ICACHE_PRELOAD_CTRL | **0x2** | bit0 ENA → cleared, bit1 DONE set | raw store |
| 0x0A0 | ICACHE_AUTOLOAD_CTRL | **0x8** | bit2 ENA → cleared, bit3 DONE set | raw store |
| 0x130 | CACHE_STATE | – | **0x00001001** (DCACHE_STATE [11:0] = 1 and ICACHE_STATE [23:12] = 1, both idle) | stored, ignored |
| 0x150 | DCACHE_FREEZE | 0 | stored | If bit0 FREEZE_ENA is set, store bit2 DONE is set; otherwise bit2 is cleared. Nothing else is stored, so reads show only bit2. |
| 0x154 | ICACHE_FREEZE | 0 | stored | same |

Notes:

* The DONE bits appear only **on read**: a write of ENA followed by a read gives DONE. The DONE bit then stays set in the store until overwritten.
* `dcache_enable` and `icache_enable` are not reset by system reset. The store and the MMU INVALID bits are reset.
* **UNMODELLED and likely polled**: DCACHE_LOCK_CTRL and ICACHE_LOCK_CTRL LOCK_DONE (bit2) read 0. So do the occupy, prelock, ILG and ACS interrupt status, the MMU fault content, and all access and miss counters. The cache preload, sync and access interrupts (sources 61–64, 94, 95) are never raised.

---

## 5. eFuse: `hw/nvram/esp_efuse.c` (base) + `hw/nvram/esp32s3_efuse.c`

* Type `nvram.esp32s3.efuse` (parent `nvram.esp.efuse`), base **0x60007000**, size **0x204**.
* IRQ → source **36**, level.
* Backing store: the device property `drive`, which **esp32s3.c never sets**. With no `-global driver=nvram.esp32s3.efuse,property=drive,value=<id>`, the backing is a RAM buffer **initialised to all zeros**.

### 5.1 Default contents with no file

**Every eFuse bit is 0.** That means:

* MAC (BLOCK1 words 0–1) = 00:00:00:00:00:00;
* WAFER_VERSION_MAJOR/MINOR, BLK_VERSION and PKG_VERSION = 0, so the chip reads as revision v0.0;
* no flash or PSRAM capacity, vendor or temperature fields;
* no keys, no secure boot or flash-encryption bits, no write or read protection.

The backing file layout is the 0x150-byte `ESPEfuseBlocks` structure, which starts at RD_WR_DIS. Byte offset in the file = register offset − 0x2C.

### 5.2 Register map

Reads at offsets ≤ 0x178 support 1, 2 and 4-byte accesses. Everything at higher offsets must be accessed as 32-bit words: a narrower access **aborts QEMU** (assert).

| Off | Name | Read | Write |
|---|---|---|---|
| 0x000–0x01C | PGM_DATA0–7 | stored | stored (any size) |
| 0x020–0x028 | PGM_CHECK_VALUE0–2 | stored | stored. Not used: no RS code check. |
| 0x02C | RD_WR_DIS (BLOCK0 w0) | current visible block data | ignored |
| 0x030–0x040 | RD_REPEAT_DATA0–4 (BLOCK0 w1–5) | " | ignored |
| 0x044–0x058 | RD_MAC_SPI_SYS_0–5 (BLOCK1, 6 words) | " | ignored |
| 0x05C–0x078 | RD_SYS_PART1_DATA0–7 (BLOCK2) | " | ignored |
| 0x07C–0x098 | RD_USR_DATA0–7 (BLOCK3) | " | ignored |
| 0x09C–0x0B8 | RD_KEY0_DATA0–7 (BLOCK4) | " | ignored |
| 0x0BC–0x0D8 | RD_KEY1 (BLOCK5) | " | ignored |
| 0x0DC–0x0F8 | RD_KEY2 (BLOCK6) | " | ignored |
| 0x0FC–0x118 | RD_KEY3 (BLOCK7) | " | ignored |
| 0x11C–0x138 | RD_KEY4 (BLOCK8) | " | ignored |
| 0x13C–0x158 | RD_KEY5 (BLOCK9) | " | ignored |
| 0x15C–0x178 | RD_SYS_PART2_DATA0–7 (BLOCK10) | " | ignored |
| 0x17C–0x1C4 | RD_REPEAT_ERR*, RD_RS_ERR* | **0** (no errors) | ignored |
| 0x1C8 | CLK | stored (power-on 0) | stored |
| 0x1CC | CONF | stored | stored. OP_CODE [15:0]: 0x5AA5 = read, 0x5A5A = program. |
| 0x1D0 | STATUS | **0x00000001** (STATE [3:0] = 1, idle), constant | ignored |
| 0x1D4 | CMD | stored; non-zero while an operation is pending | See 5.3 |
| 0x1D8 | INT_RAW | stored: bit0 READ_DONE, bit1 PGM_DONE | **any write clears both bits** (QEMU-specific), then the IRQ is updated |
| 0x1DC | INT_ST | stored (ENA & RAW) | ignored |
| 0x1E0 | INT_ENA | stored | value & 3, then the IRQ is updated |
| 0x1E4 | INT_CLR | 0 | RAW &= ~value & 3, then the IRQ is updated |
| 0x1E8 | DAC_CONF | stored | stored |
| 0x1EC | RD_TIM_CONF | stored | stored |
| 0x1F0 | WR_TIM_CONF0 | 0 | ignored (**UNMODELLED**) |
| 0x1F4 | WR_TIM_CONF1 | stored | stored |
| 0x1F8 | WR_TIM_CONF2 | stored | stored |
| 0x1FC | DATE | **0** | ignored |
| 0x200 | DBG_ERASE_ALL | 0 | **QEMU-only**: any 32-bit write zeroes every eFuse in the backing store and in the visible registers |

### 5.3 Command protocol

* **Write CMD with bit0 READ = 1 while CONF.OP_CODE = 0x5AA5**: CMD keeps the written value (busy) and a timer is armed for **100 µs** of virtual time. If a timer is already pending, the earlier deadline wins. When it fires:
  1. CMD is restored from the saved copy and the visible blocks are reloaded from the backing store.
  2. Read-protected blocks are hidden: for each set bit k of RD_REPEAT_DATA0.RD_DIS [6:0], BLOCK(4+k) reads as zeros.
  3. INT_RAW |= 1 and CMD = 0.
  4. INT_ST = ENA & RAW. If INT_ST ≠ 0 the IRQ is raised; this path never lowers it.
* **Write CMD with bit1 PGM = 1 while OP_CODE = 0x5A5A**: block number = CMD [5:2].
  * The burn happens **immediately**, into the backing store only. The visible RD registers do not change until a READ command.
  * For each word i of the block (6 words for blocks 0 and 1, 8 for the others), the new value is old_visible[i] | (PGM_DATA[i] & ~protect_mask[i]). Bits never go back to 0.
  * Blocks 1–10 are wholly protected by RD_WR_DIS bit (19 + block). BLOCK0 is protected bit by bit by a per-WR_DIS-bit mask table.
  * If the block is protected: nothing is written, CMD = 0 immediately, no interrupt.
  * Otherwise the 100 µs timer is armed. When it fires: INT_RAW |= 2, CMD = 0, the IRQ is updated as above.
* **Any other CMD write**: CMD = 0, nothing happens.

Reset (every system reset) cancels the pending timer, lowers the IRQ and reloads the visible blocks from the backing store, with protection hiding applied. STATUS, CONF and the other config registers are not reset.

---

## 6. TIMG0 / TIMG1: `hw/timer/esp_timg.c` (+ `esp32s3_timg.c`, which only sets `has_t1 = true`)

* Type `timer.esp32s3.timg`. Bases **0x6001F000** and **0x60020000**, size **0x100** each.
* Outputs: T0 → source 50 / 53, T1 → 51 / 54, WDT interrupt → 52 / 55. All level, high while raw & ena.
* **The WDT reset output is NOT connected on the S3 machine.** An MWDT timeout in reset mode does nothing.
* Clocks are fixed constants. APB = **80 MHz** and XTAL = **40 MHz**, whatever SYSTEM or RTC says.

### 6.1 Counter model (applies to T0, T1 and the WDT)

Each counter has a virtual-time base and a frequency.

* Updating adds ticks = elapsed_ns × freq / 1e9 and advances the base by the duration of whole ticks only. Frequent reads lose no time.
* Alarms are QEMU timers armed at now + ticks × 1e9 / freq.

### 6.2 Registers

T1 occupies 0x24–0x44 with exactly the T0 layout (offset − 0x24).

| Off | Name | Reset | Read | Write |
|---|---|---|---|---|
| 0x00 / 0x24 | TxCONFIG | **0x60002000** (INCREASE [30], AUTORELOAD [29], DIVIDER [28:13] = 1) | stored | See 6.3. Bit12 DIVCNT_RST is never stored. |
| 0x04 / 0x28 | TxLO | 0 | latched value [31:0] | ignored |
| 0x08 / 0x2C | TxHI | 0 | latched value [53:32] | ignored |
| 0x0C / 0x30 | TxUPDATE | – | **0** (the update completes at once) | Any value latches the live 54-bit count into TxLO/HI. If the timer is disabled, the held value is latched. |
| 0x10 / 0x34 | TxALARMLO | 0 (not reset) | stored | alarm [31:0] = value, then the alarm is re-evaluated |
| 0x14 / 0x38 | TxALARMHI | 0 | stored | alarm [53:32] = value & 0x3FFFFF, then re-evaluated |
| 0x18 / 0x3C | TxLOADLO | 0 | stored | load [31:0] |
| 0x1C / 0x40 | TxLOADHI | 0 | stored | load [53:32] (22 bits) |
| 0x20 / 0x44 | TxLOAD | – | 0 | Any value: update the count, set count = load, then re-evaluate the alarm |
| 0x48 | WDTCONFIG0 | 0 | stored | 6.4. Write-protected. |
| 0x4C | WDTCONFIG1 | 0 | stored mirror without bit0 | stored in the mirror only (not protected) |
| 0x50–0x5C | WDTCONFIG2–5 | 0 (mirrors) | mirror | stored in the mirror (not protected) |
| 0x60 | WDTFEED | – | 0 | Any value: if EN, go to stage 0 and re-arm stage 0 (not protected) |
| 0x64 | WDTWPROTECT | **0x50D83AA1** (unlocked) | stored | stored. Unlocked iff the value equals 0x50D83AA1. |
| 0x68 | RTCCALICFG | **0x00013000** (power-on only; not reset) | stored | 6.5 |
| 0x6C | RTCCALICFG1 | 0 | result: VALUE [31:7]. CYCLING_DATA_VLD [0] always 0. | ignored |
| 0x70 | INT_ENA | 0 | bit0 T0, bit1 T1, bit2 WDT | Sets the three enables. A line whose enable changed is re-driven to raw & ena. |
| 0x74 | INT_RAW | 0 | bit0 T0, bit1 T1, bit2 WDT | ignored |
| 0x78 | INT_ST | 0 | raw & ena per bit | ignored |
| 0x7C | INT_CLR | – | 0 | Each set bit clears that raw flag and lowers its line |
| 0x80 | RTCCALICFG2 | 0 | stored | 6.5 |
| 0xF8 | NTIMG_DATE | – | 0 | ignored |
| 0xFC | REGCLK | – | 0 | ignored |

### 6.3 T0 / T1 timers

Tick rate = src / div:

* src = 40 MHz if USE_XTAL [9] is set, else 80 MHz;
* div = DIVIDER [28:13], where 0 means 65536 and 1 means 2.

The reset value therefore gives 40 MHz. The count is 54 bits and wraps modulo 2^54. Direction follows INCREASE [30].

TxCONFIG write, in this order:

1. If the timer was enabled, the count is brought up to date at the old rate.
2. The new rate is applied.
3. If DIVCNT_RST [12] is written as 1, the tick in progress restarts and the alarm is re-evaluated.
4. If ALARM_EN [10] changed: 0→1 re-evaluates the alarm, 1→0 cancels it.
5. If INCREASE changed, the alarm is re-evaluated.
6. If EN [31] changed: 0→1 resumes from the held value (base = now) and re-evaluates the alarm; 1→0 cancels the alarm and holds the value.

Alarm evaluation runs only when EN and ALARM_EN are both set. The count is updated first, then with d = |alarm − value| and L = 2^53:

* Counting up with alarm > value and d ≤ L: arm in d ticks.
* Counting up with value ≥ alarm and d < L, or alarm > value and d > L: **fire immediately**, synchronously inside the register write.
* Remaining cases, which need a wrap: arm in (2^54 − max) + min ticks.
* Counting down: mirror image.

On fire:

1. The QEMU timer is cancelled and the base set to now.
2. **The count is forced to the alarm value**, or to the load value if AUTORELOAD [29].
3. ALARM_EN is cleared in TxCONFIG and raw is set.
4. If enabled, the line goes high and stays high until INT_CLR.

**QUIRK**: an alarm written in the past fires immediately and *snaps the counter back to the alarm value*. On silicon the counter keeps running.

### 6.4 MWDT

Power-on and reset state:

* stage values (internal) = { 26 000 000, 0x7FFFFFFF, 0x0FFFFFFF, 0x0FFFFFFF }, all stage actions OFF, prescaler 1, clock 80 MHz;
* config0 = 0, so **the MWDT is disabled**. The ROM-enabled flash-boot watchdog of silicon does not exist.

The register mirrors (CONFIG1–5) power up as 0. They do **not** show the internal stage defaults.

WDTCONFIG0 write is ignored entirely unless WPROTECT = 0x50D83AA1. Otherwise:

* config0 = value with bit22 and bits [10:0] cleared.
* If CONF_UPDATE_EN [22] is set:
  * The prescaler is recomputed from the CONFIG1 mirror, but only if its CLK_PRESCALE [31:16] differs from the current prescaler or USE_XTAL [21] changed. Prescaler = 1 if CONFIG1.DIVCNT_RST [0] is set or CONFIG1 == 0. Rate = (USE_XTAL ? 40 MHz : 80 MHz) / prescaler. A CLK_PRESCALE of 0 with other bits set divides by zero.
  * Stage actions come from STG0 [30:29], STG1 [28:27], STG2 [26:25] and STG3 [24:23]: 0 off, 1 interrupt, 2 CPU reset, 3 system reset.
  * The stage hold values come from the CONFIG2–5 mirrors. If the WDT stays enabled and the current stage's value changes, that stage is re-armed for (new − elapsed). If that is ≤ 0, the stage timer is stopped until the next feed.
* EN [31]: 0→1 arms stage 0 for stage[0] ticks; 1→0 cancels.

Without CONF_UPDATE_EN, the stage and prescaler registers have no effect.

At expiry of the current stage:

* **Interrupt**: go to the next stage (3 wraps to 0), arm it, set raw, raise the line if enabled.
* **Off**: go to the next stage and arm it for stage[next] − elapsed.
* **CPU or system reset**: pulse the reset output (unconnected on the S3) and stop. Nothing more happens until fed or reconfigured.

The `wdt_disable` property (default false) makes expiry a no-op.

### 6.5 RTC slow-clock calibration

RTCCALICFG write:

1. RDY [15] is cleared from the value.
2. If START [31] or START_CYCLING [12] is set, the measurement completes **instantly**:
   * CLK_SEL [14:13] picks freq: 0 → **136 000 Hz**, 1 → 68 359 Hz (17.5 MHz / 256), 2 → 32 768 Hz, **3 → 0, which divides by zero and crashes QEMU**;
   * max = MAX [30:16];
   * RTCCALICFG1 = (40 000 000 × max / freq) << 7;
   * RDY is set in the stored value and RTCCALICFG2.TIMEOUT [0] is cleared.
3. The value is stored. START stays set as written.

RTCCALICFG2 write: the value is stored without bit0. Bit0 TIMEOUT is then set if TIMEOUT_RST_CNT [6:3] == 0 or < RTCCALICFG.MAX / 10.

The RTC_CNTL slow-clock selection is ignored: CLK_SEL 0 always means 136 kHz.

Reset (every system reset) rewrites the WDT state, the T0/T1 config, raw and ena, and the counts (to 0). It cancels all timers. Alarm, load and latched values and the RTCCALI registers keep their contents.

---

## 7. SYSTIMER: `hw/timer/esp_systimer.c` (+ `esp32s3_systimer.c`)

* Type `esp32s3.systimer`, base **0x60023000**, size **0x100**.
* Comparator n → source **57 + n**, level, high while raw & ena.
* 2 counters (units), 52-bit, **16 MHz**. 3 comparators.

### 7.1 Counter model

On update: ticks = floor(elapsed_ns × 16 / 1000), value = (value + ticks) mod 2^52, base = now.

* **QUIRK**: the fractional tick is thrown away at every update. Updates happen on every OP-update write, LOAD and comparator reprogram. Very frequent polling makes the counter run slow; polling every < 62.5 ns freezes it.
* Unit 0 counts from virtual time 0 at power-on.
* **QUIRK**: a disabled unit freezes as seen through reads, but its base is not refreshed. On re-enable, the first update adds the whole disabled interval.
* **QUIRK**: counter values are **not reset** by system reset.

### 7.2 Registers

| Off | Name | Reset | Read | Write |
|---|---|---|---|---|
| 0x00 | CONF | **0x46000000** (UNIT0_WORK_EN [30], UNIT1_CORE0_STALL_EN [26], UNIT1_CORE1_STALL_EN [25]) | stored raw value | Stores all 32 bits. Takes UNIT0_WORK_EN [30], UNIT1_WORK_EN [29], TARGET0/1/2_WORK_EN [24]/[23]/[22]. The stall bits [28:25] are recorded but **do nothing**; CLK_EN [31] is ignored. An enabled comparator whose unit changed state is reprogrammed. A comparator that just became enabled is reprogrammed; one just disabled has its timer cancelled. |
| 0x04 | UNIT0_OP | – | **0x20000000** (VALUE_VALID [29] always) | bit30 UPDATE: latch unit0 into VALUE_HI/LO (live value if enabled, held value otherwise) |
| 0x08 | UNIT1_OP | – | 0x20000000 | bit30: latch unit1 |
| 0x0C / 0x10 | UNIT0_LOAD_HI [19:0] / LO | 0 (not reset) | stored | stored |
| 0x14 / 0x18 | UNIT1_LOAD_HI / LO | 0 | stored | stored |
| 0x1C / 0x20 | TARGET0_HI [19:0] / LO | 0 | stored *pending* target | stored (pending) |
| 0x24 / 0x28 | TARGET1_HI / LO | 0 | " | " |
| 0x2C / 0x30 | TARGET2_HI / LO | 0 | " | " |
| 0x34 / 0x38 / 0x3C | TARGETn_CONF | 0 | **active** period [25:0], PERIOD_MODE [30], TIMER_UNIT_SEL [31] | PERIOD_MODE, UNIT_SEL and *pending* period stored. If the comparator is enabled and either its unit changed or period mode was just turned on, it is reprogrammed. |
| 0x40 / 0x44 | UNIT0_VALUE_HI [19:0] / LO | 0 | latched | ignored |
| 0x48 / 0x4C | UNIT1_VALUE_HI / LO | 0 | latched | ignored |
| 0x50 / 0x54 / 0x58 | COMPn_LOAD | – | 0 | bit0: active target = pending target, active period = pending period. Reprogrammed if enabled. |
| 0x5C | UNIT0_LOAD | – | 0 | **Any value** (bit0 not checked): unit0 value = LOAD_HI:LO, then enabled comparators on unit0 are reprogrammed |
| 0x60 | UNIT1_LOAD | – | 0 | same for unit1 |
| 0x64 | INT_ENA | 0 | bits [2:0] | Stores bits [2:0] and re-drives lines. **S3 subclass: after any INT_ENA write, every enabled comparator is reprogrammed** (QUIRK, 7.3). |
| 0x68 | INT_RAW | 0 | bits [2:0] | ignored |
| 0x6C | INT_CLR | – | 0 | Each set bit clears raw, then lines are re-driven |
| 0x70 | INT_ST | 0 | raw & ena | ignored |
| 0x74 / 0x78 | REAL_TARGET0_LO / HI [19:0] | 0 | active target | ignored |
| 0x7C / 0x80 | REAL_TARGET1_LO / HI | 0 | " | " |
| 0x84 / 0x88 | REAL_TARGET2_LO / HI | 0 | " | " |
| 0xFC | DATE | – | **0** | ignored |

### 7.3 Comparator scheduling ("reprogram"; runs only while the comparator is enabled)

1. Update the selected unit. If that unit is disabled, cancel the timer and stop.
2. overflow = target < count **and** count − target ≥ 2^51 − 1.
3. One-shot mode, target ≤ count and not overflow: fire in **10 µs** (deliberate deferral). Stop.
4. Otherwise compute ticks:
   * period mode: ticks = active **period**. The first fire comes one period after the reprogram, whatever the target.
   * overflow: ticks = 2^52 − count + target.
   * otherwise: ticks = target − count.
5. Arm at now + ceil(ticks × 1000 / 16) ns.

On fire:

1. The unit is updated and raw = 1, then the line is re-driven.
2. In period mode with the unit enabled: re-arm at previous_expiry + period. This compensates for lateness. REAL_TARGET is never advanced.
3. In one-shot mode nothing is re-armed. The comparator stays enabled.

**QUIRKS**:

* Because of the S3 INT_ENA hook, writing INT_ENA re-fires any enabled one-shot comparator whose target is already past (10 µs later) and restarts the phase of periodic ones.
* TARGETn_CONF reads return the active period, not the one just written.

Reset (every system reset) zeroes the comparators (target, period, enable, raw, ena) and lowers their lines. CONF is restored to 0x46000000 and unit0 marked enabled. Unit1's internal enable flag and both counter values are left as they were.

---

## 8. Interrupt matrix: `hw/xtensa/esp32s3_intc.c`

* Type `misc.esp32s3.intmatrix`, base **0x600C2000**, size **0x1000**.
* PRO CPU block at 0x000–0x7FF, APP CPU block at 0x800–0xFFF. Each block is 512 one-byte map slots exposed as 32-bit registers (slot s at block + 4s).
* There are 512 input lines; sources 0..98 are meaningful.
* Its reset runs only with a peripheral reset (15.4), never on a CPU-only reset.

| Block offset | Name | Reset | Read | Write |
|---|---|---|---|---|
| 0x000–0x188 (slots 0–98) | SOURCE_n_MAP | **6** | stored 5-bit line | Stores value & 0x1F. If the line changed, both the old and the new CPU line are recomputed. |
| 0x18C, 0x190, 0x194, 0x198 | INTR_STATUS_0..3 | – | Bit b of word w = **raw level of source 32w + b** (sources 0–127). Not filtered by mapping; identical in both blocks. | ignored (read-only) |
| 0x19C | CLOCK_GATE | **6** | stored & 0x1F (it is just map slot 103) | stored & 0x1F, no effect. **QUIRK**: silicon reset is 1. |
| 0x1A0–0x7F8 | (slots 104–510) | 6 | stored | stored, no effect |
| 0x7FC | DATE | **6** | stored (slot 511) | stored. **QUIRK**: not a version number. |

So for the APP CPU: maps at 0x800 + 4n, status at 0x98C–0x998, clock gate at 0x99C, date at 0xFFC.

Combining:

* The model keeps the level of every source.
* CPU line L of CPU c is the **OR** of the levels of every source whose map value for CPU c equals L. It is recomputed whenever a source level changes (both CPUs) or a mapping changes.
* Only **changes** of a line are forwarded to the CPU, so a line already high is not raised again.
* Only Xtensa external interrupts can receive a line: **0, 1, 2, 3, 4, 5, 8, 9, 10, 12, 13, 14, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 30, 31**.
* Mapping a source to 6, 7, 11, 15, 16 or 29 (timer, software, profiling) disconnects it. The reset value 6 therefore means "unrouted".
* On the CPU side, a 0→1 transition sets the bit in INTERRUPT. A 1→0 transition clears it only for level-type interrupts. Lines **10, 22, 28 and 30 are edge-type**: they latch until INTCLEAR. Line 14 is the NMI (level 7).
* FROM_CPU software interrupts are ordinary level sources 79..82, driven by the SYSTEM_CPU_INTR_FROM_CPU_n bit0 (section 1). Route them through these maps like any other source.

Reset clears all source levels, sets every map slot to 6 and lowers all CPU lines.

---

## 9. UART0/1/2: `hw/char/esp32s3_uart.c` on top of `hw/char/esp32_uart.c` (`esp_soc.uart`)

* Type `esp32s3_soc.uart`, bases **0x60000000**, **0x60010000** and **0x6002E000**.
* Region size **0x7C** (the ESP32 register file). On the S3, DATE at 0x7C and ID at 0x80 fall to the catch-all and read 0.
* IRQ → source **27 / 28 / 29**, level.
* chardev `serial0/1/2` (`-serial`). Property `rx-overflow`, default false.
* The FIFOs are 128 bytes each. The register layout is the ESP32's, with S3 fixes for CONF0, CONF1 and MEM_CONF. Several S3 offsets therefore hit ESP32 semantics, flagged below.
* Reset only with a peripheral reset (15.4).

| Off | S3 name | QEMU behaviour |
|---|---|---|
| 0x00 | FIFO | **Read**: pops one RX byte. If the RX FIFO is empty (count 0 and rd == wr), returns **0xEE** and logs an error. After an rx-overflow the pointers can differ with count 0; reads still advance. **Write**: pushes byte [7:0] to TX and immediately pushes everything it can to the chardev, with no baud delay. If the chardev is not open, the TX FIFO is discarded. If it is full (128), the byte is dropped with an error. |
| 0x04 | INT_RAW | Recomputed on every update (below). Writes ignored. |
| 0x08 | INT_ST | = INT_RAW & INT_ENA, recomputed on every update. Writes ignored. |
| 0x0C | INT_ENA | R/W, all 32 bits stored |
| 0x10 | INT_CLR | Write: INT_ST &= ~value; bit8 clears the RX-timeout flag; bit4 clears the overflow flag. The value is stored, so a read returns the last write. INT_ST is recomputed at once, so the level-type bits (FULL, EMPTY, DONE) reappear immediately. |
| 0x14 | CLKDIV | R/W. Reset **0x000002B6**. baud = (40 MHz × 16) / (CLKDIV [19:0] × 16 + FRAG [23:20]), or 115200 if that divisor is 0. Baud is used **only** for RX pacing and RX timeout, never for TX. **QUIRK**: QEMU assumes a 40 MHz UART clock and ignores CLK_CONF; the reset divisor gives 57 636 baud by this formula, while the reset baud variable is set to 115200 directly. |
| 0x18 | RX_FILT (S3) / AUTOBAUD (ESP32) | **QUIRK**: write bit0 = 1 sets the 0x30 register to 0x3FF, bit0 = 0 sets it to 0. Every **CONF0 write** also overwrites this register with CONF0.AUTOBAUD_EN [27] (0 or 1). Reads return that stored value. |
| 0x1C | STATUS | RXFIFO_CNT [7:0] = RX count; TXFIFO_CNT [23:16] = TX count (normally 0, since TX drains instantly). All other bits (RXD, CTSN, DSRN, TXD, RTSN, DTRN) read **0**. Writes ignored. |
| 0x20 | CONF0 | R/W stored. Side effects: AUTOBAUD as above. **RXFIFO_RST [17] = 1** empties the RX FIFO, cancels the RX timeout timer and re-opens chardev input; this repeats on every write that has bit17 set. TXFIFO_RST [18], loopback, parity, bits, stop and inversion are ignored. |
| 0x24 | CONF1 | Stored in an S3 mirror, which is what reads return. Takes RXFIFO_FULL_THRHD [9:0], TXFIFO_EMPTY_THRHD [19:10] and RX_TOUT_EN [23], then re-arms the RX timeout (see the quirk below) and updates the IRQ. The mirror is not cleared by reset, but the thresholds are (to 0). |
| 0x28, 0x2C | LOWPULSE, HIGHPULSE | Read constant **337** (0x151). Writes stored but not readable. |
| 0x30 | RXD_CNT | Reads the stored value: 0x3FF after AUTOBAUD = 1, else 0, or whatever was written. |
| 0x34–0x54 | FLOW_CONF … AT_CMD_POSTCNT | Plain stored R/W, no behaviour |
| 0x58 | AT_CMD_GAPTOUT (S3) | **QUIRK**: reads the constant **0x88** (the ESP32 MEM_CONF RX_SIZE = TX_SIZE = 1). Writes stored but not readable. |
| 0x5C | AT_CMD_CHAR | stored R/W |
| 0x60 | MEM_CONF (S3) | **Write**: only RX_TOUT_THRHD [26:17] is taken (the RX timeout threshold, in bit times); nothing is stored. **Read** (**QUIRK**, ESP32 MEM_RX_STATUS semantics): (rx_rd << 2) \| (rx_wr << 13), the two RX FIFO pointers. RX_SIZE, TX_SIZE and FORCE_PU/PD read as garbage. |
| 0x64, 0x68, 0x6C, 0x70, 0x74 | MEM_TX_STATUS, MEM_RX_STATUS, FSM_STATUS, POSPULSE, NEGPULSE | Plain stored R/W, initial 0. The real RX pointers are **not** here. |
| 0x78 | CLK_CONF (S3) | **QUIRK**: reads the constant **0x15122500** (the ESP32 UART_DATE). Writes stored but not readable. sclk_sel, divider and rst_core are ignored. |

INT_RAW is recomputed after every register write, RX event, TX drain, FIFO read and timeout. The interrupt line = (INT_RAW & INT_ENA) ≠ 0.

| Bit | Name | Condition |
|---|---|---|
| 0 | RXFIFO_FULL | rx_count **≥** RXFIFO_FULL_THRHD |
| 1 | TXFIFO_EMPTY | tx_count **≤** TXFIFO_EMPTY_THRHD |
| 4 | RXFIFO_OVF | sticky flag: a byte overwrote the FIFO (rx-overflow mode only). Cleared by INT_CLR bit4 or reset. |
| 8 | RXFIFO_TOUT | sticky flag set by the timeout timer. Cleared by INT_CLR bit8, by new input, or by writing CONF1 with RX_TOUT_EN = 0. |
| 14 | TX_DONE | tx_count == 0 |

All other bits read 0.

* **QUIRK**: bits 0, 1 and 14 are pure *levels*, not latched events. INT_CLR cannot clear them while the condition holds.
* **QUIRK**: with the reset thresholds (0), RXFIFO_FULL is always 1. Right after reset INT_RAW reads 0, but the first update makes it **0x00004003**.

RX path:

* The chardev feeds bytes into the 128-byte RX FIFO.
* Default mode, rx-overflow false: QEMU only accepts as many bytes as there is room for. Once the FIFO reaches 128, input is paused for 128 × 10 bit times at the computed baud.
* rx-overflow true: bytes are paced at 10 bits per byte. A byte arriving while the FIFO is full overwrites the oldest slot and sets OVF.
* RX timeout: if RX_TOUT_EN, a timer fires `thres × 1e9 / baud` ns after the last input, or after the paced end of input. It sets the TOUT flag.
* **QUIRK**: writing CONF1 with RX_TOUT_EN = 1 also arms the timer **even with no data received**. The reset threshold is 0, so TOUT then fires at once.

Reset clears all registers (CLKDIV back to 0x2B6), both FIFOs, flags, thresholds and timers, and lowers the IRQ.

---

## 10. USB-Serial-JTAG: `hw/misc/esp32c3_jtag.c` (type `misc.esp32c3.usb_serial_jtag`)

* Base **0x60038000**, size **0x84**. **Every read returns 0. Every write is ignored.** Reset does nothing. No IRQ (source 96 never asserted). No chardev.
* It exists only so accesses do not fault.
* **Gotcha**: EP1_CONF (0x04) SERIAL_IN_EP_DATA_FREE [1] reads 0, so a driver waiting for TX FIFO space spins forever or times out. EP1 data writes vanish. INT_RAW reads 0.

---

## 11. GPIO: `hw/gpio/esp32s3_gpio.c` on top of `hw/gpio/esp32_gpio.c` (`esp32.gpio`)

* Type `esp32s3.gpio`, base **0x60004000**, MMIO region 0 of size **0x1000**. It also covers GPIO_SD at 0x60004F00, which reads 0.
* Only region 0 is mapped. The model's second region (an IO_MUX shadow) is **not mapped** by the S3 machine.
* The IRQ is not connected.

| Off | Name | Read | Write |
|---|---|---|---|
| 0x038 | GPIO_STRAP | **0x00000004** (property `strap_mode`, S3 default "SPI boot") | ignored |
| anything else | OUT, OUT_W1TS/W1TC, ENABLE*, IN (0x3C), IN1 (0x40), STATUS, PINn, FUNCn_IN/OUT_SEL… | **0** | ignored |

The base class has loop-back logic for OUT, ENABLE and IN (IN = (OUT & ENABLE) | (pull-up & ~ENABLE)). It is **disabled on the S3**: the register offsets it needs are left at 0. Override the strap with `-global driver=esp32s3.gpio,property=strap_mode,value=N`. QEMU itself never interprets the strap; the ROM does.

IO_MUX (0x60009000, 0x2000) is an `unimplemented-device` at priority −1000, under the catch-all. It reads 0 and ignores writes. So pull-up bits written there have no effect on GPIO_IN.

---

## 12. SHA: `hw/misc/esp_sha.c` + `hw/misc/esp32s3_sha.c`

* Type `misc.esp32s3.sha`, base **0x6003B000**, size **0x100**. IRQ → source **78**.
* All operations finish **synchronously** inside the triggering write.

| Off | Name | Read | Write |
|---|---|---|---|
| 0x00 | MODE | stored [2:0] | value & 7. 0 SHA-1, 1 SHA-224, 2 SHA-256, 3 SHA-384, 4 SHA-512, 5 SHA-512/224, 6 SHA-512/256, 7 SHA-512/t. Not reset. |
| 0x04 | T_STRING | 0 | stored byte-swapped (for mode 7) |
| 0x08 | T_LENGTH | 0 | stored as bswap32(value & 0x7F) (for mode 7) |
| 0x0C | DMA_BLOCK_NUM | stored | value & 0x3F |
| 0x10 | START | 0 | bit0: init + one block (below) |
| 0x14 | CONTINUE | 0 | bit0: state from H_MEM + one block |
| 0x18 | BUSY | **0 always** | ignored |
| 0x1C | DMA_START | 0 | bit0: init + DMA_BLOCK_NUM blocks from GDMA |
| 0x20 | DMA_CONTINUE | 0 | bit0: state from H_MEM + DMA blocks |
| 0x24 | CLEAR_IRQ | 0 | **any value** lowers the IRQ |
| 0x28 | IRQ_ENA | bit0 | bit0 |
| 0x2C | DATE | **0x20190402** | ignored |
| 0x40–0x7F | H_MEM[0..15] | bswap32(hash word i) | hash word i = bswap32(value). Non-32-bit writes log an error but are still applied. |
| 0x80–0xFF | M_MEM[0..31] | stored | stored raw |

Algorithms (sizes in bytes):

| Mode | Block | Digest state | H_MEM words used |
|---|---|---|---|
| 0 SHA-1 | 64 | 5 words | 0–4 |
| 1 SHA-224, 2 SHA-256 | 64 | 8 words | 0–7 |
| 3–7 (SHA-384 and SHA-512 family) | 128 | eight 64-bit words | 0–15: word 2i = high half and word 2i+1 = low half of state[i] |

Byte order:

* Each H_MEM word read is the byte-swapped native state word. Reading H_MEM as bytes therefore gives the digest in canonical big-endian order. Writing it back the same way restores the state.
* M_MEM is consumed as raw bytes in memory order: byte 0 of word 0 is message byte 0.

Operations:

* **START**: initialise the context to the standard IV of the mode. SHA-384 is initialised with its own IV but compressed with the SHA-512 function. Then compress M_MEM block 0 and write the state to H_MEM.
* **Mode 7 (SHA-512/t) START** differs:
  1. The IV is the SHA-512 IV XOR 0xA5A5A5A5A5A5A5A5.
  2. **M_MEM is overwritten** with the IV-generation block: word0 = 0x2D414853, word1 = 0x2F323135 ("SHA-512/" as bytes), word2 = the stored T_STRING, words 3–30 = 0, word31 = the stored T_LENGTH.
  3. That block is compressed, which leaves the SHA-512/t IV in H_MEM.
  4. User data then goes through CONTINUE.
* **CONTINUE**: load the state from the current H_MEM words, compress one M_MEM block, write H_MEM.
* **DMA_START / DMA_CONTINUE**:
  1. Same initialisation.
  2. Find the GDMA OUT channel whose peripheral selection is SHA (warn and abort if none).
  3. Read DMA_BLOCK_NUM × block bytes from it and compress them all.
  4. Write H_MEM.
  5. **If IRQ_ENA, raise the IRQ.** It stays high until CLEAR_IRQ.
* Register-mode START and CONTINUE never raise the IRQ.

No padding is done in hardware: software pads, as on silicon.

Reset (every system reset): H_MEM words 0–7 and M_MEM zeroed, DMA_BLOCK_NUM = 0, IRQ_ENA = 0, IRQ lowered. MODE and H_MEM 8–15 are kept.

---

## 13. RNG: `hw/misc/esp32s3_rng.c`

* Type `misc.esp32s3.rng`, one 4-byte register at **0x6003507C** (WDEV + 0x7C). The header's `DR_REG_WDEV_BASE 0x3FF75000` is a stale ESP32 value and is not used for mapping.
* **Read**: a fresh 32-bit value from the host RNG (`qemu_guest_getrandom_nofail`) on every access. It is deterministic only with `-seed`.
* **Write**: the model has **no write handler at all**. A guest store here makes QEMU call a NULL callback and crash. Never write it.
* No dependence on Wi-Fi, ADC or the RC_FAST clock enables: it is always random.
* The rest of WDEV (0x60035000–0x60035FFF) reads 0 through the catch-all.

---

## 14. PMS / SENSITIVE: `hw/misc/esp32s3_pms.c`

* Type `misc.esp32s3.pms`, base **0x600C1000**, size **0x1000**: a 1024-word scratch array.
* Every offset reads back the last 32-bit value written there. The power-on value is **0 everywhere**, including registers whose silicon reset is non-zero (CLOCK_GATE = 1, DATE, the cache tag access defaults…).
* No reset handler: values survive system resets.
* No permission checking, no lock behaviour, no violation interrupts (sources 84–95 never asserted).

---

## 15. Machine-level behaviour: `hw/xtensa/esp32s3.c`

### 15.1 CPUs

* Machine `esp32s3`: max 2 CPUs, default 2 (`-smp 1` builds only CPU0). CPU type `esp32s3-xtensa-cpu`, config `core-esp32s3`, CCOUNT clock 40 MHz.
* PRID is set at creation and is not changed by CPU reset: CPU0 (PRO) **0xCDCD**, CPU1 (APP) **0xABAB**.
* Each CPU has a private address space: its own IROM copy at 0x40000000, then the system bus.
* The DROM alias at 0x3FF00000 is added once per CPU onto the *shared* system bus. The second addition wins, so 0x3FF00000 shows **CPU1's** ROM copy at offset 0x40000 to both CPUs.
* Reset vector:
  * static vector 1 = **0x40000400**, static vector 0 = **0x50000000**.
  * At every per-core reset QEMU selects the vector from RTC_CNTL RESET_STATE PRO/APPCPU_STAT_VECTOR_SEL, which is 1 at power-on. Cores therefore start at 0x40000400.
* At each core reset, QEMU additionally forces **CPENABLE = 0xFF**.
* PS = 0x1F, VECBASE = the config default, and ATOMCTL, MEMCTL and LITBASE get their generic Xtensa reset values.

### 15.2 Fixed contents

`esp32s3.apbctrl` is plain **RAM** at 0x60026000–0x600263FF (where SYSCON / APB_CTRL lives on silicon). It is written once at realize:

* 0x6002607C = **0x96042000** (0x16042000 with bit31 set). This copies the ESP32 APB_CTRL_DATE "ECO3" convention; on the S3 that offset is not the date register.
* 0x600263F8 = **0x51454D55** ("QEMU", the QEMU-detection signature).

Everything else there is RAM (0 at start, R/W, not reset). SYSCON_DATE at 0x3FC reads 0.

### 15.3 `esp32s3.iomem` catch-all

* An MMIO region at 0x60000000, size **0xD1000** (to 0x600D0FFF), added before the devices. Devices added later win over it at equal priority.
* **Read**: 0, except that an access whose start address is exactly **0x6000E040** (I2C_MST_ANA_CONF0) returns **0x01000000** (BBPLL_CAL_DONE, bit24). The PLL calibration therefore always reads "done".
* **Write**: ignored.
* Nothing else of the regi2c analog block (0x6000E000) is modelled. The I2C master busy bit reads 0, so the ROM's regi2c read and write helpers "succeed" and return 0 data.

### 15.4 Resets

SoC reset requests carry a mask: PROCPU = 1, APPCPU = 2, PERIPH = 4, RTC = 8. DIG = 7 and ALL = 15. On every QEMU system reset, `esp32s3_soc_reset` runs:

* mask = ALL if nothing was requested (power-on, monitor `system_reset`);
* if PERIPH: cold-reset the interrupt matrix and the 3 UARTs;
* if PROCPU: reset core 0; if APPCPU: reset core 1;
* end every pending cross-core hold, then re-apply the RTC stall levels.

**Independently, QEMU resets every sysbus device on every system reset, whatever the mask.** That covers SYSTEM/clock, SPI1 and the flash, EXTMEM/MMU, eFuse, TIMG0/1, SYSTIMER, GPIO, SHA, GDMA, AES, RSA, XTS-AES, TWAI and SDMMC. RTC_CNTL (on the rtc bus) and the RTC RAMs are never reset. The SoC object itself has no bus.

Sources of reset:

* **RTC_CNTL SW_SYS_RESET** (OPTIONS0 bit31): mask = DIG, then a system reset (cause GUEST_RESET; `-no-reboot` makes QEMU exit).
* **RTC_CNTL SW_PROCPU_RESET / SW_APPCPU_RESET** (bits 5 and 4):
  * *Issued by the other core*: only that core is reset, at once, on its own thread. It is then **held stalled for 20 ms of virtual time** (`cpu_reset_hold` timer) before it may run. This models IDF restart, which resets and then stalls the other core. Devices are not reset.
  * *Issued by the core itself, or from outside any CPU*: a full QEMU system reset with mask = PROCPU or APPCPU. So **all sysbus devices are reset**; the UARTs and the interrupt matrix are not. The cause is GUEST_RESET, except an APP-CPU reset requested from outside a CPU, which uses SUBSYSTEM_RESET so that it does not end a `-no-reboot` run.
* **TIMG MWDT reset stages**: unconnected, so nothing happens. There is no RTC WDT. **No watchdog can reset the S3 machine.**
* **CPU stall**: the RTC_CNTL stall level, or a pending reset-hold timer, sets the Xtensa runstall. Stalls survive the stalled core's resets. Releasing a stall also drops a halt request the core had not yet taken.

### 15.5 APP CPU handling

* There is **no APP CPU gating**: SYSTEM_CORE_1_CONTROL_0 is unmodelled. Both cores leave reset together and both start at 0x40000400.
* With the real ROM, the APP core runs the ROM's APP path immediately: it waits for SYSTEM_CORE_1_CONTROL_1, which QEMU stores and reads back.
* With `-kernel` or `-bios` ELF boot, CPU1's ROM copy is empty (zero-filled), so the APP CPU executes zeros from 0x40000400 (illegal-instruction exceptions) unless the guest stalls it through RTC_CNTL.

### 15.6 ROM boot vs ELF boot

* An MMU reset is done first (`cpu_reset(cpu0)`) so that ELF segments can be placed.
* If `-kernel` is given (it wins over `-bios`), or else `-bios`, the file is loaded as an **Xtensa ELF** through CPU0's address translation, into CPU0's view. If the entry point ≠ 0x40000400, a 14-byte stub is placed at 0x40000400 in CPU0's view only: `j` over a literal holding the entry, then `l32r a0, <entry>; jx a0`. CPU0's PC is set to 0x40000400. No ROM is loaded: ROM functions do not exist.
* Otherwise `esp32s3_rev0_rom.bin` is looked up in QEMU's firmware search path and loaded at 0x40000000 (up to 0x60000 bytes) into **each** CPU's private IROM. A missing file is fatal.

### 15.7 Other machine options

* **Flash**: `-drive if=mtd` (2, 4, 8 or 16 MiB; section 3.4). It gives the m25p80 on SPI1 CS0 and the backing for the cache MMU.
* **PSRAM**: `-m N` rounds up to 2, 4, 8, 16 or 32 MiB (0 means no PSRAM). It gives an SSI PSRAM on SPI1 CS1 and the PSRAM target of MMU TYPE = 1 entries.
* **SD**: `-drive if=sd` gives an SD card on the DWC SDMMC.
* **Ethernet**: `-nic` gives OpenCores Ethernet, registers at 0x600CD000, descriptors at 0x600CD400, IRQ on source 0.
* **eFuse file**: `-drive file=…,if=none,id=efuse -global driver=nvram.esp32s3.efuse,property=drive,value=efuse`. Without it, all-zero eFuses (section 5.1).

### 15.8 Memory topology caveats worth mirroring or deliberately not mirroring

* **QUIRK**: "DRAM" (0x3FC80000, 1.4375 MiB) and "IRAM" (0x40370000, 512 KiB) are two **independent** RAMs. On silicon, SRAM1 is visible on both buses (D address = I address − 0x6F0000). In QEMU, code written through a 0x3FCx_xxxx pointer is not visible at 0x4037_xxxx. The DRAM range is also larger than the silicon's.
* The GDMA's view contains only DRAM and the DCache window. IRAM, ROM, RTC RAM and peripherals are invisible to DMA, which gets a descriptor error.
