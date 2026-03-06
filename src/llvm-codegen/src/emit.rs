//! Object-file emission.
//!
//! Produces a minimal ELF-64 (Linux/x86-64) or Mach-O 64-bit (macOS/x86-64)
//! relocatable object file containing a single `.text` section.
//! The actual byte encoding is supplied by the target via the [`Emitter`] trait.

// ── object-file model ──────────────────────────────────────────────────────

/// Supported object-file formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFormat {
    Elf,
    MachO,
}

/// Kind of relocation record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocKind {
    /// 32-bit PC-relative addend (e.g. near call / branch).
    Pc32,
    /// 64-bit absolute address.
    Abs64,
}

/// A single relocation record.
#[derive(Clone, Debug)]
pub struct Reloc {
    /// Byte offset within the section data.
    pub offset: u64,
    /// Index into `ObjectFile::symbols` for the referenced symbol.
    pub symbol: usize,
    pub kind: RelocKind,
    /// Addend (ELF RELA / Mach-O addend).
    pub addend: i64,
}

/// A named output section (`.text`, `__TEXT,__text`, etc.).
#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub data: Vec<u8>,
    pub relocs: Vec<Reloc>,
}

/// A symbol definition.
#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    /// Index of the section this symbol lives in.
    pub section: usize,
    /// Byte offset within that section.
    pub offset: u64,
    pub size: u64,
    pub global: bool,
}

/// Assembled object file ready to be written to disk or passed to a linker.
#[derive(Clone, Debug)]
pub struct ObjectFile {
    pub format: ObjectFormat,
    /// ELF e_machine value when `format == ObjectFormat::Elf`.
    /// Ignored for Mach-O.
    pub elf_machine: u16,
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
}

impl ObjectFile {
    /// Serialize the object file to raw bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self.format {
            ObjectFormat::Elf => serialize_elf(self),
            ObjectFormat::MachO => serialize_macho(self),
        }
    }
}

// ── Emitter trait ──────────────────────────────────────────────────────────

use crate::isel::MachineFunction;

/// Implemented by each target to encode machine instructions into bytes.
pub trait Emitter {
    /// Encode `mf` and return a [`Section`] containing the machine code.
    fn emit_function(&mut self, mf: &MachineFunction) -> Section;

    /// The object format this emitter targets.
    fn object_format(&self) -> ObjectFormat;

    /// ELF `e_machine` field for this target.
    fn elf_machine(&self) -> u16 {
        62 // EM_X86_64
    }
}

/// Build a complete [`ObjectFile`] from a [`MachineFunction`] using `emitter`.
pub fn emit_object(mf: &MachineFunction, emitter: &mut dyn Emitter) -> ObjectFile {
    let section = emitter.emit_function(mf);
    let size = section.data.len() as u64;
    let sym = Symbol {
        name: mf.name.clone(),
        section: 0,
        offset: 0,
        size,
        global: true,
    };
    ObjectFile {
        format: emitter.object_format(),
        elf_machine: emitter.elf_machine(),
        sections: vec![section],
        symbols: vec![sym],
    }
}

// ── ELF-64 serialization ───────────────────────────────────────────────────
//
// Minimal ELF-64 relocatable (.o) layout:
//   ELF header (64 B)
//   Section header table
//     [0] null
//     [1] .text       SHT_PROGBITS
//     [2] .symtab     SHT_SYMTAB
//     [3] .strtab     SHT_STRTAB   (symbol names)
//     [4] .shstrtab   SHT_STRTAB   (section names)
//     [5] .rela.text  SHT_RELA     (if relocs present)
//   Section data: .text, .symtab, .strtab, .shstrtab, .rela.text

