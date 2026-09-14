use std::cell::Cell;

pub trait Memory {
    fn read8(&self, addr: u32) -> u8;
    fn read16(&self, addr: u32) -> u16;
    fn read32(&self, addr: u32) -> u32;
    /// Instruction fetch (exactly like read16, except an unmapped fetch
    /// pends an execute-class bus fault instead of a data-class one, and
    /// no MPU data check runs — the loop-top XN check owns execute
    /// permission, which is also the more correct fault class there).
    fn fetch16(&self, addr: u32) -> u16;
    /// Mapped for any access (peripheral space, flash, RAM, extras).
    /// The run loop pre-checks fetches with this so a wild PC faults
    /// precisely instead of executing a dummy NOP (which would advance PC
    /// and let the re-fault clobber BFAR).
    fn is_mapped(&self, addr: u32) -> bool;
    fn write8(&mut self, addr: u32, v: u8);
    fn write16(&mut self, addr: u32, v: u16);
    fn write32(&mut self, addr: u32, v: u32);
    /// Drain staged DMA/DTC transfers. The run loop calls this every 16
    /// instructions while dma_active() is set; peripheral writes call it
    /// unconditionally after staging.
    fn service_sync_dma(&mut self) {}
}

pub struct MemRegion {
    pub base: u32,
    pub data: Vec<u8>,
}

fn is_periph(addr: u32) -> bool {
    (addr >= 0x40000000 && addr < 0x51000000)
        // Full FSMC window (banks 1-4 every 0x10000000 up to 0xA0000000):
        // untapped banks must reach the model (inert 0), not the bus-fault
        // arms — the fsmc_test BANK4 probe depends on it.
        || (addr >= 0x60000000 && addr < 0xA0000000)
        || (addr >= 0xA0000000 && addr < 0xA2000000)
        || (addr >= 0xE0000000 && addr < 0xE1000000)
}

/// Flat guest memory for the WASM-native CPU: flash + main SRAM + any number
/// of extra RAM/ROM regions (doom's EXTRAM at 0xC0000000, the WAD image at
/// 0xB8000000, ...). Peripheral addresses route into the Rust model via the
/// process-wide `SYS` instance (which `init`/`init_svd` installs before any
/// `WasmCpu` is created).
///
/// Flash is execute/read-only for the guest: normal `write8` stores to flash
/// are ignored (real flash needs an erase/program sequence, serviced by the
/// JS flash driver). `load()` bypasses the protection
/// and is the only way to get firmware into flash.
pub struct FlatMemory {
    pub flash: Vec<u8>,
    pub ram: Vec<u8>,
    pub extra: Vec<MemRegion>,
    pub flash_base: u32,
    pub ram_base: u32,
    /// Last unmapped access (read or write), for diagnostics. Reads of
    /// unmapped memory return 0; writes are dropped.
    pub bad: Cell<Option<u32>>,
}

impl FlatMemory {
    pub fn new(flash_size: usize, ram_size: usize) -> Self {
        Self {
            flash: vec![0; flash_size],
            ram: vec![0; ram_size],
            extra: Vec::new(),
            flash_base: 0x08000000,
            ram_base: 0x20000000,
            bad: Cell::new(None),
        }
    }

    #[inline(always)]
    fn in_flash(&self, addr: u32) -> bool {
        addr >= self.flash_base && (addr - self.flash_base) < self.flash.len() as u32
    }
    #[inline(always)]
    fn in_ram(&self, addr: u32) -> bool {
        addr >= self.ram_base && (addr - self.ram_base) < self.ram.len() as u32
    }
    fn extra_idx(&self, addr: u32) -> Option<usize> {
        self.extra
            .iter()
            .position(|r| addr >= r.base && (addr - r.base) < r.data.len() as u32)
    }

    /// Map a zeroed extra region in bulk. `load()` builds regions byte by
    /// byte (quadratic for megabyte images); pre-mapping makes WAD/EXTRAM
    /// setup O(size) with a fast fill. Matches the JS driver, which zeroes
    /// every extra_ram region at map time.
    pub fn map_extra(&mut self, base: u32, size: usize) {
        self.extra.push(MemRegion { base, data: vec![0; size] });
    }

