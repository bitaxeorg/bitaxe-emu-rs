//! The firmware logs: defmt frames from RTT up-buffer 0, decoded with the table
//! of the application ELF.

use defmt_decoder::{DecodeError, StreamDecoder, Table};

pub struct Logs {
    rtt: Option<u32>,
    decoder: Option<Box<dyn StreamDecoder + Send + Sync + 'static>>,
}

/// Address of the RTT control block in the ELF.
fn rtt_address(elf: &[u8]) -> Option<u32> {
    let obj = goblin::elf::Elf::parse(elf).ok()?;
    obj.syms
        .iter()
        .find(|s| obj.strtab.get_at(s.st_name) == Some("_SEGGER_RTT"))
        .map(|s| s.st_value as u32)
}

impl Logs {
    pub fn new(elf: Option<&[u8]>) -> Self {
        let Some(elf) = elf else {
            return Logs {
                rtt: None,
                decoder: None,
            };
        };
        let table: Option<&'static Table> = match Table::parse(elf) {
            Ok(Some(t)) => Some(Box::leak(Box::new(t))),
            Ok(None) => {
                eprintln!("the ELF has no defmt table");
                None
            }
            Err(e) => {
                eprintln!("cannot read the defmt table: {e}");
                None
            }
        };
        Logs {
            rtt: rtt_address(elf),
            decoder: table.map(|t| t.new_stream_decoder()),
        }
    }

    /// Address of the RTT control block.
    pub fn rtt(&self) -> Option<u32> {
        self.rtt
    }

    /// Decodes the frames in `bytes`, returning the log lines.
    pub fn decode(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        let Some(dec) = self.decoder.as_mut() else {
            return lines;
        };
        dec.received(bytes);
        loop {
            match dec.decode() {
                Ok(frame) => {
                    let level = frame
                        .level()
                        .map(|l| l.as_str().to_uppercase())
                        .unwrap_or_default();
                    lines.push(format!("{level:<5} {}", frame.display_message()));
                }
                Err(DecodeError::UnexpectedEof) => break,
                Err(DecodeError::Malformed) => {
                    lines.push("(malformed defmt frame)".into());
                }
            }
        }
        lines
    }
}