fn serialize_elf(obj: &ObjectFile) -> Vec<u8> {
    // ── string tables ───────────────────────────────────────────────────────
    let mut shstrtab: Vec<u8> = vec![0u8]; // index 0 = empty string
    let text_name_off = push_str(&mut shstrtab, b".text");
    let symtab_name_off = push_str(&mut shstrtab, b".symtab");
    let strtab_name_off = push_str(&mut shstrtab, b".strtab");
    let shstrtab_name_off = push_str(&mut shstrtab, b".shstrtab");

    let text_data = obj.sections.first().map_or(&[][..], |s| s.data.as_slice());
    let text_relocs = obj
        .sections
        .first()
        .map_or(&[][..], |s| s.relocs.as_slice());
    let has_relocs = !text_relocs.is_empty();

    let relatext_name_off = if has_relocs {
        push_str(&mut shstrtab, b".rela.text")
    } else {
        0
    };

    let mut strtab: Vec<u8> = vec![0u8];
    let sym_name_offs: Vec<u32> = obj
        .symbols
        .iter()
        .map(|s| push_str(&mut strtab, s.name.as_bytes()))
        .collect();

    // ── layout ─────────────────────────────────────────────────────────────
    const ELF_HDR: u64 = 64;
    const SH_ENT: u64 = 64;
    const SYM_ENT: u64 = 24;
    const RELA_ENT: u64 = 24;

    let num_sections: u16 = if has_relocs { 6 } else { 5 };
    let sh_table_size = num_sections as u64 * SH_ENT;

    let text_off = ELF_HDR + sh_table_size;
    let text_size = text_data.len() as u64;
    let sym_count = 1 + obj.symbols.len() as u64; // null + symbols
    let symtab_off = text_off + text_size;
    let symtab_size = sym_count * SYM_ENT;
    let strtab_off = symtab_off + symtab_size;
    let shstrtab_off = strtab_off + strtab.len() as u64;
    let relatext_off = shstrtab_off + shstrtab.len() as u64;
    let relatext_size = text_relocs.len() as u64 * RELA_ENT;

    // ── buffer ─────────────────────────────────────────────────────────────
    let mut buf = Vec::<u8>::new();

    // ELF header
    buf.extend_from_slice(b"\x7fELF"); // magic
    buf.push(2); // EI_CLASS: 64-bit
    buf.push(1); // EI_DATA: little-endian
    buf.push(1); // EI_VERSION
    buf.push(0); // EI_OSABI: System V
    buf.extend_from_slice(&[0u8; 8]); // padding
    w16(&mut buf, 1); // e_type: ET_REL
    w16(&mut buf, obj.elf_machine); // e_machine: target-specific
    w32(&mut buf, 1); // e_version
    w64(&mut buf, 0); // e_entry
    w64(&mut buf, 0); // e_phoff
    w64(&mut buf, ELF_HDR); // e_shoff
    w32(&mut buf, 0); // e_flags
    w16(&mut buf, ELF_HDR as u16); // e_ehsize
    w16(&mut buf, 0); // e_phentsize
    w16(&mut buf, 0); // e_phnum
    w16(&mut buf, SH_ENT as u16); // e_shentsize
    w16(&mut buf, num_sections); // e_shnum
    w16(&mut buf, 4); // e_shstrndx (.shstrtab at index 4)

    // Helper: write a 64-byte section header entry
    let write_shdr = |buf: &mut Vec<u8>,
                      name: u32,
                      sh_type: u32,
                      flags: u64,
                      addr: u64,
                      off: u64,
                      size: u64,
                      link: u32,
                      info: u32,
                      align: u64,
                      entsize: u64| {
        w32(buf, name);
        w32(buf, sh_type);
        w64(buf, flags);
        w64(buf, addr);
        w64(buf, off);
        w64(buf, size);
        w32(buf, link);
        w32(buf, info);
        w64(buf, align);
        w64(buf, entsize);
    };

    // Section headers
    write_shdr(&mut buf, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0); // [0] null
    write_shdr(
        &mut buf,
        text_name_off,
        1,
        6, // [1] .text  SHF_ALLOC|SHF_EXECINSTR
        0,
        text_off,
        text_size,
        0,
        0,
        16,
        0,
    );
    write_shdr(
        &mut buf,
        symtab_name_off,
        2,
        0, // [2] .symtab link=3 info=first_global(1)
        0,
        symtab_off,
        symtab_size,
        3,
        1,
        8,
        SYM_ENT,
    );
    write_shdr(
        &mut buf,
        strtab_name_off,
        3,
        0, // [3] .strtab
        0,
        strtab_off,
        strtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    write_shdr(
        &mut buf,
        shstrtab_name_off,
        3,
        0, // [4] .shstrtab
        0,
        shstrtab_off,
        shstrtab.len() as u64,
        0,
        0,
        1,
        0,
    );
    if has_relocs {
        write_shdr(
            &mut buf,
            relatext_name_off,
            4,
            0, // [5] .rela.text link=2 info=1
            0,
            relatext_off,
            relatext_size,
            2,
            1,
            8,
            RELA_ENT,
        );
    }

    // Section data: .text
    buf.extend_from_slice(text_data);

    // .symtab: null entry then symbols
    buf.extend_from_slice(&[0u8; 24]);
    for (i, sym) in obj.symbols.iter().enumerate() {
        let st_info: u8 = (1u8 << 4) | 2u8; // STB_GLOBAL | STT_FUNC
        let st_shndx: u16 = (sym.section + 1) as u16; // +1 for null section header
        w32(&mut buf, sym_name_offs[i]);
        buf.push(st_info);
        buf.push(0); // st_other
        w16(&mut buf, st_shndx);
        w64(&mut buf, sym.offset);
        w64(&mut buf, sym.size);
    }

    // .strtab
    buf.extend_from_slice(&strtab);

    // .shstrtab
    buf.extend_from_slice(&shstrtab);

    // .rela.text
    if has_relocs {
        for reloc in text_relocs {
            let sym_idx = (reloc.symbol + 1) as u64;
            let r_type: u64 = match reloc.kind {
                RelocKind::Pc32 => 2,  // R_X86_64_PC32
                RelocKind::Abs64 => 1, // R_X86_64_64
            };
            let r_info = (sym_idx << 32) | r_type;
            w64(&mut buf, reloc.offset);
            w64(&mut buf, r_info);
            buf.extend_from_slice(&reloc.addend.to_le_bytes());
        }
    }

    let _ = relatext_name_off;
    buf
}

// ── Mach-O 64-bit serialization ────────────────────────────────────────────
//
// Minimal Mach-O 64-bit MH_OBJECT layout:
//   mach_header_64     (32 B)
//   LC_SEGMENT_64      (72 B)
//     section_64 __TEXT,__text  (80 B)
//   LC_SYMTAB          (24 B)
//   LC_DYSYMTAB        (80 B)
//   [padding to 16-byte boundary]
//   section data: __text
//   relocation entries
//   symbol table (nlist_64, 16 B each)
//   string table

fn serialize_macho(obj: &ObjectFile) -> Vec<u8> {
    let text_data = obj.sections.first().map_or(&[][..], |s| s.data.as_slice());
    let text_size = text_data.len() as u32;
    let text_relocs = obj
        .sections
        .first()
        .map_or(&[][..], |s| s.relocs.as_slice());

    const MH_HDR: u32 = 32;
    const SEG_CMD: u32 = 72;
    const SECT_HDR: u32 = 80;
    const SYMTAB_CMD: u32 = 24;
    const DYSYMTAB_CMD: u32 = 80;
    const SYM_ENT: u32 = 16; // nlist_64

    let header_size = MH_HDR + SEG_CMD + SECT_HDR + SYMTAB_CMD + DYSYMTAB_CMD;
    let cmds_size = SEG_CMD + SECT_HDR + SYMTAB_CMD + DYSYMTAB_CMD;

    // Align __text to 16-byte boundary after headers.
    let text_align = 16u32;
    let text_pad = (text_align - (header_size % text_align)) % text_align;
    let text_off = header_size + text_pad;
    let reloc_off = text_off + text_size;
    let reloc_size = text_relocs.len() as u32 * 8;

    // String table: index 0 = \0 (null, required by Mach-O spec — empty string).
    let mut strtab: Vec<u8> = vec![0u8];
    let sym_name_offs: Vec<u32> = obj
        .symbols
        .iter()
        .map(|s| {
            let off = strtab.len() as u32;
            strtab.push(b'_'); // C symbol underscore prefix
            strtab.extend_from_slice(s.name.as_bytes());
            strtab.push(0);
            off
        })
        .collect();
    while strtab.len() % 4 != 0 {
        strtab.push(0);
    } // align to 4 bytes

    let symtab_off = reloc_off + reloc_size;
    let symtab_size = obj.symbols.len() as u32 * SYM_ENT;
    let strtab_off = symtab_off + symtab_size;

    let mut buf = Vec::<u8>::new();

    // mach_header_64
    w32(&mut buf, 0xfeedfacf); // MH_MAGIC_64
    w32(&mut buf, 0x01000007); // CPU_TYPE_X86_64
    w32(&mut buf, 0x00000003); // CPU_SUBTYPE_X86_64_ALL
    w32(&mut buf, 1); // MH_OBJECT
    w32(&mut buf, 3); // ncmds
    w32(&mut buf, cmds_size); // sizeofcmds
    w32(&mut buf, 0); // flags

    // LC_SEGMENT_64
    w32(&mut buf, 0x19); // LC_SEGMENT_64
    w32(&mut buf, SEG_CMD + SECT_HDR); // cmdsize
    buf.extend_from_slice(b"__TEXT\0\0\0\0\0\0\0\0\0\0"); // segname[16]
    w64(&mut buf, 0); // vmaddr
    w64(&mut buf, text_size as u64); // vmsize
    w64(&mut buf, text_off as u64); // fileoff
    w64(&mut buf, text_size as u64); // filesize
    w32(&mut buf, 7); // maxprot
    w32(&mut buf, 5); // initprot (R|X)
    w32(&mut buf, 1); // nsects
    w32(&mut buf, 0); // flags

    // section_64 __TEXT,__text
    buf.extend_from_slice(b"__text\0\0\0\0\0\0\0\0\0\0"); // sectname[16]
    buf.extend_from_slice(b"__TEXT\0\0\0\0\0\0\0\0\0\0"); // segname[16]
    w64(&mut buf, 0); // addr
    w64(&mut buf, text_size as u64); // size
    w32(&mut buf, text_off); // offset
    w32(&mut buf, 4); // align (2^4 = 16)
    w32(&mut buf, reloc_off); // reloff
    w32(&mut buf, text_relocs.len() as u32); // nreloc
    w32(&mut buf, 0x80000400); // S_ATTR_PURE_INSTRUCTIONS|S_ATTR_SOME_INSTRUCTIONS
    w32(&mut buf, 0);
    w32(&mut buf, 0);
    w32(&mut buf, 0); // reserved1-3

    // LC_SYMTAB
    w32(&mut buf, 2); // LC_SYMTAB
    w32(&mut buf, SYMTAB_CMD);
    w32(&mut buf, symtab_off);
    w32(&mut buf, obj.symbols.len() as u32);
    w32(&mut buf, strtab_off);
    w32(&mut buf, strtab.len() as u32);

    // LC_DYSYMTAB
    w32(&mut buf, 0xB); // LC_DYSYMTAB
    w32(&mut buf, DYSYMTAB_CMD);
    let n_globals = obj.symbols.iter().filter(|s| s.global).count() as u32;
    w32(&mut buf, 0);
    w32(&mut buf, 0); // ilocalsym, nlocalsym
    w32(&mut buf, 0);
    w32(&mut buf, n_globals); // iextdefsym, nextdefsym
    w32(&mut buf, n_globals);
    w32(&mut buf, 0); // iundefsym, nundefsym
    buf.extend_from_slice(&[0u8; 48]); // remaining fields

    // padding
    buf.resize(buf.len() + text_pad as usize, 0);

    // __text section data
    buf.extend_from_slice(text_data);

    // relocation entries (relocation_info, 8 bytes each)
    for reloc in text_relocs {
        let sym_idx = reloc.symbol as u32;
        let (r_type, r_length, r_pcrel): (u32, u32, u32) = match reloc.kind {
            RelocKind::Pc32 => (2, 2, 1),  // X86_64_RELOC_BRANCH, 4 bytes, PC-rel
            RelocKind::Abs64 => (0, 3, 0), // X86_64_RELOC_UNSIGNED, 8 bytes, abs
        };
        let r_extern: u32 = 1;
        let r_info =
            sym_idx | (r_pcrel << 24) | (r_length << 25) | (r_extern << 27) | (r_type << 28);
        w32(&mut buf, reloc.offset as u32); // r_address
        w32(&mut buf, r_info);
    }

    // symbol table (nlist_64)
    for (i, sym) in obj.symbols.iter().enumerate() {
        let n_type: u8 = if sym.global { 0x0F } else { 0x0E }; // N_EXT|N_SECT
        w32(&mut buf, sym_name_offs[i]); // n_strx
        buf.push(n_type); // n_type
        buf.push(1); // n_sect (1-based, __text = 1)
        w16(&mut buf, 0); // n_desc
        w64(&mut buf, sym.offset); // n_value
    }

    // string table
    buf.extend_from_slice(&strtab);

    buf
}

// ── byte-writing helpers ───────────────────────────────────────────────────

#[inline]
fn w16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn w32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn w64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}

