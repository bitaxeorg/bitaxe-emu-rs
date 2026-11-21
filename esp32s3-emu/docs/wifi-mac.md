# ESP32-S3 Wi-Fi MAC: what the driver programs

What the Espressif Wi-Fi driver (the closed `libpp`/`libnet80211` libraries and the ROM functions
they call) reads and writes in the ESP32-S3's Wi-Fi MAC, gathered to emulate it. The MAC is not
documented by Espressif.

## Method and sources

- **Traces with function names.** `BITAXE_EMU_MMIO_LOG=1` logs every peripheral access with the PC
  of the instruction. The esp-miner-rs ELF links the Wi-Fi libraries statically with their
  symbols, so `addr2line` names the function behind each access (`hal_mac_rx_set_base`,
  `lmacSetTxFrame`, ...). Trace: esp-miner-rs `baab160` on board 601, 15 emulated seconds, no Wi-Fi
  network set (the setup access point starts and sends a beacon). The libraries are those of
  `esp-wifi-sys-esp32s3` 0.3.0, used by esp-radio 1.0.
- **Disassembly** of the same ELF with radare2 (`pdf` of the named functions), for the addresses
  the traced run does not reach.
- **The ESP32 map** of the esp32-open-mac project (its QEMU fork, `hw/xtensa/esp32_wifi.c`,
  GPL-2.0: read for the register meanings only, no code taken). The ESP32-S3 MAC follows the same
  layout for most of the receive side and the address filters; the transmit queues and interrupt
  registers moved.

