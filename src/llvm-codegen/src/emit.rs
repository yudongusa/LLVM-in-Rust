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

/// A single source mapping row for debug line table emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugLineRow {
    pub address: u64,
    pub line: u32,
    pub column: u32,
}

/// A named output section (`.text`, `__TEXT,__text`, etc.).
#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub data: Vec<u8>,
    pub relocs: Vec<Reloc>,
    /// Address->source rows collected while encoding this section.
    pub debug_rows: Vec<DebugLineRow>,
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
    let mut sections = vec![section];
    if emitter.object_format() == ObjectFormat::Elf {
        if !sections[0].debug_rows.is_empty() {
            let source = mf.debug_source.as_deref().unwrap_or("unknown");
            sections.push(Section {
                name: ".debug_line".into(),
                data: build_debug_line(source, &sections[0].debug_rows),
                relocs: Vec::new(),
                debug_rows: Vec::new(),
            });
        } else if let Some(line) = mf.debug_line_start {
            let source = mf.debug_source.as_deref().unwrap_or("unknown");
            sections.push(Section {
                name: ".debug_line".into(),
                data: build_debug_line(
                    source,
                    &[DebugLineRow {
                        address: 0,
                        line,
                        column: 0,
                    }],
                ),
                relocs: Vec::new(),
                debug_rows: Vec::new(),
            });
        }
    }
    ObjectFile {
        format: emitter.object_format(),
        elf_machine: emitter.elf_machine(),
        sections,
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
    let text_sec = obj.sections.first();
    let text_data = text_sec.map_or(&[][..], |s| s.data.as_slice());
    let text_relocs = text_sec.map_or(&[][..], |s| s.relocs.as_slice());
    let extra_secs = if obj.sections.len() > 1 {
        &obj.sections[1..]
    } else {
        &[][..]
    };
    let has_relocs = !text_relocs.is_empty();

    let mut shstrtab: Vec<u8> = vec![0u8];
    let text_name_off = push_str(&mut shstrtab, b".text");
    let extra_name_offs: Vec<u32> = extra_secs
        .iter()
        .map(|s| push_str(&mut shstrtab, s.name.as_bytes()))
        .collect();
    let symtab_name_off = push_str(&mut shstrtab, b".symtab");
    let strtab_name_off = push_str(&mut shstrtab, b".strtab");
    let shstrtab_name_off = push_str(&mut shstrtab, b".shstrtab");
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

    const ELF_HDR: u64 = 64;
    const SH_ENT: u64 = 64;
    const SYM_ENT: u64 = 24;
    const RELA_ENT: u64 = 24;
    const SHT_PROGBITS: u32 = 1;
    const SHT_SYMTAB: u32 = 2;
    const SHT_STRTAB: u32 = 3;
    const SHT_RELA: u32 = 4;

    let idx_text = 1u16;
    let idx_extra_start = idx_text + 1;
    let idx_symtab = idx_extra_start + extra_secs.len() as u16;
    let idx_strtab = idx_symtab + 1;
    let idx_shstrtab = idx_strtab + 1;
    let idx_rela = idx_shstrtab + 1;

    let num_sections: u16 = if has_relocs { idx_rela + 1 } else { idx_shstrtab + 1 };
    let sh_table_size = num_sections as u64 * SH_ENT;

    let mut cursor = ELF_HDR + sh_table_size;
    let text_off = cursor;
    let text_size = text_data.len() as u64;
    cursor += text_size;

    let mut extra_offs = Vec::with_capacity(extra_secs.len());
    for sec in extra_secs {
        extra_offs.push(cursor);
        cursor += sec.data.len() as u64;
    }

    let sym_count = 1 + obj.symbols.len() as u64;
    let symtab_off = cursor;
    let symtab_size = sym_count * SYM_ENT;
    cursor += symtab_size;

    let strtab_off = cursor;
    cursor += strtab.len() as u64;
    let shstrtab_off = cursor;
    cursor += shstrtab.len() as u64;

    let relatext_off = cursor;
    let relatext_size = text_relocs.len() as u64 * RELA_ENT;

    let mut buf = Vec::<u8>::new();
    buf.extend_from_slice(b"\x7fELF");
    buf.push(2);
    buf.push(1);
    buf.push(1);
    buf.push(0);
    buf.extend_from_slice(&[0u8; 8]);
    w16(&mut buf, 1);
    w16(&mut buf, obj.elf_machine);
    w32(&mut buf, 1);
    w64(&mut buf, 0);
    w64(&mut buf, 0);
    w64(&mut buf, ELF_HDR);
    w32(&mut buf, 0);
    w16(&mut buf, ELF_HDR as u16);
    w16(&mut buf, 0);
    w16(&mut buf, 0);
    w16(&mut buf, SH_ENT as u16);
    w16(&mut buf, num_sections);
    w16(&mut buf, idx_shstrtab);

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

    write_shdr(&mut buf, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    write_shdr(
        &mut buf,
        text_name_off,
        SHT_PROGBITS,
        6,
        0,
        text_off,
        text_size,
        0,
        0,
        16,
        0,
    );

    for (i, sec) in extra_secs.iter().enumerate() {
        write_shdr(
            &mut buf,
            extra_name_offs[i],
            SHT_PROGBITS,
            0,
            0,
            extra_offs[i],
            sec.data.len() as u64,
            0,
            0,
            1,
            0,
        );
    }

    write_shdr(
        &mut buf,
        symtab_name_off,
        SHT_SYMTAB,
        0,
        0,
        symtab_off,
        symtab_size,
        idx_strtab as u32,
        1,
        8,
        SYM_ENT,
    );
    write_shdr(
        &mut buf,
        strtab_name_off,
        SHT_STRTAB,
        0,
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
        SHT_STRTAB,
        0,
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
            SHT_RELA,
            0,
            0,
            relatext_off,
            relatext_size,
            idx_symtab as u32,
            idx_text as u32,
            8,
            RELA_ENT,
        );
    }

    buf.extend_from_slice(text_data);
    for sec in extra_secs {
        buf.extend_from_slice(&sec.data);
    }

    buf.extend_from_slice(&[0u8; 24]);
    for (i, sym) in obj.symbols.iter().enumerate() {
        let st_info: u8 = (1u8 << 4) | 2u8;
        let st_shndx: u16 = (sym.section + 1) as u16;
        w32(&mut buf, sym_name_offs[i]);
        buf.push(st_info);
        buf.push(0);
        w16(&mut buf, st_shndx);
        w64(&mut buf, sym.offset);
        w64(&mut buf, sym.size);
    }

    buf.extend_from_slice(&strtab);
    buf.extend_from_slice(&shstrtab);

    if has_relocs {
        for reloc in text_relocs {
            let sym_idx = (reloc.symbol + 1) as u64;
            let r_type: u64 = match reloc.kind {
                RelocKind::Pc32 => 2,
                RelocKind::Abs64 => 1,
            };
            let r_info = (sym_idx << 32) | r_type;
            w64(&mut buf, reloc.offset);
            w64(&mut buf, r_info);
            buf.extend_from_slice(&reloc.addend.to_le_bytes());
        }
    }
    buf
}