/// Append a null-terminated string to `table` and return its start offset.
fn push_str(table: &mut Vec<u8>, s: &[u8]) -> u32 {
    let off = table.len() as u32;
    table.extend_from_slice(s);
    table.push(0);
    off
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_obj(fmt: ObjectFormat, code: Vec<u8>) -> ObjectFile {
        let section_name = match fmt {
            ObjectFormat::Elf => ".text",
            ObjectFormat::MachO => "__text",
        };
        ObjectFile {
            format: fmt,
            elf_machine: 62,
            sections: vec![Section {
                name: section_name.into(),
                data: code,
                relocs: vec![],
            }],
            symbols: vec![Symbol {
                name: "f".into(),
                section: 0,
                offset: 0,
                size: 1,
                global: true,
            }],
        }
    }

    #[test]
    fn elf_magic_and_class() {
        let bytes = make_obj(ObjectFormat::Elf, vec![0x90]).to_bytes();
        assert_eq!(&bytes[0..4], b"\x7fELF", "ELF magic");
        assert_eq!(bytes[4], 2, "64-bit");
        assert_eq!(bytes[5], 1, "little-endian");
    }

    #[test]
    fn elf_machine_x86_64() {
        let bytes = make_obj(ObjectFormat::Elf, vec![0x90]).to_bytes();
        let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        assert_eq!(e_machine, 62, "EM_X86_64 = 62");
    }

    #[test]
    fn elf_relocatable_type() {
        let bytes = make_obj(ObjectFormat::Elf, vec![0x90]).to_bytes();
        let e_type = u16::from_le_bytes([bytes[16], bytes[17]]);
        assert_eq!(e_type, 1, "ET_REL = 1");
    }

    #[test]
    fn macho_magic() {
        let bytes = make_obj(ObjectFormat::MachO, vec![0xc3]).to_bytes();
        assert_eq!(&bytes[0..4], &[0xcf, 0xfa, 0xed, 0xfe], "MH_MAGIC_64");
    }

    #[test]
    fn macho_filetype_object() {
        let bytes = make_obj(ObjectFormat::MachO, vec![0xc3]).to_bytes();
        let filetype = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        assert_eq!(filetype, 1, "MH_OBJECT = 1");
    }

    #[test]
    fn macho_strtab_first_byte_is_null() {
        // Issue #37: Mach-O string table byte 0 must be \0 (null), not ' ' (space).
        // The string table is the last thing written in the object file.
        // For symbol "f", strtab = [\0, '_', 'f', \0] (4 bytes, aligned).
        // We verify the first byte of strtab (= last 4 bytes of the file) is \0.
        let bytes = make_obj(ObjectFormat::MachO, vec![0xc3]).to_bytes();
        // strtab is padded to 4 bytes: [\0, '_', 'f', \0] = 4 bytes.
        let strtab_first = bytes[bytes.len() - 4];
        assert_eq!(
            strtab_first, 0x00,
            "Mach-O strtab[0] must be null (\\0), was 0x{:02x}",
            strtab_first
        );
    }

    #[test]
    fn emit_object_roundtrip() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, mf: &MachineFunction) -> Section {
                let _ = mf;
                Section {
                    name: ".text".into(),
                    data: vec![0x90],
                    relocs: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Elf
            }
        }

        let mut mf = MachineFunction::new("test".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });
        let obj = emit_object(&mf, &mut NopEmitter);
        assert_eq!(obj.symbols[0].name, "test");
        assert_eq!(obj.sections[0].data, vec![0x90]);
        assert_eq!(obj.elf_machine, 62);
    }
}