    /// Load `data` at `base`, writing through flash protection. Bytes landing
    /// in flash or RAM go there; bytes landing anywhere else create (or
    /// extend) an extra region, so ELF segments / WAD images just work.
    pub fn load(&mut self, data: &[u8], base: u32) {
        for (i, &b) in data.iter().enumerate() {
            let a = base.wrapping_add(i as u32);
            if self.in_flash(a) {
                self.flash[(a - self.flash_base) as usize] = b;
            } else if self.in_ram(a) {
                self.ram[(a - self.ram_base) as usize] = b;
            } else if is_periph(a) {
                // never load firmware into MMIO
            } else if let Some(idx) = self.extra_idx(a) {
                let r = &mut self.extra[idx];
                r.data[(a - r.base) as usize] = b;
            } else {
                // extend backwards into a touching region, else create one
                let mut done = false;
                for r in self.extra.iter_mut() {
                    if a.wrapping_add(1) == r.base {
                        r.base = a;
                        r.data.insert(0, b);
                        done = true;
                        break;
                    }
                    if a == r.base.wrapping_add(r.data.len() as u32) {
                        r.data.push(b);
                        done = true;
                        break;
                    }
                }
                if !done {
                    self.extra.push(MemRegion { base: a, data: vec![b] });
                }
            }
        }
    }

    #[inline(always)]
    fn read_ram_byte(&self, addr: u32) -> u8 {
        if self.in_ram(addr) {
            self.ram[(addr - self.ram_base) as usize]
        } else if let Some(idx) = self.extra_idx(addr) {
            self.extra[idx].data[(addr - self.extra[idx].base) as usize]
        } else {
            0
        }
    }

    #[inline(always)]
    fn write_ram_byte(&mut self, addr: u32, v: u8) {
        if self.in_ram(addr) {
            self.ram[(addr - self.ram_base) as usize] = v;
        } else if let Some(idx) = self.extra_idx(addr) {
            let r = &mut self.extra[idx];
            r.data[(addr - r.base) as usize] = v;
        }
        // Flash/unmapped destinations are skipped (matches the JS driver,
        // whose mem_write there throws and is ignored before completion).
    }

    /// Synchronously complete staged memory-to-memory DMA transfers.
    /// Real mem-to-mem DMA runs at bus rate while the guest continues, but
    /// polling firmware (edge_test, periph_test) checks NDTR/dst/flags on
    /// the very next instructions — only an inline move satisfies that, the
    /// way the pre-latch model behaved. Peripheral transfers stay staged:
    /// their data path lives in the JS driver.