fn build_debug_line(source_file: &str, rows: &[DebugLineRow]) -> Vec<u8> {
    let file = source_file.rsplit('/').next().unwrap_or(source_file);

    let mut header_body = Vec::<u8>::new();
    header_body.push(1); // minimum_instruction_length
    header_body.push(1); // default_is_stmt
    header_body.push((-5i8) as u8); // line_base
    header_body.push(14); // line_range
    header_body.push(13); // opcode_base
    header_body.extend_from_slice(&[0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1]); // std opcode lengths
    header_body.push(0); // include_directories terminator
    header_body.extend_from_slice(file.as_bytes());
    header_body.push(0); // file name terminator
    write_uleb128(&mut header_body, 0); // dir index
    write_uleb128(&mut header_body, 0); // mtime
    write_uleb128(&mut header_body, 0); // size
    header_body.push(0); // file_names terminator

    let mut program = Vec::<u8>::new();
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(|r| r.address);
    let mut cur_addr = 0u64;
    let mut cur_line = 1u32;
    let mut cur_col = 0u32;
    for row in sorted {
        if row.address > cur_addr {
            program.push(2); // DW_LNS_advance_pc
            write_uleb128(&mut program, row.address - cur_addr);
            cur_addr = row.address;
        }
        if row.line != cur_line {
            program.push(3); // DW_LNS_advance_line
            write_sleb128(&mut program, row.line as i64 - cur_line as i64);
            cur_line = row.line;
        }
        if row.column != cur_col {
            program.push(5); // DW_LNS_set_column
            write_uleb128(&mut program, row.column as u64);
            cur_col = row.column;
        }
        program.push(1); // DW_LNS_copy
    }
    program.push(0);
    program.push(1);
    program.push(1); // DW_LNE_end_sequence

    let unit_length = (2 + 4 + header_body.len() + program.len()) as u32;
    let mut out = Vec::<u8>::new();
    w32(&mut out, unit_length);
    w16(&mut out, 2); // DWARF v2 line table
    w32(&mut out, header_body.len() as u32);
    out.extend_from_slice(&header_body);
    out.extend_from_slice(&program);
    out
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
    w32(&mut buf, 0); // reserved

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

fn write_uleb128(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if v == 0 {
            break;
        }
    }
}

fn write_sleb128(buf: &mut Vec<u8>, mut v: i64) {
    loop {
        let byte = (v as u8) & 0x7f;
        let sign = (byte & 0x40) != 0;
        v >>= 7;
        let done = (v == 0 && !sign) || (v == -1 && sign);
        if done {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
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
                debug_rows: vec![],
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
                    debug_rows: vec![],
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

    #[test]
    fn emit_object_adds_debug_line_section_for_elf() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section {
                    name: ".text".into(),
                    data: vec![0x90],
                    relocs: vec![],
                    debug_rows: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Elf
            }
        }

        let mut mf = MachineFunction::new("dbg".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });
        mf.debug_source = Some("foo.c".into());
        mf.debug_line_start = Some(17);

        let obj = emit_object(&mf, &mut NopEmitter);
        assert!(obj.sections.iter().any(|s| s.name == ".debug_line"));
        let bytes = obj.to_bytes();
        assert!(bytes.windows(11).any(|w| w == b".debug_line"));
    }
}
