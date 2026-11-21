# ESP32-S3 Wi-Fi MAC: what the driver programs

What the Espressif Wi-Fi driver (the closed `libpp`/`libnet80211` libraries and the ROM functions
they call) reads and writes in the ESP32-S3's Wi-Fi MAC, gathered to emulate it
(`src/devices/wifi_mac.rs`). The MAC is not documented by Espressif.

## Method and sources

- **Traces with function names.** `BITAXE_EMU_MMIO_LOG=1` logs every peripheral access with the PC
  of the instruction. The esp-miner-rs ELF links the Wi-Fi libraries statically with their
  symbols, so `addr2line` names the function behind each access (`hal_mac_rx_set_base`,
  `lmacSetTxFrame`, ...). The libraries are those of `esp-wifi-sys-esp32s3` 0.3.0, used by
  esp-radio 1.0.0-beta.1.
- **Disassembly** of the same ELF with radare2 (`pdf` of the named functions), for the paths the
  traces do not reach, and of the ROM (`esp32s3_rev0_rom.bin` at 0x4000_0000): the ROM functions
  the libraries call are entries of a jump table (`l32r a9, <body>; jx a9`), named after the
  ELF's absolute symbols.
- **The ESP32 map**, in the `esp32` PAC (MIT/Apache, register block `wifi`, from the
  esp32-open-mac project) and the esp32-open-mac QEMU fork (GPL-2.0, read for the meanings only,
  no code taken). The ESP32-S3 MAC keeps the ESP32 layout for the address filters and the receive
  DMA; its interrupt registers sit 0xc lower and its transmit queue registers 0x18 lower.
- **Running the model** against esp-miner-rs (station joining an access point, setup access point
  with a client, TCP both ways) and ESP-Miner releases.

Confidence: **traced** (seen in a trace, with the function), **disassembled** (read in the
function's code), **run** (the driver works against the model as described), **ESP32** (from the
ESP32 map only).

## Blocks

| Base | Size | Contents |
|---|---|---|
| `0x6003_3000` | `0x1000` | MAC: address filters, receive DMA, receive policies, auto-ACK, interrupts, transmit queues |
| `0x6003_4000` | `0x1000` | MAC continued: per-queue transmit settings (PLCP, HT-SIG, PMD), a table at `0x540`–`0x6cc` written once at init |
| `0x6003_5000` | `0x1000` | TSF timers, power-management interrupts, status (`0x000` polled by `wait_freq_set_busy` until 0, `0x07c` a random number for `hal_random`) |

Offsets below are from `0x6003_3000`. The MAC is reset by APB_CTRL `WIFI_RST_EN` (`0x6002_6018`)
bit 2 (MAC_RST), which `wifi_reset_mac` pulses (run).

## Control (disassembled, run)

`0xd14`: `hal_init` sets bit 1 and waits for bit 0 (MAC ready).

## Address filters (ESP32 layout, traced)

| Offset | Register | Writer |
|---|---|---|
| `0x000`/`0x004`, `0x008`/`0x00c` | BSSID of interfaces 0 and 1 (low 32 bits, high 16 bits) | `hal_mac_set_bssid` |
| `0x020`/`0x024`, `0x028`/`0x02c` | BSSID filter masks | `hal_mac_set_bssid`, `hal_mac_rx_set_policy` |
| `0x040`/`0x044`, `0x048`/`0x04c` | Receive address (MAC) of interfaces 0 and 1 | `hal_mac_set_addr` |
| `0x060`/`0x064`, `0x068`/`0x06c` | Their masks; bit 16 of the high word enables the filter | `hal_mac_set_addr`, `hal_mac_rx_set_policy` |
| `0x0d8`–`0x0e4` | Receive policies of the interfaces | `hal_mac_rx_set_policy` |
| `0x100`/`0x104` | Promiscuous masks | |
| `0x400`–`0x41c` | Auto-ACK setup, low rates | `hal_mac_rate_autoack_init`, `hal_mac_disable_low_rate` |

## Receive (disassembled, run)

| Offset | Meaning |
|---|---|
| `0x080` | Receive buffer setup (`mac_rxbuf_init`) |
| `0x084` | Bit 31: receive on (`ic_enable_rx`; cleared by `wifi_reset_mac`). Bit 0: reload the descriptor list, set by `wDev_AppendRxBlocks` after linking new descriptors to the last one, polled by `hal_mac_rx_is_dscr_reload` until the MAC clears it |
| `0x088` | Address of the first receive descriptor (`hal_mac_rx_set_base`) |
| `0x08c` | Next descriptor the MAC fills (`hal_mac_rx_read_rxdscrnext`); with the list run out, only the high address bits |
| `0x090` | Low 20 bits of the last descriptor filled |
| `0xc5c`, `0xc60`, `0xc64` | Written by `mac_rxbuf_init`; `0xc64` bits 31:20 complete the descriptor addresses of `0x08c`/`0x090` (`0x3fc` for internal RAM) |