    /// DTC activations queued by event dispatch (IELSR.DTCE): resolve the
    /// SRAM vector table + transfer_info fresh on every fire (so
    /// R_DTC_Reconfigure is honored with no extra modeling), move one
    /// transfer unit, maintain repeat counting in system state, and
    /// write back advanced pointers like HW. Peripheral destinations
    /// (e.g. DAC DADR) go through the bus; RAM through the flat array.
    fn service_dtc(&mut self) {
        while let Some(irq) = crate::system::dtc_take_pending() {
            self.dtc_fire(irq);
        }
    }
    #[inline(always)]
    fn rd_ram_u32(&self, a: u32) -> u32 {
        (self.read_ram_byte(a) as u32)
            | ((self.read_ram_byte(a.wrapping_add(1)) as u32) << 8)
            | ((self.read_ram_byte(a.wrapping_add(2)) as u32) << 16)
            | ((self.read_ram_byte(a.wrapping_add(3)) as u32) << 24)
    }
    #[inline(always)]
    fn rd_ram_u16(&self, a: u32) -> u16 {
        (self.read_ram_byte(a) as u16) | ((self.read_ram_byte(a.wrapping_add(1)) as u16) << 8)
    }
    #[inline(always)]
    fn wr_ram_u32(&mut self, a: u32, v: u32) {
        self.write_ram_byte(a, (v & 0xFF) as u8);
        self.write_ram_byte(a.wrapping_add(1), ((v >> 8) & 0xFF) as u8);
        self.write_ram_byte(a.wrapping_add(2), ((v >> 16) & 0xFF) as u8);
        self.write_ram_byte(a.wrapping_add(3), ((v >> 24) & 0xFF) as u8);
    }
    fn dtc_fire(&mut self, irq: u32) {
        let sys = crate::sys();
        let vbr = sys.p.read(sys, 0x4000_5400 + 4, 4);
        let info = self.rd_ram_u32(vbr.wrapping_add(irq.wrapping_mul(4)));
        if !self.in_ram(info) || !self.in_ram(info.wrapping_add(15)) {
            return; // no descriptor (spurious activation)
        }
        let settings = self.rd_ram_u32(info);
        let src = self.rd_ram_u32(info.wrapping_add(4));
        let dest = self.rd_ram_u32(info.wrapping_add(8));
        // Real transfer_info_t layout (r_transfer_api.h): settings +0,
        // p_src +4, p_dest +8, num_blocks u16 +12, length u16 +14.
        // Repeat/block modes run on the CRA counter: R_DTC doubles the
        // programmed length into CRAL+CRAH (r_dtc.c: length =
        // (CRAL<<8)|CRAL), so the live low half reads e.g. 0x1818 for
        // length=24. CRAL (low byte) is the transfer count; normal
        // mode reads the full halfword (no doubling there).
        let mode = (settings >> 30) & 3;
        let raw = self.rd_ram_u16(info.wrapping_add(14));
        let length = if mode == 0 { raw } else { raw & 0xFF };
        if length == 0 {
            return;
        }
        if mode == 2 || mode == 3 {
            return; // block/chain modes unmodeled (no consumer)
        }
        let size = match (settings >> 28) & 3 {
            0 => 1u32,
            1 => 2,
            2 => 4,
            _ => return,
        };
        let src_inc = (settings >> 26) & 3 == 2;
        let dest_inc = (settings >> 18) & 3 == 2;
        let irq_each = settings & (1 << 21) != 0;
        // Resync repeat state when the driver rewrote the descriptor.
        let mut ch = crate::system::dtc_get(irq).unwrap_or_default();
        if ch.settings != settings || ch.dest != dest || ch.length != length {
            ch = crate::system::DtcCh {
                settings,
                dest,
                length,
                base_src: src,
                base_dest: dest,
                remaining: length,
                done: false,
            };
        }
        if mode == 0 && ch.done {
            crate::system::dtc_put(irq, ch);
            return;
        }
        if ch.remaining == 0 {
            ch.remaining = ch.length;
        }
        // One transfer unit src -> dest.
        let mut buf = [0u8; 4];
        for i in 0..size {
            buf[i as usize] = self.read_ram_byte(src.wrapping_add(i));
        }
        if is_periph(dest) {
            sys.p.write(sys, dest, size as u8, u32::from_le_bytes(buf));
        } else {
            for i in 0..size {
                self.write_ram_byte(dest.wrapping_add(i), buf[i as usize]);
            }
        }
        let nsrc = if src_inc { src.wrapping_add(size) } else { src };
        let ndest = if dest_inc { dest.wrapping_add(size) } else { dest };
        self.wr_ram_u32(info.wrapping_add(4), nsrc);
        self.wr_ram_u32(info.wrapping_add(8), ndest);
        ch.remaining -= 1;
        // Completion: NORMAL stops (done), REPEAT wraps the repeat area
        // and continues. IRQ on EACH transfer, or (END) at wrap/done.
        let repeat_src = settings & (1 << 20) != 0; // 1 = source repeats
        let wrapped = if ch.remaining == 0 {
            if mode == 0 {
                ch.done = true;
            } else {
                ch.remaining = ch.length;
                if repeat_src {
                    self.wr_ram_u32(info.wrapping_add(4), ch.base_src);
                } else {
                    self.wr_ram_u32(info.wrapping_add(8), ch.base_dest);
                }
            }
            true
        } else {
            false
        };
        crate::system::dtc_put(irq, ch);
        if irq_each || wrapped {
            sys.p.nvic.borrow_mut().set_intr_pending(irq as i32);
        }
    }