Confidence: **traced** (seen in the trace, with the function), **disassembled** (read in the
function's code), **ESP32** (from the ESP32 map, not checked on the S3 yet).

## Blocks

| Base | Size | Contents |
|---|---|---|
| `0x6003_3000` | `0x1000` | MAC: address filters, receive DMA, receive policies, auto-ACK, transmit queues, interrupts |
| `0x6003_4000` | `0x1000` | MAC continued: transmit slot settings (PLCP, HT-SIG), a table at `0x540`–`0x6cc` (written once each at init) |
| `0x6003_5000` | `0x1000` | TSF timer and status (`0x000` polled by `wait_freq_set_busy`, `0x07c` random value of `hal_random`) |

Offsets below are from `0x6003_3000`.

## Address filters (ESP32 layout, traced)

| Offset | Register | Writer |
|---|---|---|
| `0x000`/`0x004`, `0x008`/`0x00c` | BSSID of interfaces 0 and 1 (low 32 bits, high 16 bits) | `hal_mac_set_bssid` |
| `0x020`/`0x024`, `0x028`/`0x02c` | BSSID filter masks | `hal_mac_set_bssid`, `hal_mac_rx_set_policy` |
| `0x040`/`0x044`, `0x048`/`0x04c` | MAC address of interfaces 0 and 1 | `hal_mac_set_addr` |
| `0x060`/`0x064`, `0x068`/`0x06c` | MAC address filter masks | `hal_mac_set_addr`, `hal_mac_rx_set_policy` |
| `0x0d8`–`0x0e4` | Receive policies | `hal_mac_rx_set_policy` |
| `0x100`/`0x104` | Promiscuous masks | |
| `0x400`–`0x41c` | Auto-ACK setup, low rates | `hal_mac_rate_autoack_init`, `hal_mac_disable_low_rate` |

## Receive (disassembled, matches the ESP32)

| Offset | Meaning |
|---|---|
| `0x080` | Receive buffer setup (`mac_rxbuf_init` writes `0xffffff00`) |
| `0x084` | Bit 31: receive on (`ic_enable_rx`). Bit 0: reload the descriptor list, set by the driver after appending buffers (`hal_mac_rx_set_dscr_reload`), cleared by the MAC (`hal_mac_rx_is_dscr_reload` polls it) |
| `0x088` | Address of the first receive descriptor (`hal_mac_rx_set_base`; `0x3fca35f4` in the trace) |
| `0x08c` | Next descriptor the MAC will fill (`hal_mac_rx_read_rxdscrnext`) |
| `0x090` | Low 20 bits of the last descriptor filled; `hal_mac_rx_get_last_dscr` takes bits 31:20 from `0xc64` |
| `0xc5c`, `0xc60`, `0xc64` | Written by `mac_rxbuf_init` (`0xffff8000`, `0xfff84000`, `0x3fcfffff`); `0xc64` holds the high address bits of the descriptors |

Descriptors (ESP32): `size` bits 11:0, `length` 23:12, `eof` bit 30, `owner` bit 31 (set: the MAC
may fill it), then the buffer address and the next descriptor's address. On the ESP32 a received
buffer starts with a 28-byte `wifi_pkt_rx_ctrl_t` followed by the 802.11 frame. On the S3 that
structure is 48 bytes (`esp-wifi-sys-esp32s3` 0.3.0 bindings); **that the hardware header is the
same 48 bytes is still to be checked** in `wDev_ProcessRxSucData`.

## Transmit (disassembled and traced)

- **Queue `q`** (0 to about 4) has one register at `0xd08 − 8 × q`. The driver writes the frame's
  first descriptor as `(address & 0xfffff) | 0x0060_0000` (a ROM function, PC `0x4003b56f`:
  `0x006aa738` for the beacon at `0x3fca_a738`), then `hal_mac_txq_enable(q)` sets bits 31:30.
  The beacon used queue 0.
- **Per-queue settings** around `0x12xx` (`0x6003_4000` page offsets `0x230`, `0x27c`, `0x2c8`,
  `0x314`: 0x4c apart): `mac_tx_set_plcp1` writes `0x12f8`/`0x12fc`/`0x1314`, `mac_tx_set_htsig`
  `0x1310`/`0x1314`, `hal_set_tx_pti` `0x12fc`, `mac_tx_set_txop_q` `0x1314`. Their meaning
  matters to the radio only.
- **`0xd04`**: transmit timeout and EDCA settings (`hal_mac_tx_config_timeout`,
  `hal_mac_tx_config_edca`).
- **Queue state**: `0xca8` (bits 10:0 and 23:16, `hal_mac_get_txq_state`), `0xcb0`; cleared by
  writing `0xca4` (`hal_mac_clr_txq_state`). On the ESP32 these were the "complete" and
  "collision/timeout" states.

## Interrupts (disassembled)

- The MAC is interrupt source 0 (`WIFI_MAC_INTR`); esp-radio's `set_isr` routes it to CPU
  interrupt 1. The handler is `wDev_ProcessFiq`.
- **Status** `0xc3c` (`hal_mac_interrupt_get_event`), **clear** by writing the bits to `0xc40`
  (`hal_mac_interrupt_clr_event`). The ESP32 has them at `0xc48`/`0xc4c`.
- **Bits**, from `wDev_ProcessFiq`: 0 and 10 a frame received (`lmacProcessRxSucData`), 7 transmit
  complete (`lmacPostTxComplete`), 19 transmit timeout (`lmacProcessAllTxTimeout`), 8 collision
  (`lmacProcessCollisions`), 1–4 TBTT (`wdev_process_tbtt`), 5–8 TSF timers
  (`wdev_process_tsf_timer`), 15 beacon filter. A second status comes from
  `hal_pwr_interrupt_get_event` (power management).

## TSF (traced)

`0x200c` control (`hal_get_tsf_time` writes 2 before reading, 0 after; `hal_mac_tsf_reset` writes
`0x20`), `0x2018`/`0x201c` the time in µs (low, high), `0x2028` station TSF enable
(`hal_enable_sta_tsf` writes `0x98000000`). `lmacSetTxFrame` and the beacon code read `0x2000`.

## Still to find

- That the receive metadata header is the 48-byte `wifi_pkt_rx_ctrl_t`, and the fields the driver
  checks (`wDev_ProcessRxSucData`).
- Which bits of `0xca8` say a queue completed, and what `lmacPostTxComplete` reads.
- What the `0x200c` latch and `0x2000` hold, so the TSF counts.
- The power-management interrupt source and register (`hal_pwr_interrupt_get_event`).
- The same trace with ESP-Miner (ESP-IDF 6.0.2's libraries), to check the map holds for both.