Descriptors: word 0 holds the buffer size (bits 11:0), the length (23:12), SUC_EOF (30) and the
DMA owner bit (31); then the buffer address and the next descriptor (0 at the end).
`wDev_AppendRxBlocks` hands a descriptor back with the owner bit set, SUC_EOF and bit 29 clear,
the length equal to the size, and a `0xdeadbeef` canary at the start of the buffer and just past
its end.

The ROM's `wdevProcessRxSucDataAll` walks the driver's list from its head up to the descriptor of
`0x090`, one frame per run of descriptors ending with SUC_EOF; it does not look at the owner bit.
A frame in a buffer is a 48-byte header then the MPDU without FCS; the descriptor length counts
both (run). The header is `wifi_pkt_rx_ctrl_t` of the S3 (48 bytes in the `esp-wifi-sys-esp32s3`
bindings); the fields the driver uses:

| Word | Bits | Field |
|---|---|---|
| 0 | 7:0 | RSSI (dBm, signed) |
| 0 | 12:8 | Rate (`wifi_phy_rate_t`, 11 = 6 Mb/s) |
| 0 | 15:14 | Signal mode (0 non-HT, 1 HT) |
| 0 | 28 + i | The frame passes the receive filter of interface i (esp-wifi-hal on the ESP32; `wDev_ProcessRxSucData` tests bits 29:28) |
| 1 | 19:8 | HT length, which `wDev_ProcessRxSucData` takes for HT frames in its sniffer path |
| 2 | 19:16 | Channel |
| 3 | 31:0 | Timestamp (µs) |
| 5 | 7:0 | Noise floor |
| 11 | 11:0 | Signal length: the MPDU with its FCS |
| 11 | 31:24 | Receive state: 0 for a good frame (`wDev_ProcessRxSucData` treats others, but 245, as errors) |

A frame is announced by interrupt bit 24 (run; bit 14 too in `wDev_ProcessFiq`).

## Transmit (disassembled, run)

- **Queue `q`** (0 to 4) has a PLCP0 register at `0xd08 − 8 × q` and a config register (timeout,
  backoff, AIFSN, as on the ESP32) at `0xd04 − 8 × q`. The driver writes the low 20 bits of the
  frame's first descriptor with bits 22:21 (and bit 24, wait for the ACK, for unicast), then
  `hal_mac_txq_enable(q)` sets bits 31:30. A transmission is one descriptor whose length counts
  the FCS the MAC appends.
- **Per-queue settings** at `0x12xx`, 0x4c apart going down from queue 0: PLCP1 at
  `0x12f8 − 0x4c × q` (length with FCS in bits 11:0), `mac_tx_set_htsig` at `0x1310`/`0x1314`,
  `hal_set_tx_pti` at `0x12fc`, and the **PMD** (result) at `0x1320 − 0x4c × q`
  (`hal_mac_get_txq_pmd`).
- **Completion**: `0xcb0` bit q (`hal_mac_get_txq_state(2)`, bits 3:0), cleared by writing the bit
  to `0xcac`; interrupt bit 7 → `lmacPostTxComplete` (ROM: posts event 23 to the pp task) →
  `lmacProcessTxComplete`, which for each completed queue in use reads the PMD and dispatches on
  its bits 15:12: 0 success (`lmacProcessTxSuccess`, with bits 23:16), 1 RTS error, 2 CTS timeout,
  4 and 5 other errors. Errors: `0xca8` collision bits 10:0 and timeout bits 23:16, cleared by
  writing `0xca4`.

## Interrupts (disassembled, run)

- The MAC is interrupt source 0; esp-radio's `set_isr` routes it to CPU interrupt 1. The handler
  is `wDev_ProcessFiq`; it loops while either status has bits set.
- **MAC status** `0xc3c`, cleared by writing the bits to `0xc40`: bits 14 and 24 a frame received
  (`lmacProcessRxSucData`, ROM: event 25), 7 transmission done, 19 transmit timeout
  (`lmacProcessAllTxTimeout`), 8 collisions, 15 beacon filter, 21 posts event 14.
- **Power-management status** `0x2118`, cleared at `0x211c` (`hal_pwr_interrupt_get_event`):
  bits 1–4 TBTT (`wdev_process_tbtt`), 5–8 TSF timers.
- `hal_mac_is_dma_enable` (ROM) reads `0x2128` and returns 0.

## TSF (disassembled, traced)

`0x200c` control: setting bit 0 (interface 0) or 1 latches the time of that interface's TSF into
`0x2018` (low) and `0x201c` (high), µs, which `hal_get_tsf_time` reads before clearing the bit;
bit 5 resets the TSF (`hal_mac_tsf_reset`). `0x2028` station TSF enable (`hal_enable_sta_tsf`).

## Not modelled yet

- TBTT and TSF timer interrupts: the chip's access point does not beacon on its own, and power
  save (not used by esp-radio by default) would not wake up.
- The crypto engine (key slots at `0x800` on the ESP32): no WPA2.
- Retries, collisions and errors: every transmission succeeds.