    fn service_sync_dma_inner(&mut self) {
        if !crate::system::dma_active() {
            return;
        }
        self.service_dtc();
        loop {
            let now = crate::system::instruction_count();
            let t = match crate::sys().take_due_memcopy_dma_transfer(now) {
                Some(t) => t,
                None => break,
            };
            if t.dma_name == "DMAC_EV" {
                if std::env::var("DMAEVLOG").is_ok() {
                    eprintln!("SVC ch={} due={} seq={} now={}", t.stream_idx, t.due, t.seq, crate::system::instruction_count());
                }
                // RA DMAC event unit: endpoints may be peripheral
                // registers (GPIO PCNTR3/PCNTR2 for SoftwareSerial).
                // Executed here in FIFO (due-time) order so TX writes
                // land before the paired RX reads.
                let sys = crate::sys();
                for i in 0..t.size {
                    let sa = t.src.wrapping_add(i as u32);
                    let v = if self.in_ram(sa) {
                        self.read_ram_byte(sa)
                    } else {
                        sys.p.read(sys, sa, 1) as u8
                    };
                    let da = t.dst.wrapping_add(i as u32);
                    if self.in_ram(da) {
                        self.write_ram_byte(da, v);
                    } else {
                        sys.p.write(sys, da, 1, v as u32);
                    }
                }
                crate::system::dmac_unit_done(t.stream_idx);
                crate::sys().mark_dma_completed(t.stream_idx, true);
                if std::env::var("DMAEVLOG").is_ok() && t.stream_idx == 0 {
                    eprintln!("RXVAL due={} dst={:#x}", t.due, t.dst);
                }
                continue;
            }
            // memmove semantics via a temp buffer (src/dst may overlap).
            let mut buf = Vec::with_capacity(t.size);
            for i in 0..t.size {
                buf.push(self.read_ram_byte(t.src.wrapping_add(i as u32)));
            }
            for (i, b) in buf.iter().enumerate() {
                self.write_ram_byte(t.dst.wrapping_add(i as u32), *b);
            }
            crate::sys().mark_dma_completed(t.stream_idx, true);
        }
        // Stay active while future-dated units remain (they become due
        // as the run loop advances); idle only on a drained queue.
        if !crate::sys().has_pending_memcopy() {
            crate::system::dma_idle();
        }
    }
    /// Fetch one byte without any MPU check (execute permission belongs to
    /// the loop-top XN check). Unmapped bytes pend an execute-class bus
    /// fault (first-wins keeps the lowest faulting address).
    fn fetch_byte(&self, addr: u32) -> Option<u8> {        if self.in_flash(addr) {
            Some(self.flash[(addr - self.flash_base) as usize])
        } else if self.in_ram(addr) {
            Some(self.ram[(addr - self.ram_base) as usize])
        } else if let Some(idx) = self.extra_idx(addr) {
            let r = &self.extra[idx];
            Some(r.data[(addr - r.base) as usize])
        } else {
            self.bad.set(Some(addr));
            crate::system::pend_bus_fault(addr, true);
            None
        }
    }

    /// True for mapped normal memory (flash/RAM/extra). Periph accesses
    /// return earlier; anything else falls to the bus-fault bad-arms.
    #[inline(always)]
    fn mapped(&self, addr: u32) -> bool {
        is_periph(addr) || self.in_flash(addr) || self.in_ram(addr) || self.extra_idx(addr).is_some()
    }

    /// CCR.UNALIGN_TRP gate for multi-byte normal-memory accesses. Skipped
    /// when unmapped (the bus-fault arms own those) and on the periph path
    /// (Device-memory unaligned stays lenient). Unaligned *Device* access
    /// faults regardless of the trap (the one observable MPU type rule).
    /// Like the MPU data path the faulting access completes dropped and
    /// raises before the next fetch (flags exact, PC deferred by one).
    #[inline(always)]
    fn unaligned_deny(&self, addr: u32, size: u32) -> bool {
        if !self.mapped(addr) {
            return false;
        }
        if size <= 1 || (addr & (size - 1)) == 0 {
            return false;
        }
        if crate::system::unalign_trp() || crate::sys().p.mpu_is_device(addr) {
            crate::system::pend_align_fault(addr);
            true
        } else {
            false
        }
    }

    /// MPU gate for one CPU access (size bytes at addr). Returns true when
    /// denied (violation latched for the run loop; caller returns dummy /
    /// drops the access). Fast path is a single predictable-false branch
    /// when the MPU is off; unmapped addresses never reach here (callers
    /// check mapped-ness first and keep the legacy bad-address behavior).
    #[inline(always)]
    fn mpu_deny(&self, addr: u32, size: u32, write: bool) -> bool {
        if !crate::system::is_mpu_enabled() {
            return false;
        }
        match crate::sys().p.mpu_check(addr, size, write, false) {
            Some(_) => {
                crate::system::pend_mpu_fault(addr, false);
                true
            }
            None => false,
        }
    }
}

impl Memory for FlatMemory {
    fn service_sync_dma(&mut self) {
        self.service_sync_dma_inner();
    }
    fn read8(&self, addr: u32) -> u8 {
        if is_periph(addr) {
            // MPU first: a faulting access must not reach model side
            // effects (UART TX, RXNE clears, ...).
            if self.mpu_deny(addr, 1, false) {
                return 0;
            }
            // Single width-1 model read (mirrors the JS memReadHook, which
            // takes the low byte). The model aligns internally.
            return crate::sys().p.read(crate::sys(), addr, 1) as u8;
        }
        // Unmapped addresses keep the legacy behavior (bad-address latch
        // + 0) and never raise MPU faults; only mapped memory is checked.
        if self.in_flash(addr) {
            if self.mpu_deny(addr, 1, false) {
                return 0;
            }
            self.flash[(addr - self.flash_base) as usize]
        } else if self.in_ram(addr) {
            if self.mpu_deny(addr, 1, false) {
                return 0;
            }
            self.ram[(addr - self.ram_base) as usize]
        } else if let Some(idx) = self.extra_idx(addr) {
            if self.mpu_deny(addr, 1, false) {
                return 0;
            }
            let r = &self.extra[idx];
            r.data[(addr - r.base) as usize]
        } else {
            // Unmapped memory: legacy bad-address latch plus a precise
            // BusFault (silicon faults wild accesses; the 0-return alone
            // used to hide null derefs and overruns). Peripheral-space
            // holes never reach here (the model returns 0 for those).
            self.bad.set(Some(addr));
            crate::system::pend_bus_fault(addr, false);
            0
        }
    }
    fn fetch16(&self, addr: u32) -> u16 {
        if is_periph(addr) {
            // No MPU data check: the loop-top XN check owns execute
            // permission (and reports the correct IACCVIOL-class fault).
            return crate::sys().p.read(crate::sys(), addr, 2) as u16;
        }
        // Fast path: both bytes inside one region (the common case).
        // Falls back to the byte-wise faulting path at region edges.
        let lo = addr;
        let hi = addr.wrapping_add(1);
        if self.in_flash(lo) {
            if self.in_flash(hi) {
                let b = (addr - self.flash_base) as usize;
                return (self.flash[b] as u16) | ((self.flash[b + 1] as u16) << 8);
            }
        } else if self.in_ram(lo) {
            if self.in_ram(hi) {
                let b = (addr - self.ram_base) as usize;
                return (self.ram[b] as u16) | ((self.ram[b + 1] as u16) << 8);
            }
        } else if let Some(idx) = self.extra_idx(lo) {
            let r = &self.extra[idx];
            if hi >= r.base && (hi - r.base) < r.data.len() as u32 {
                let b = (addr - r.base) as usize;
                return (r.data[b] as u16) | ((r.data[b + 1] as u16) << 8);
            }
        }
        // Bounds-checked per byte (a fetch straddling a region end faults
        // on the first unmapped byte instead of panicking the host).
        match (self.fetch_byte(addr), self.fetch_byte(addr.wrapping_add(1))) {
            (Some(lo), Some(hi)) => (lo as u16) | ((hi as u16) << 8),
            _ => 0,
        }
    }
    fn is_mapped(&self, addr: u32) -> bool {
        // Fast path first (flash/RAM cover all fetches); the full
        // mapped() adds the periph windows + extra-region linear scan.
        if self.in_flash(addr) || self.in_ram(addr) {
            return true;
        }
        self.mapped(addr)
    }
    fn read16(&self, addr: u32) -> u16 {
        if is_periph(addr) {
            if self.mpu_deny(addr, 2, false) {
                return 0;
            }
            return crate::sys().p.read(crate::sys(), addr, 2) as u16;
        }
        if self.unaligned_deny(addr, 2) {
            return 0;
        }
        let lo = self.read8(addr) as u16;
        let hi = self.read8(addr + 1) as u16;
        lo | (hi << 8)
    }
    fn read32(&self, addr: u32) -> u32 {
        if is_periph(addr) {
            if self.mpu_deny(addr, 4, false) {
                return 0;
            }
            return crate::sys().p.read(crate::sys(), addr, 4);
        }
        if self.unaligned_deny(addr, 4) {
            return 0;
        }
        let b0 = self.read8(addr) as u32;
        let b1 = self.read8(addr + 1) as u32;
        let b2 = self.read8(addr + 2) as u32;
        let b3 = self.read8(addr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }
    fn write8(&mut self, addr: u32, v: u8) {
        if is_periph(addr) {
            if self.mpu_deny(addr, 1, true) {
                return;
            }
            // Single width-1 model write. Never split a wider guest store
            // into byte RMWs here: each model write can have side effects
            // (a USART DR write emits a UART char), so one guest store must
            // equal exactly one model call, like the JS memWriteHook.
            crate::sys().p.write(crate::sys(), addr, 1, v as u32);
            self.service_sync_dma();
            return;
        }
        if self.in_flash(addr) {
            // MPU first (an RX-mapped flash write faults on silicon),
            // then flash protection: guest stores are ignored (see docs).
            if self.mpu_deny(addr, 1, true) {
                return;
            }
            // flash protection: guest stores are ignored (see struct docs)
        } else if self.in_ram(addr) {
            if self.mpu_deny(addr, 1, true) {
                return;
            }
            self.ram[(addr - self.ram_base) as usize] = v;
        } else if let Some(idx) = self.extra_idx(addr) {
            if self.mpu_deny(addr, 1, true) {
                return;
            }
            let r = &mut self.extra[idx];
            r.data[(addr - r.base) as usize] = v;
        } else {
            // Unmapped store: bad-address latch plus a precise BusFault
            // (read arm documents the split with peripheral holes).
            self.bad.set(Some(addr));
            crate::system::pend_bus_fault(addr, false);
        }
    }
    fn write16(&mut self, addr: u32, v: u16) {
        if is_periph(addr) {
            if self.mpu_deny(addr, 2, true) {
                return;
            }
            crate::sys().p.write(crate::sys(), addr, 2, v as u32);
            self.service_sync_dma();
            return;
        }
        if self.unaligned_deny(addr, 2) {
            return;
        }
        self.write8(addr, (v & 0xFF) as u8);
        self.write8(addr + 1, (v >> 8) as u8);
    }
    fn write32(&mut self, addr: u32, v: u32) {
        if is_periph(addr) {
            if self.mpu_deny(addr, 4, true) {
                return;
            }
            crate::sys().p.write(crate::sys(), addr, 4, v);
            self.service_sync_dma();
            return;
        }
        if self.unaligned_deny(addr, 4) {
            return;
        }
        self.write8(addr, (v & 0xFF) as u8);
        self.write8(addr + 1, ((v >> 8) & 0xFF) as u8);
        self.write8(addr + 2, ((v >> 16) & 0xFF) as u8);
        self.write8(addr + 3, ((v >> 24) & 0xFF) as u8);
    }
}
