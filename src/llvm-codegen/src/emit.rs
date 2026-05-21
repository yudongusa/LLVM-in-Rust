//! Object-file emission.
//!
//! Produces a minimal ELF-64, Mach-O 64-bit, or COFF relocatable object file
//! containing a single `.text` section.
//! The actual byte encoding is supplied by the target via the [`Emitter`] trait.

// ── object-file model ──────────────────────────────────────────────────────

/// Supported object-file formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFormat {
    /// `Elf` variant.
    Elf,
    /// `MachO` variant.
    MachO,
    /// `Coff` variant.
    Coff,
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
    /// Public API for `kind`.
    pub kind: RelocKind,
    /// Addend (ELF RELA / Mach-O addend).
    pub addend: i64,
    /// When set, `symbol` is ignored and this reloc references an external
    /// symbol by name (e.g. `__chkstk`). `emit_object` resolves it to an
    /// index after collecting all symbols.
    pub external_name: Option<String>,
}

/// A single source mapping row for debug line table emission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugLineRow {
    /// Public API for `address`.
    pub address: u64,
    /// Public API for `line`.
    pub line: u32,
    /// Public API for `column`.
    pub column: u32,
}

/// A named output section (`.text`, `__TEXT,__text`, etc.).
#[derive(Clone, Debug)]
pub struct Section {
    /// Public API for `name`.
    pub name: String,
    /// Public API for `data`.
    pub data: Vec<u8>,
    /// Public API for `relocs`.
    pub relocs: Vec<Reloc>,
    /// Address->source rows collected while encoding this section.
    pub debug_rows: Vec<DebugLineRow>,
}

/// A symbol definition.
#[derive(Clone, Debug)]
pub struct Symbol {
    /// Public API for `name`.
    pub name: String,
    /// Index of the section this symbol lives in.
    pub section: usize,
    /// Byte offset within that section.
    pub offset: u64,
    /// Public API for `size`.
    pub size: u64,
    /// Public API for `global`.
    pub global: bool,
    /// When `true`, this is an external undefined symbol (COFF SectionNumber=0).
    pub undefined: bool,
}

/// Assembled object file ready to be written to disk or passed to a linker.
#[derive(Clone, Debug)]
pub struct ObjectFile {
    /// Public API for `format`.
    pub format: ObjectFormat,
    /// ELF e_machine value when `format == ObjectFormat::Elf`.
    /// Ignored for Mach-O.
    pub elf_machine: u16,
    /// COFF `Machine` field when `format == ObjectFormat::Coff`.
    pub coff_machine: u16,
    /// Public API for `sections`.
    pub sections: Vec<Section>,
    /// Public API for `symbols`.
    pub symbols: Vec<Symbol>,
}

impl ObjectFile {
    /// Serialize the object file to raw bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self.format {
            ObjectFormat::Elf => serialize_elf(self),
            ObjectFormat::MachO => serialize_macho(self),
            ObjectFormat::Coff => serialize_coff(self),
        }
    }
}

// ── Emitter trait ──────────────────────────────────────────────────────────

use crate::isel::{MachineFunction, PReg};

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

    /// COFF `Machine` field for this target.
    fn coff_machine(&self) -> u16 {
        0x8664 // IMAGE_FILE_MACHINE_AMD64
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
        undefined: false,
    };
    let mut sections = vec![section];

    // Always emit baseline unwind metadata for supported object families.
    // This enables stack unwinding infrastructure to discover frame ranges
    // even when full per-instruction CFI richness is still evolving.
    match emitter.object_format() {
        ObjectFormat::Elf => {
            sections.push(Section {
                name: ".eh_frame".into(),
                data: build_eh_frame(size, mf.frame_size, &mf.used_callee_saved),
                relocs: Vec::new(),
                debug_rows: Vec::new(),
            });
        }
        ObjectFormat::Coff => {
            let (xdata, pdata) = build_coff_unwind_tables(size, mf.frame_size, &mf.used_callee_saved);
            sections.push(Section {
                name: ".xdata".into(),
                data: xdata,
                relocs: Vec::new(),
                debug_rows: Vec::new(),
            });
            sections.push(Section {
                name: ".pdata".into(),
                data: pdata,
                relocs: Vec::new(),
                debug_rows: Vec::new(),
            });
        }
        ObjectFormat::MachO => {}
    }

    let has_debug = !sections[0].debug_rows.is_empty() || mf.debug_line_start.is_some();
    if has_debug {
        let source = mf.debug_source.as_deref().unwrap_or("unknown.c");
        let rows = if !sections[0].debug_rows.is_empty() {
            sections[0].debug_rows.clone()
        } else {
            vec![DebugLineRow {
                address: 0,
                line: mf.debug_line_start.unwrap_or(1),
                column: 0,
            }]
        };
        match emitter.object_format() {
            ObjectFormat::Elf => {
                let line = build_debug_line(source, &rows);
                let abbrev = build_debug_abbrev();
                let loclists = build_debug_loclists(size);
                let info = build_debug_info(source, &mf.name, size, 0, 0, 12);
                sections.push(Section {
                    name: ".debug_abbrev".into(),
                    data: abbrev,
                    relocs: Vec::new(),
                    debug_rows: Vec::new(),
                });
                sections.push(Section {
                    name: ".debug_info".into(),
                    data: info,
                    relocs: Vec::new(),
                    debug_rows: Vec::new(),
                });
                sections.push(Section {
                    name: ".debug_line".into(),
                    data: line,
                    relocs: Vec::new(),
                    debug_rows: Vec::new(),
                });
                sections.push(Section {
                    name: ".debug_loclists".into(),
                    data: loclists,
                    relocs: Vec::new(),
                    debug_rows: Vec::new(),
                });
            }
            ObjectFormat::Coff => {
                let cv = build_codeview_debug_s(source, &rows, &mf.name, size);
                sections.push(Section {
                    name: ".debug$S".into(),
                    data: cv,
                    relocs: Vec::new(),
                    debug_rows: Vec::new(),
                });
            }
            ObjectFormat::MachO => {}
        };
    }
    let mut symbols: Vec<Symbol> = vec![sym];

    // Resolve external_name relocs: for each reloc that names an external
    // symbol, find or insert an undefined Symbol entry and set reloc.symbol.
    for sec in &mut sections {
        for reloc in &mut sec.relocs {
            if let Some(ref ext) = reloc.external_name.clone() {
                let idx = symbols.iter().position(|s| &s.name == ext).unwrap_or_else(|| {
                    let i = symbols.len();
                    symbols.push(Symbol {
                        name: ext.clone(),
                        section: 0,
                        offset: 0,
                        size: 0,
                        global: true,
                        undefined: true,
                    });
                    i
                });
                reloc.symbol = idx;
            }
        }
    }

    ObjectFile {
        format: emitter.object_format(),
        elf_machine: emitter.elf_machine(),
        coff_machine: emitter.coff_machine(),
        sections,
        symbols,
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
    const ELF_HDR: u64 = 64;
    const SH_ENT: u64 = 64;
    const SYM_ENT: u64 = 24;
    const RELA_ENT: u64 = 24;
    const SHT_PROGBITS: u32 = 1;
    const SHT_SYMTAB: u32 = 2;
    const SHT_STRTAB: u32 = 3;
    const SHT_RELA: u32 = 4;

    // SHF flags per section name heuristic.
    // .text → SHF_ALLOC|SHF_EXECINSTR (6)
    // .data → SHF_ALLOC|SHF_WRITE (3)
    // .rodata / debug / eh_frame → SHF_ALLOC (2) or 0
    let sec_flags = |name: &str| -> u64 {
        if name == ".text" || name == "__text" {
            6
        } else if name == ".data" || name == "__data" {
            3
        } else if name.starts_with(".rodata") || name.starts_with("__const") {
            2
        } else {
            0
        }
    };
    let sec_align = |name: &str| -> u64 {
        if name == ".text" || name == "__text" { 16 }
        else if name == ".data" || name.starts_with(".rodata") { 8 }
        else { 1 }
    };

    // Identify sections with relocs — we'll emit a .rela.<name> for each.
    let secs_with_relocs: Vec<usize> = obj
        .sections
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.relocs.is_empty())
        .map(|(i, _)| i)
        .collect();

    // Build shstrtab.
    let mut shstrtab: Vec<u8> = vec![0u8];
    let sec_name_offs: Vec<u32> = obj
        .sections
        .iter()
        .map(|s| push_str(&mut shstrtab, s.name.as_bytes()))
        .collect();
    let symtab_name_off = push_str(&mut shstrtab, b".symtab");
    let strtab_name_off = push_str(&mut shstrtab, b".strtab");
    let shstrtab_name_off = push_str(&mut shstrtab, b".shstrtab");
    let rela_name_offs: Vec<u32> = secs_with_relocs
        .iter()
        .map(|&si| {
            let rela_name = format!(".rela{}", obj.sections[si].name);
            push_str(&mut shstrtab, rela_name.as_bytes())
        })
        .collect();

    // Build strtab.
    let mut strtab: Vec<u8> = vec![0u8];
    let sym_name_offs: Vec<u32> = obj
        .symbols
        .iter()
        .map(|s| push_str(&mut strtab, s.name.as_bytes()))
        .collect();

    // Section-header index assignments:
    // [0]          null
    // [1..n]       data sections (obj.sections)
    // [n+1]        .symtab
    // [n+2]        .strtab
    // [n+3]        .shstrtab
    // [n+4..]      .rela.<x> for each section with relocs
    let num_data_secs = obj.sections.len() as u16;
    let idx_symtab = 1 + num_data_secs;
    let idx_strtab = idx_symtab + 1;
    let idx_shstrtab = idx_strtab + 1;
    let idx_rela_base = idx_shstrtab + 1;
    let num_sections = idx_rela_base + secs_with_relocs.len() as u16;

    let sh_table_size = num_sections as u64 * SH_ENT;
    let mut cursor = ELF_HDR + sh_table_size;

    // Lay out section data.
    let sec_offs: Vec<u64> = obj
        .sections
        .iter()
        .map(|s| {
            let off = cursor;
            cursor += s.data.len() as u64;
            off
        })
        .collect();

    let sym_count = 1 + obj.symbols.len() as u64;
    let symtab_off = cursor;
    let symtab_size = sym_count * SYM_ENT;
    cursor += symtab_size;
    let strtab_off = cursor;
    cursor += strtab.len() as u64;
    let shstrtab_off = cursor;
    cursor += shstrtab.len() as u64;

    // Lay out .rela sections.
    let rela_offs: Vec<u64> = secs_with_relocs
        .iter()
        .map(|&si| {
            let off = cursor;
            cursor += obj.sections[si].relocs.len() as u64 * RELA_ENT;
            off
        })
        .collect();

    let mut buf = Vec::<u8>::new();

    // ELF header.
    buf.extend_from_slice(b"\x7fELF");
    buf.push(2); // EI_CLASS = ELFCLASS64
    buf.push(1); // EI_DATA  = ELFDATA2LSB
    buf.push(1); // EI_VERSION
    buf.push(0); // EI_OSABI
    buf.extend_from_slice(&[0u8; 8]);
    w16(&mut buf, 1); // e_type = ET_REL
    w16(&mut buf, obj.elf_machine);
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
    w16(&mut buf, idx_shstrtab); // e_shstrndx

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

    // Null section header.
    write_shdr(&mut buf, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);

    // Data section headers.
    for (i, sec) in obj.sections.iter().enumerate() {
        write_shdr(
            &mut buf,
            sec_name_offs[i],
            SHT_PROGBITS,
            sec_flags(&sec.name),
            0,
            sec_offs[i],
            sec.data.len() as u64,
            0,
            0,
            sec_align(&sec.name),
            0,
        );
    }

    // .symtab
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
    // .strtab
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
    // .shstrtab
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

    // .rela.<section> headers.
    for (j, &si) in secs_with_relocs.iter().enumerate() {
        let rela_size = obj.sections[si].relocs.len() as u64 * RELA_ENT;
        write_shdr(
            &mut buf,
            rela_name_offs[j],
            SHT_RELA,
            0,
            0,
            rela_offs[j],
            rela_size,
            idx_symtab as u32,
            (1 + si) as u32, // sh_info = index of target section
            8,
            RELA_ENT,
        );
    }

    // Section data.
    for sec in &obj.sections {
        buf.extend_from_slice(&sec.data);
    }

    // Symbol table: null entry first.
    buf.extend_from_slice(&[0u8; 24]);
    for (i, sym) in obj.symbols.iter().enumerate() {
        let st_info: u8 = if sym.undefined {
            1u8 << 4 // STB_GLOBAL | STT_NOTYPE
        } else {
            (1u8 << 4) | 1u8 // STB_GLOBAL | STT_OBJECT (for data) or FUNC
        };
        // st_shndx: 0 for undefined, otherwise section index (1-based).
        let st_shndx: u16 = if sym.undefined {
            0
        } else {
            (sym.section + 1) as u16
        };
        w32(&mut buf, sym_name_offs[i]);
        buf.push(st_info);
        buf.push(0);
        w16(&mut buf, st_shndx);
        w64(&mut buf, sym.offset);
        w64(&mut buf, sym.size);
    }

    buf.extend_from_slice(&strtab);
    buf.extend_from_slice(&shstrtab);

    // Relocation tables.
    for &si in &secs_with_relocs {
        for reloc in &obj.sections[si].relocs {
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

    buf
}

fn build_debug_line(source_file: &str, rows: &[DebugLineRow]) -> Vec<u8> {
    let file = source_file.rsplit('/').next().unwrap_or(source_file);

    let mut header_body = vec![
        1u8, // minimum_instruction_length
        1,   // default_is_stmt
        (-5i8) as u8, // line_base
        14,  // line_range
        13,  // opcode_base
    ];
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

fn build_debug_abbrev() -> Vec<u8> {
    // DWARF5 abbrev set:
    // 1: CU (children=yes)
    // 2: subprogram (children=yes)
    // 3: variable (children=no)
    // 4: base_type (children=no)
    const DW_TAG_COMPILE_UNIT: u64 = 0x11;
    const DW_TAG_SUBPROGRAM: u64 = 0x2e;
    const DW_TAG_VARIABLE: u64 = 0x34;
    const DW_TAG_BASE_TYPE: u64 = 0x24;

    const DW_CHILDREN_NO: u8 = 0x00;
    const DW_CHILDREN_YES: u8 = 0x01;

    const DW_AT_NAME: u64 = 0x03;
    const DW_AT_STMT_LIST: u64 = 0x10;
    const DW_AT_LOW_PC: u64 = 0x11;
    const DW_AT_HIGH_PC: u64 = 0x12;
    const DW_AT_COMP_DIR: u64 = 0x1b;
    const DW_AT_ENCODING: u64 = 0x3e;
    const DW_AT_BYTE_SIZE: u64 = 0x0b;
    const DW_AT_LOCLISTS_BASE: u64 = 0x8c;

    const DW_FORM_ADDR: u64 = 0x01;
    const DW_FORM_DATA1: u64 = 0x0b;
    const DW_FORM_DATA8: u64 = 0x07;
    const DW_FORM_STRING: u64 = 0x08;
    const DW_FORM_SEC_OFFSET: u64 = 0x17;

    let mut out = Vec::new();

    // Abbrev code 1: compile_unit
    write_uleb128(&mut out, 1);
    write_uleb128(&mut out, DW_TAG_COMPILE_UNIT);
    out.push(DW_CHILDREN_YES);
    write_uleb128(&mut out, DW_AT_NAME);
    write_uleb128(&mut out, DW_FORM_STRING);
    write_uleb128(&mut out, DW_AT_STMT_LIST);
    write_uleb128(&mut out, DW_FORM_SEC_OFFSET);
    write_uleb128(&mut out, DW_AT_COMP_DIR);
    write_uleb128(&mut out, DW_FORM_STRING);
    write_uleb128(&mut out, DW_AT_LOW_PC);
    write_uleb128(&mut out, DW_FORM_ADDR);
    write_uleb128(&mut out, DW_AT_HIGH_PC);
    write_uleb128(&mut out, DW_FORM_DATA8);
    write_uleb128(&mut out, DW_AT_LOCLISTS_BASE);
    write_uleb128(&mut out, DW_FORM_SEC_OFFSET);
    out.push(0);
    out.push(0);

    // Abbrev code 2: subprogram
    write_uleb128(&mut out, 2);
    write_uleb128(&mut out, DW_TAG_SUBPROGRAM);
    out.push(DW_CHILDREN_YES);
    write_uleb128(&mut out, DW_AT_NAME);
    write_uleb128(&mut out, DW_FORM_STRING);
    write_uleb128(&mut out, DW_AT_LOW_PC);
    write_uleb128(&mut out, DW_FORM_ADDR);
    write_uleb128(&mut out, DW_AT_HIGH_PC);
    write_uleb128(&mut out, DW_FORM_DATA8);
    out.push(0);
    out.push(0);

    // Abbrev code 3: variable (keep minimal to satisfy verifier)
    write_uleb128(&mut out, 3);
    write_uleb128(&mut out, DW_TAG_VARIABLE);
    out.push(DW_CHILDREN_NO);
    write_uleb128(&mut out, DW_AT_NAME);
    write_uleb128(&mut out, DW_FORM_STRING);
    out.push(0);
    out.push(0);

    // Abbrev code 4: base_type (e.g. i64)
    write_uleb128(&mut out, 4);
    write_uleb128(&mut out, DW_TAG_BASE_TYPE);
    out.push(DW_CHILDREN_NO);
    write_uleb128(&mut out, DW_AT_NAME);
    write_uleb128(&mut out, DW_FORM_STRING);
    write_uleb128(&mut out, DW_AT_ENCODING);
    write_uleb128(&mut out, DW_FORM_DATA1);
    write_uleb128(&mut out, DW_AT_BYTE_SIZE);
    write_uleb128(&mut out, DW_FORM_DATA1);
    out.push(0);
    out.push(0);

    // End of abbrev table
    out.push(0);
    out
}

fn build_debug_info(
    source_file: &str,
    fn_name: &str,
    text_size: u64,
    stmt_list_off: u32,
    _abbrev_off: u32,
    _loclists_var_off: u32,
) -> Vec<u8> {
    const DWARF_VERSION: u16 = 5;
    const DW_UT_COMPILE: u8 = 0x01;
    const DW_ATE_SIGNED: u8 = 0x05;

    let file = source_file.rsplit('/').next().unwrap_or(source_file);
    let comp_dir = source_file
        .rfind('/')
        .map(|i| &source_file[..i])
        .filter(|s| !s.is_empty())
        .unwrap_or(".");

    let mut body = Vec::new();

    // DIE: compile unit (abbrev 1)
    write_uleb128(&mut body, 1);
    body.extend_from_slice(file.as_bytes());
    body.push(0);
    w32(&mut body, stmt_list_off);
    body.extend_from_slice(comp_dir.as_bytes());
    body.push(0);
    w64(&mut body, 0); // low_pc
    w64(&mut body, text_size); // high_pc as address range size
    w32(&mut body, 0); // DW_AT_loclists_base (base for indexed loclists)

    // DIE: subprogram (abbrev 2)
    write_uleb128(&mut body, 2);
    body.extend_from_slice(fn_name.as_bytes());
    body.push(0);
    w64(&mut body, 0);
    w64(&mut body, text_size);

    // DIE: variable (abbrev 3)
    write_uleb128(&mut body, 3);
    body.extend_from_slice(b"result");
    body.push(0);

    // End children of subprogram.
    body.push(0);

    // DIE: base_type (abbrev 4)
    write_uleb128(&mut body, 4);
    body.extend_from_slice(b"i64");
    body.push(0);
    body.push(DW_ATE_SIGNED);
    body.push(8);

    // End children of CU.
    body.push(0);

    let mut out = Vec::new();
    let unit_length = (2 + 1 + 1 + 4 + body.len()) as u32;
    w32(&mut out, unit_length);
    w16(&mut out, DWARF_VERSION);
    out.push(DW_UT_COMPILE);
    out.push(8); // address size
    w32(&mut out, 0); // abbrev offset
    out.extend_from_slice(&body);
    out
}

fn build_debug_loclists(text_size: u64) -> Vec<u8> {
    // DWARF5 .debug_loclists with one list at offset 12 (after header).
    // List entries:
    //   DW_LLE_offset_pair [0, text_size] exprloc(DW_OP_reg0)
    //   DW_LLE_end_of_list
    // Use offset-pair (relative to CU low_pc) to avoid absolute relocations in .o files.
    const DW_LLE_END_OF_LIST: u8 = 0x00;
    const DW_LLE_OFFSET_PAIR: u8 = 0x04;
    const DW_OP_REG0: u8 = 0x50;

    let mut body = Vec::new();
    body.push(DW_LLE_OFFSET_PAIR);
    write_uleb128(&mut body, 0);
    write_uleb128(&mut body, text_size.max(1));
    body.push(1); // exprloc length
    body.push(DW_OP_REG0);
    body.push(DW_LLE_END_OF_LIST);

    let mut out = Vec::new();
    let unit_length = (2 + 1 + 1 + 4 + body.len()) as u32;
    w32(&mut out, unit_length);
    w16(&mut out, 5); // DWARF v5
    out.push(8); // address size
    out.push(0); // segment selector size
    w32(&mut out, 0); // offset entry count
    out.extend_from_slice(&body);
    out
}

fn build_eh_frame(text_size: u64, frame_size: u32, used_callee_saved: &[PReg]) -> Vec<u8> {
    // Baseline .eh_frame with one CIE/FDE, now shaped by frame facts.
    // CIE augmentation uses zR and encodes FDE pointers as pcrel/sdata4.
    let mut out = Vec::new();

    let mut cie = Vec::new();
    cie.push(1); // version
    cie.extend_from_slice(b"zR\0"); // augmentation
    write_uleb128(&mut cie, 1); // code alignment factor
    write_sleb128(&mut cie, -8); // data alignment factor
    write_uleb128(&mut cie, 16); // return address register (RIP)
    write_uleb128(&mut cie, 1); // augmentation data length
    cie.push(0x1b); // DW_EH_PE_pcrel | DW_EH_PE_sdata4

    // Initial canonical frame: CFA = rsp + 8, RA saved at cfa-8.
    cie.push(0x0c); // DW_CFA_def_cfa
    write_uleb128(&mut cie, 7); // rsp
    write_uleb128(&mut cie, 8);
    cie.push(0x90); // DW_CFA_offset + r16 (rip)
    write_uleb128(&mut cie, 1);

    w32(&mut out, cie.len() as u32 + 4);
    w32(&mut out, 0); // CIE id
    out.extend_from_slice(&cie);
    while out.len() % 8 != 0 {
        out.push(0);
    }

    let fde_start = out.len();
    let mut fde = Vec::new();
    w32(&mut fde, 0); // initial_location (placeholder in object file)
    w32(&mut fde, text_size.max(1) as u32); // address range

    // Build FDE instruction stream from frame shape.
    let mut fde_prog = Vec::new();
    let mut cfa_off = 8u64;

    // Account for frame pointer setup and pushed callee-saved registers.
    let pushes = if frame_size > 0 || !used_callee_saved.is_empty() {
        1 + used_callee_saved.len() as u64 // push rbp + pushes
    } else {
        0
    };

    if pushes > 0 {
        cfa_off += pushes * 8;
        fde_prog.push(0x0e); // DW_CFA_def_cfa_offset
        write_uleb128(&mut fde_prog, cfa_off);

        // rbp saved at CFA-16 after push rbp + call return address.
        fde_prog.push(0x86); // DW_CFA_offset + r6 (rbp)
        write_uleb128(&mut fde_prog, 2);

        for (idx, pr) in used_callee_saved.iter().enumerate() {
            let reg = pr.0;
            if reg <= 0x3f {
                fde_prog.push(0x80 | reg); // DW_CFA_offset + reg
                write_uleb128(&mut fde_prog, 3 + idx as u64); // after RA+RBP
            }
        }

        // set CFA register to rbp once prologue establishes frame pointer.
        fde_prog.push(0x0d); // DW_CFA_def_cfa_register
        write_uleb128(&mut fde_prog, 6); // rbp
    }

    if frame_size > 0 {
        cfa_off += frame_size as u64;
        fde_prog.push(0x0e); // DW_CFA_def_cfa_offset
        write_uleb128(&mut fde_prog, cfa_off);
    }

    write_uleb128(&mut fde, fde_prog.len() as u64); // augmentation data length
    fde.extend_from_slice(&fde_prog);

    w32(&mut out, fde.len() as u32 + 4);
    let cie_ptr = fde_start as u32;
    w32(&mut out, cie_ptr); // CIE pointer (offset back to CIE at 0)
    out.extend_from_slice(&fde);
    while out.len() % 8 != 0 {
        out.push(0);
    }

    w32(&mut out, 0); // terminator
    out
}

fn build_coff_unwind_tables(text_size: u64, frame_size: u32, used_callee_saved: &[PReg]) -> (Vec<u8>, Vec<u8>) {
    // Build UNWIND_INFO from prologue facts.
    //
    // Normal prologue byte layout (alloc ≤ 4096):
    //   push rbp            1 byte  → CodeOffset = 1
    //   mov rbp, rsp        3 bytes → no unwind code (frame setup)
    //   sub rsp, N (imm8)   4 bytes → CodeOffset = 8  (alloc ≤ 128)
    //   sub rsp, N (imm32)  7 bytes → CodeOffset = 11 (alloc > 128)
    //
    // __chkstk prologue (alloc > 4096, COFF only):
    //   push rbp            1 byte  → CodeOffset = 1
    //   mov rbp, rsp        3 bytes
    //   [callee-saved pushes]
    //   mov rax, N         10 bytes
    //   call __chkstk       5 bytes
    //   sub rsp, rax        3 bytes → CodeOffset = 22 + callee_push_instr_bytes
    //
    // Per the Win64 ABI, UNWIND_CODE slots must be ordered by CodeOffset
    // in DESCENDING order (last prologue instruction first).
    let has_frame = frame_size > 0 || !used_callee_saved.is_empty();

    // Round frame allocation up to 16-byte boundary.
    let alloc_size: u32 = if frame_size == 0 { 0 } else { (frame_size + 15) & !15 };

    // Instruction bytes consumed by callee-saved pushes (1 byte for rax-rdi, 2 for r8-r15).
    let callee_push_instr_bytes: u32 = used_callee_saved
        .iter()
        .map(|pr| if pr.0 >= 8 { 2u32 } else { 1u32 })
        .sum();

    let uses_chkstk = alloc_size > 4096;

    // code_bytes holds raw UNWIND_CODE slots (2 bytes each) in descending offset order.
    let mut code_bytes: Vec<u8> = Vec::new();
    let mut slot_count: u8 = 0;

    // Alloc code comes first (highest CodeOffset).
    if alloc_size > 0 && alloc_size <= 128 {
        // UWOP_ALLOC_SMALL (op=2, info=(alloc/8)-1): 1 slot
        let info = ((alloc_size / 8) - 1) as u8;
        code_bytes.push(8); // CodeOffset: byte after `sub rsp, imm8` (4-byte encoding)
        code_bytes.push((info << 4) | 0x02);
        slot_count += 1;
    } else if alloc_size > 128 {
        // UWOP_ALLOC_LARGE (op=1, info=0): 2 slots
        // Slot 0: {CodeOffset, UWOP_ALLOC_LARGE | info=0}
        // Slot 1 (u16): alloc_size / 8
        let alloc_code_off: u8 = if uses_chkstk {
            // push rbp(1) + mov rbp,rsp(3) + callee_pushes + mov rax,N(10) + call(5) + sub rsp,rax(3)
            (22 + callee_push_instr_bytes) as u8
        } else {
            11 // byte after `sub rsp, imm32` (7-byte encoding)
        };
        code_bytes.push(alloc_code_off);
        code_bytes.push(0x01); // UWOP_ALLOC_LARGE = 1, info = 0
        let sz16 = (alloc_size / 8) as u16;
        code_bytes.extend_from_slice(&sz16.to_le_bytes());
        slot_count += 2;
    }

    // PUSH_NONVOL for RBP comes second (lower CodeOffset = 1).
    if has_frame {
        code_bytes.push(1); // CodeOffset: byte after `push rbp`
        code_bytes.push(5 << 4); // UWOP_PUSH_NONVOL = 0, info = RBP (5)
        slot_count += 1;
    }

    let prolog_size: u8 = if uses_chkstk {
        (22 + callee_push_instr_bytes) as u8
    } else if alloc_size > 128 {
        11
    } else if alloc_size > 0 {
        8
    } else if has_frame {
        4
    } else {
        0
    };

    let mut xdata = Vec::new();
    xdata.push(0x01); // version=1, flags=0
    xdata.push(prolog_size);
    xdata.push(slot_count);
    xdata.push(if has_frame { 5 } else { 0 }); // FrameRegister=RBP(5), FrameOffset=0

    xdata.extend_from_slice(&code_bytes);

    // Align unwind info to 4-byte boundary.
    while xdata.len() % 4 != 0 {
        xdata.push(0);
    }

    // One RUNTIME_FUNCTION entry in .pdata:
    // BeginAddress, EndAddress, UnwindInfoAddress (all image-relative u32).
    let mut pdata = Vec::new();
    w32(&mut pdata, 0);
    w32(&mut pdata, text_size.max(1) as u32);
    w32(&mut pdata, 0); // points at start of .xdata in this object's section space

    (xdata, pdata)
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
    while !strtab.len().is_multiple_of(4) {
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

// ── COFF (PE/COFF object) serialization ───────────────────────────────────

fn serialize_coff(obj: &ObjectFile) -> Vec<u8> {
    const FILE_HEADER_SIZE: usize = 20;
    const SECTION_HEADER_SIZE: usize = 40;
    const RELOC_SIZE: usize = 10;
    const SYMBOL_SIZE: usize = 18;

    let nsec = obj.sections.len();
    let sec_headers_size = nsec * SECTION_HEADER_SIZE;
    let sec_data_start = FILE_HEADER_SIZE + sec_headers_size;

    let mut data_ptr = sec_data_start as u32;
    let mut sec_raw_ptrs = Vec::with_capacity(nsec);
    let mut sec_reloc_ptrs = Vec::with_capacity(nsec);
    for sec in &obj.sections {
        let raw_size = sec.data.len() as u32;
        let reloc_size = (sec.relocs.len() * RELOC_SIZE) as u32;
        sec_raw_ptrs.push(data_ptr);
        data_ptr = data_ptr.wrapping_add(raw_size);
        sec_reloc_ptrs.push(if reloc_size > 0 { data_ptr } else { 0 });
        data_ptr = data_ptr.wrapping_add(reloc_size);
    }

    let symtab_ptr = data_ptr;
    let nsym = obj.symbols.len() as u32;
    // COFF string table: u32 size + NUL-terminated strings.
    let mut strtab: Vec<u8> = vec![0, 0, 0, 0];
    let mut section_name_offs = Vec::with_capacity(nsec);
    let mut symbol_name_offs = Vec::with_capacity(obj.symbols.len());
    for sec in &obj.sections {
        section_name_offs.push(append_coff_string(&mut strtab, &sec.name));
    }
    for sym in &obj.symbols {
        symbol_name_offs.push(append_coff_string(&mut strtab, &sym.name));
    }
    let strtab_size = strtab.len() as u32;
    strtab[0..4].copy_from_slice(&strtab_size.to_le_bytes());

    let total_est = symtab_ptr as usize + nsym as usize * SYMBOL_SIZE + strtab.len();
    let mut buf = Vec::with_capacity(total_est);

    // IMAGE_FILE_HEADER
    w16(&mut buf, obj.coff_machine); // Machine
    w16(&mut buf, nsec as u16); // NumberOfSections
    w32(&mut buf, 0); // TimeDateStamp
    w32(&mut buf, symtab_ptr); // PointerToSymbolTable
    w32(&mut buf, nsym); // NumberOfSymbols
    w16(&mut buf, 0); // SizeOfOptionalHeader
    w16(&mut buf, 0); // Characteristics

    // IMAGE_SECTION_HEADER
    for (i, sec) in obj.sections.iter().enumerate() {
        write_coff_name_field(&mut buf, &sec.name, section_name_offs[i]);
        w32(&mut buf, 0); // VirtualSize
        w32(&mut buf, 0); // VirtualAddress
        w32(&mut buf, sec.data.len() as u32); // SizeOfRawData
        w32(&mut buf, sec_raw_ptrs[i]); // PointerToRawData
        w32(&mut buf, sec_reloc_ptrs[i]); // PointerToRelocations
        w32(&mut buf, 0); // PointerToLinenumbers
        w16(&mut buf, sec.relocs.len() as u16); // NumberOfRelocations
        w16(&mut buf, 0); // NumberOfLinenumbers
        w32(&mut buf, coff_section_characteristics(&sec.name));
    }

    // section data + relocations
    for sec in &obj.sections {
        buf.extend_from_slice(&sec.data);
        for reloc in &sec.relocs {
            w32(&mut buf, reloc.offset as u32); // VirtualAddress
            w32(&mut buf, reloc.symbol as u32); // SymbolTableIndex
            let typ = match reloc.kind {
                RelocKind::Pc32 => 0x0004,  // IMAGE_REL_AMD64_REL32
                RelocKind::Abs64 => 0x0001, // IMAGE_REL_AMD64_ADDR64
            };
            w16(&mut buf, typ);
        }
    }

    // symbol table
    for (i, sym) in obj.symbols.iter().enumerate() {
        write_coff_name_field(&mut buf, &sym.name, symbol_name_offs[i]);
        w32(&mut buf, sym.offset as u32); // Value
        if sym.undefined {
            // SectionNumber = 0 means external/undefined
            w16(&mut buf, 0u16);
        } else {
            w16(&mut buf, (sym.section + 1) as u16); // SectionNumber (1-based)
        }
        // Type: IMAGE_SYM_DTYPE_FUNCTION (2) << 4 | IMAGE_SYM_TYPE_NULL (0) = 0x0020
        w16(&mut buf, if sym.global { 0x0020 } else { 0 });
        buf.push(if sym.global { 2 } else { 3 }); // StorageClass: EXTERNAL or STATIC
        buf.push(0); // NumberOfAuxSymbols
    }

    // string table
    buf.extend_from_slice(&strtab);
    buf
}

fn append_coff_string(strtab: &mut Vec<u8>, s: &str) -> u32 {
    let off = strtab.len() as u32;
    strtab.extend_from_slice(s.as_bytes());
    strtab.push(0);
    off
}

fn write_coff_name_field(buf: &mut Vec<u8>, name: &str, strtab_off: u32) {
    if name.len() <= 8 {
        let mut raw = [0u8; 8];
        raw[..name.len()].copy_from_slice(name.as_bytes());
        buf.extend_from_slice(&raw);
    } else {
        let tag = format!("/{}", strtab_off);
        let mut raw = [0u8; 8];
        let bytes = tag.as_bytes();
        let n = bytes.len().min(8);
        raw[..n].copy_from_slice(&bytes[..n]);
        buf.extend_from_slice(&raw);
    }
}

fn coff_section_characteristics(name: &str) -> u32 {
    match name {
        ".text" => 0x60000020,  // CNT_CODE | MEM_EXECUTE | MEM_READ
        // .pdata/.xdata require explicit alignment so the SEH unwinder can
        // locate entries at load time; lld-link rejects missing alignment flags.
        ".pdata" => 0x40300040, // CNT_INITIALIZED_DATA | MEM_READ | ALIGN_4BYTES
        ".xdata" => 0x40400040, // CNT_INITIALIZED_DATA | MEM_READ | ALIGN_8BYTES
        ".rdata" => 0x40000040, // CNT_INITIALIZED_DATA | MEM_READ
        n if n.starts_with(".debug") => 0x42000040, // CNT_INITIALIZED_DATA | MEM_READ | MEM_DISCARDABLE
        _ => 0x40000040, // CNT_INITIALIZED_DATA | MEM_READ
    }
}

fn build_codeview_debug_s(
    source_file: &str,
    rows: &[DebugLineRow],
    func_name: &str,
    text_size: u64,
) -> Vec<u8> {
    // .debug$S section format: CV_SIGNATURE_C13 followed by subsections.
    //
    // Subsection order required for WinDbg source-line resolution:
    //   1. DEBUG_S_SYMBOLS  (0xF1) — S_GPROC32 + S_END
    //   2. DEBUG_S_FILECHKSMS (0xF4) — file checksum table
    //   3. DEBUG_S_STRINGTABLE (0xF3) — source file paths
    //   4. DEBUG_S_LINES    (0xF2) — code-offset → line mapping
    //
    // Each subsection: u32 kind, u32 length, [data], [align pad to 4].
    let mut out = Vec::new();
    w32(&mut out, 4); // CV_SIGNATURE_C13

    // ── 1. DEBUG_S_SYMBOLS ───────────────────────────────────────────────
    //
    // S_GPROC32 (0x1110) records a global procedure with its address range
    // so WinDbg can map instruction addresses to function names.
    //
    // S_GPROC32 field layout (cvinfo.h PROCSYM32):
    //   u16  reclen   — byte count of everything after this field
    //   u16  rectype  — 0x1110
    //   u32  pParent  — enclosing scope (0 = none)
    //   u32  pEnd     — end-of-block back-pointer (0, filled by linker)
    //   u32  pNext    — next sibling (0)
    //   u32  len      — procedure length in bytes
    //   u32  DbgStart — first debug-start offset (0 = start of proc)
    //   u32  DbgEnd   — first debug-end offset (len - 1 typically)
    //   u32  typind   — type index (0 = untyped/void)
    //   u32  off      — section-relative offset (0 for relocatable)
    //   u16  seg      — section number (1-based; .text is section 1)
    //   u8   flags    — CV_PROCFLAGS (0)
    //   char name[]   — null-terminated function name
    let name_bytes = func_name.as_bytes();
    // reclen = 2(rectype)+4*9+2+1+strlen+1 = 2+36+2+1+len+1 = 42+len
    let reclen: u16 = 42 + name_bytes.len() as u16;

    let mut sym_payload: Vec<u8> = Vec::new();
    // S_GPROC32
    w16(&mut sym_payload, reclen);
    w16(&mut sym_payload, 0x1110); // S_GPROC32
    w32(&mut sym_payload, 0); // pParent
    w32(&mut sym_payload, 0); // pEnd
    w32(&mut sym_payload, 0); // pNext
    w32(&mut sym_payload, text_size as u32); // len
    w32(&mut sym_payload, 0); // DbgStart
    w32(&mut sym_payload, text_size.saturating_sub(1) as u32); // DbgEnd
    w32(&mut sym_payload, 0); // typind (T_NOTYPE)
    w32(&mut sym_payload, 0); // off (section-relative; relocated to RVA by linker)
    w16(&mut sym_payload, 1); // seg (.text = section 1)
    sym_payload.push(0); // flags
    sym_payload.extend_from_slice(name_bytes);
    sym_payload.push(0); // null terminator
    // S_END (0x0006) — closes the GPROC32 block
    w16(&mut sym_payload, 2); // reclen
    w16(&mut sym_payload, 0x0006); // S_END

    emit_cv_subsection(&mut out, 0xF1, &sym_payload);

    // ── 2. DEBUG_S_FILECHKSMS ─────────────────────────────────────────────
    //
    // One 8-byte entry per source file:
    //   u32  offFilename   — byte offset of filename in STRINGTABLE
    //   u16  cbChecksum    — 0 (no checksum)
    //   u8   ChecksumType  — 0 (CHKSUM_TYPE_NONE)
    //   u8   padding       — 0
    //
    // Our STRINGTABLE starts with the filename at offset 0.
    let mut chksm_payload: Vec<u8> = Vec::new();
    w32(&mut chksm_payload, 0); // offFilename → offset 0 in STRINGTABLE
    w16(&mut chksm_payload, 0); // cbChecksum
    chksm_payload.push(0); // ChecksumType
    chksm_payload.push(0); // padding

    emit_cv_subsection(&mut out, 0xF4, &chksm_payload);

    // ── 3. DEBUG_S_STRINGTABLE ────────────────────────────────────────────
    //
    // Null-terminated source file path at offset 0.
    let mut strtab_payload: Vec<u8> = Vec::new();
    strtab_payload.extend_from_slice(source_file.as_bytes());
    strtab_payload.push(0);

    emit_cv_subsection(&mut out, 0xF3, &strtab_payload);

    // ── 4. DEBUG_S_LINES ──────────────────────────────────────────────────
    //
    // Maps code offsets within this function to source lines.
    //
    // Header (12 bytes):
    //   u32  offCon   — section-relative code start (0)
    //   u16  iSeg     — section index (1)
    //   u16  flags    — 0 (no column info)
    //   u32  cbCon    — byte count of code described
    //
    // File block per source file (12 bytes):
    //   u32  fileid      — byte offset into FILECHKSMS subsection
    //   u32  nLines      — count of line entries in this block
    //   u32  cbFileInfo  — 12 + nLines * 8
    //
    // Line entries (8 bytes each):
    //   u32  offset   — code offset from start of proc
    //   u32  lineInfo — bits 0-23: line number, bit 31: 0 (statement)
    let nlines = rows.len() as u32;
    let cb_file_info: u32 = 12 + nlines * 8;

    let mut lines_payload: Vec<u8> = Vec::new();
    // Header
    w32(&mut lines_payload, 0); // offCon (section-relative)
    w16(&mut lines_payload, 1); // iSeg (.text = section 1)
    w16(&mut lines_payload, 0); // flags (no columns)
    w32(&mut lines_payload, text_size as u32); // cbCon
    // File block
    w32(&mut lines_payload, 0); // fileid (offset 0 in FILECHKSMS)
    w32(&mut lines_payload, nlines);
    w32(&mut lines_payload, cb_file_info);
    // Line entries
    for row in rows {
        w32(&mut lines_payload, row.address as u32); // code offset
        w32(&mut lines_payload, row.line & 0x00FF_FFFF); // line number (24 bits)
    }

    emit_cv_subsection(&mut out, 0xF2, &lines_payload);

    out
}

fn emit_cv_subsection(out: &mut Vec<u8>, kind: u32, data: &[u8]) {
    w32(out, kind);
    w32(out, data.len() as u32);
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
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
            ObjectFormat::Coff => ".text",
        };
        ObjectFile {
            format: fmt,
            elf_machine: 62,
            coff_machine: 0x8664,
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
                undefined: false,
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
    fn coff_machine_x86_64() {
        let bytes = make_obj(ObjectFormat::Coff, vec![0x90]).to_bytes();
        let machine = u16::from_le_bytes([bytes[0], bytes[1]]);
        assert_eq!(machine, 0x8664, "IMAGE_FILE_MACHINE_AMD64");
    }

    #[test]
    fn coff_has_text_section_header() {
        let bytes = make_obj(ObjectFormat::Coff, vec![0x90]).to_bytes();
        let sec_count = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(sec_count, 1);
        let sec_name = &bytes[20..28];
        assert_eq!(sec_name, b".text\0\0\0");
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
        assert_eq!(obj.coff_machine, 0x8664);
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
        assert!(obj.sections.iter().any(|s| s.name == ".debug_info"));
        assert!(obj.sections.iter().any(|s| s.name == ".debug_abbrev"));
        assert!(obj.sections.iter().any(|s| s.name == ".debug_loclists"));
        let bytes = obj.to_bytes();
        assert!(bytes.windows(11).any(|w| w == b".debug_line"));
        assert!(bytes.windows(11).any(|w| w == b".debug_info"));
        assert!(bytes.windows(13).any(|w| w == b".debug_abbrev"));
        assert!(bytes.windows(15).any(|w| w == b".debug_loclists"));
    }

    #[test]
    fn emit_object_adds_eh_frame_for_elf() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section {
                    name: ".text".into(),
                    data: vec![0x90, 0xC3],
                    relocs: vec![],
                    debug_rows: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Elf
            }
        }

        let mut mf = MachineFunction::new("eh".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });

        let obj = emit_object(&mf, &mut NopEmitter);
        let eh = obj
            .sections
            .iter()
            .find(|s| s.name == ".eh_frame")
            .expect(".eh_frame section");
        assert!(!eh.data.is_empty());
        let bytes = obj.to_bytes();
        assert!(bytes.windows(9).any(|w| w == b".eh_frame"));
    }

    #[test]
    fn emit_object_adds_unwind_tables_for_coff() {
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
                ObjectFormat::Coff
            }
        }

        let mut mf = MachineFunction::new("seh".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });

        let obj = emit_object(&mf, &mut NopEmitter);
        assert!(obj.sections.iter().any(|s| s.name == ".xdata"));
        assert!(obj.sections.iter().any(|s| s.name == ".pdata"));
        let bytes = obj.to_bytes();
        assert!(bytes.windows(6).any(|w| w == b".xdata"));
        assert!(bytes.windows(6).any(|w| w == b".pdata"));
    }

    #[test]
    fn eh_frame_reflects_frame_facts_when_present() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section {
                    name: ".text".into(),
                    data: vec![0x90, 0x90, 0xC3],
                    relocs: vec![],
                    debug_rows: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Elf
            }
        }

        let mut mf = MachineFunction::new("eh-facts".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });
        mf.frame_size = 16;
        mf.used_callee_saved = vec![PReg(3), PReg(12)]; // rbx, r12

        let obj = emit_object(&mf, &mut NopEmitter);
        let eh = obj
            .sections
            .iter()
            .find(|s| s.name == ".eh_frame")
            .expect(".eh_frame section")
            .data
            .clone();

        // Expect def_cfa_offset opcode (0x0e) in FDE program when frame facts exist.
        assert!(eh.contains(&0x0e), "expected DW_CFA_def_cfa_offset in frame-aware FDE");
        // Expect def_cfa_register opcode (0x0d) when frame pointer model is active.
        assert!(eh.contains(&0x0d), "expected DW_CFA_def_cfa_register in frame-aware FDE");
    }

    #[test]
    fn eh_frame_has_expected_cie_fde_shape() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section {
                    name: ".text".into(),
                    data: vec![0x90, 0x90, 0xC3],
                    relocs: vec![],
                    debug_rows: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Elf
            }
        }

        let mut mf = MachineFunction::new("eh-shape".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });

        let obj = emit_object(&mf, &mut NopEmitter);
        let eh = obj
            .sections
            .iter()
            .find(|s| s.name == ".eh_frame")
            .expect(".eh_frame section")
            .data
            .clone();

        // CIE starts at offset 0.
        let cie_len = u32::from_le_bytes([eh[0], eh[1], eh[2], eh[3]]) as usize;
        assert!(cie_len > 8, "CIE should have payload");
        let cie_id = u32::from_le_bytes([eh[4], eh[5], eh[6], eh[7]]);
        assert_eq!(cie_id, 0, "CIE id must be zero");
        assert_eq!(eh[8], 1, "CIE version");
        assert!(eh.windows(3).any(|w| w == b"zR\0"), "CIE augmentation zR");

        // FDE starts at aligned boundary after first record.
        let fde_off = (4 + cie_len + 7) & !7;
        let fde_len = u32::from_le_bytes([eh[fde_off], eh[fde_off + 1], eh[fde_off + 2], eh[fde_off + 3]]) as usize;
        assert!(fde_len >= 12, "FDE should contain init loc + range + aug len");

        // FDE payload: [CIE ptr][init loc][range][aug-len]
        let range_off = fde_off + 4 + 4 + 4;
        let range = u32::from_le_bytes([eh[range_off], eh[range_off + 1], eh[range_off + 2], eh[range_off + 3]]);
        assert_eq!(range, 3, "FDE range should match text size");

        // .eh_frame terminator record exists.
        assert_eq!(&eh[eh.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn coff_unwind_tables_encode_frame_facts_when_present() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section {
                    name: ".text".into(),
                    data: vec![0x90, 0x90, 0xC3],
                    relocs: vec![],
                    debug_rows: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Coff
            }
        }

        let mut mf = MachineFunction::new("coff-unwind-facts".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });
        mf.frame_size = 16;
        mf.used_callee_saved = vec![PReg(3)]; // rbx

        let obj = emit_object(&mf, &mut NopEmitter);
        let xdata = obj
            .sections
            .iter()
            .find(|s| s.name == ".xdata")
            .expect(".xdata section")
            .data
            .clone();
        assert!(xdata.len() >= 8, "unwind info should include at least one code slot");
        assert_eq!(xdata[0] & 0x7, 1, "UNWIND_INFO version 1");
        assert!(xdata[2] >= 1, "count_of_codes should be non-zero when frame facts exist");
        assert_eq!(xdata[3] & 0x0f, 5, "frame register should be RBP");
    }

    #[test]
    fn coff_unwind_tables_have_expected_layout() {
        use crate::isel::{MachineBlock, MachineFunction};

        struct NopEmitter;
        impl Emitter for NopEmitter {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section {
                    name: ".text".into(),
                    data: vec![0x90, 0xC3],
                    relocs: vec![],
                    debug_rows: vec![],
                }
            }
            fn object_format(&self) -> ObjectFormat {
                ObjectFormat::Coff
            }
        }

        let mut mf = MachineFunction::new("coff-unwind-shape".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });

        let obj = emit_object(&mf, &mut NopEmitter);
        let xdata = obj
            .sections
            .iter()
            .find(|s| s.name == ".xdata")
            .expect(".xdata section")
            .data
            .clone();
        let pdata = obj
            .sections
            .iter()
            .find(|s| s.name == ".pdata")
            .expect(".pdata section")
            .data
            .clone();

        assert_eq!(xdata.len(), 4, "minimal UNWIND_INFO header");
        assert_eq!(xdata[0] & 0x7, 1, "UNWIND_INFO version 1");
        assert_eq!(pdata.len(), 12, "single RUNTIME_FUNCTION entry");

        let begin = u32::from_le_bytes([pdata[0], pdata[1], pdata[2], pdata[3]]);
        let end = u32::from_le_bytes([pdata[4], pdata[5], pdata[6], pdata[7]]);
        let unwind_info_rva = u32::from_le_bytes([pdata[8], pdata[9], pdata[10], pdata[11]]);

        assert_eq!(begin, 0);
        assert_eq!(end, 2, "runtime function end should match text size");
        assert_eq!(unwind_info_rva, 0, "points at section-local xdata start in this object model");
    }

    #[test]
    fn emit_object_adds_debug_s_section_for_coff() {
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
                ObjectFormat::Coff
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
        assert!(obj.sections.iter().any(|s| s.name == ".debug$S"));
        let bytes = obj.to_bytes();
        assert!(bytes.windows(8).any(|w| w == b".debug$S"));
    }

    // ── helpers for COFF section-header inspection ─────────────────────────

    /// Find the characteristics field for a named section in a raw COFF file.
    fn coff_section_chars(bytes: &[u8], target: &[u8]) -> Option<u32> {
        let n_sections = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        for i in 0..n_sections {
            let hdr = 20 + i * 40;
            if bytes[hdr..hdr + target.len()] == *target {
                let c = &bytes[hdr + 36..hdr + 40];
                return Some(u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }
        None
    }

    #[test]
    fn coff_pdata_characteristics_include_alignment() {
        // .pdata must carry ALIGN_4BYTES (0x00300000) so the SEH unwinder can
        // locate RUNTIME_FUNCTION entries without extra alignment fixup.
        use crate::isel::{MachineBlock, MachineFunction};
        struct NopCoff;
        impl Emitter for NopCoff {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section { name: ".text".into(), data: vec![0xC3], relocs: vec![], debug_rows: vec![] }
            }
            fn object_format(&self) -> ObjectFormat { ObjectFormat::Coff }
        }
        let mut mf = MachineFunction::new("pdata_test".into());
        mf.blocks.push(MachineBlock { label: "entry".into(), instrs: vec![] });
        mf.frame_size = 8;
        let bytes = emit_object(&mf, &mut NopCoff).to_bytes();
        let chars = coff_section_chars(&bytes, b".pdata")
            .expect(".pdata section not found");
        assert_eq!(chars, 0x40300040, ".pdata must have ALIGN_4BYTES (0x40300040)");
    }

    #[test]
    fn coff_xdata_characteristics_include_alignment() {
        // .xdata must carry ALIGN_8BYTES (0x00400000) so UNWIND_INFO records
        // are correctly located by the x64 exception dispatcher.
        use crate::isel::{MachineBlock, MachineFunction};
        struct NopCoff;
        impl Emitter for NopCoff {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section { name: ".text".into(), data: vec![0xC3], relocs: vec![], debug_rows: vec![] }
            }
            fn object_format(&self) -> ObjectFormat { ObjectFormat::Coff }
        }
        let mut mf = MachineFunction::new("xdata_test".into());
        mf.blocks.push(MachineBlock { label: "entry".into(), instrs: vec![] });
        mf.frame_size = 8;
        let bytes = emit_object(&mf, &mut NopCoff).to_bytes();
        let chars = coff_section_chars(&bytes, b".xdata")
            .expect(".xdata section not found");
        assert_eq!(chars, 0x40400040, ".xdata must have ALIGN_8BYTES (0x40400040)");
    }

    #[test]
    fn coff_function_symbol_type_is_0x0020() {
        // COFF symbol Type must be 0x0020 (DT_FUNCTION) for external function
        // symbols so that debuggers and tools can distinguish code from data.
        let obj = ObjectFile {
            format: ObjectFormat::Coff,
            elf_machine: 0,
            coff_machine: 0x8664,
            sections: vec![Section { name: ".text".into(), data: vec![0xC3], relocs: vec![], debug_rows: vec![] }],
            symbols: vec![Symbol { name: "fn".into(), section: 0, offset: 0, size: 1, global: true, undefined: false }],
        };
        let bytes = obj.to_bytes();
        let symtab_ptr = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        // IMAGE_SYMBOL layout: Name(8) + Value(4) + SectionNumber(2) + Type(2) + ...
        // Type field starts at offset 14 within each 18-byte symbol record.
        let type_field = u16::from_le_bytes([bytes[symtab_ptr + 14], bytes[symtab_ptr + 15]]);
        assert_eq!(type_field, 0x0020, "external function symbols must have Type = 0x0020");
    }

    #[test]
    fn coff_alloc_large_uses_two_unwind_slots() {
        // A frame_size of 256 requires UWOP_ALLOC_LARGE (2 slots) instead of
        // UWOP_ALLOC_SMALL (1 slot), since 256 > 128 (the ALLOC_SMALL limit).
        use crate::isel::{MachineBlock, MachineFunction};
        struct NopCoff;
        impl Emitter for NopCoff {
            fn emit_function(&mut self, _mf: &MachineFunction) -> Section {
                Section { name: ".text".into(), data: vec![0xC3], relocs: vec![], debug_rows: vec![] }
            }
            fn object_format(&self) -> ObjectFormat { ObjectFormat::Coff }
        }
        let mut mf = MachineFunction::new("large_frame".into());
        mf.blocks.push(MachineBlock { label: "entry".into(), instrs: vec![] });
        mf.frame_size = 256;
        let obj = emit_object(&mf, &mut NopCoff);
        let xdata = obj.sections.iter().find(|s| s.name == ".xdata")
            .expect(".xdata section").data.clone();
        // CountOfCodes = xdata[2]. ALLOC_LARGE uses 2 slots, PUSH_NONVOL for RBP = 1 → total = 3.
        assert_eq!(xdata[2], 3, "ALLOC_LARGE(2 slots) + PUSH_NONVOL(1 slot) = 3 total");
        // Header is 4 bytes; first code slot at offset 4.
        // First code must be UWOP_ALLOC_LARGE (op=1, info=0) → byte[5] low nibble = 1.
        assert_eq!(xdata[5] & 0x0f, 1, "first unwind op must be UWOP_ALLOC_LARGE (1)");
        assert_eq!(xdata[5] >> 4, 0, "UWOP_ALLOC_LARGE info must be 0 for sizes ≤ 512 KiB");
        // Slots 1 encodes alloc_size/8 = 256/8 = 32.
        let operand = u16::from_le_bytes([xdata[6], xdata[7]]);
        assert_eq!(operand, 32, "ALLOC_LARGE operand must be alloc_size/8 = 32");
    }
}

// ── Global-variable data section emission ─────────────────────────────────

use llvm_ir::{
    context::ConstId,
    ConstExprOp, ConstantData, Context, FloatKind, Linkage, Module, TypeData,
};
use std::collections::HashMap;

/// Byte size of an IR type under the System V / AAPCS64 ABI model.
///
/// Pointers are always 8 bytes; integer types round up to the nearest byte;
/// aggregates include natural padding.  Void / label / metadata / function
/// types have size 0.
pub fn sizeof_ty(ctx: &Context, ty: llvm_ir::context::TypeId) -> u64 {
    match ctx.get_type(ty) {
        TypeData::Integer(bits) => (*bits as u64).div_ceil(8),
        TypeData::Float(k) => match k {
            FloatKind::Half | FloatKind::BFloat => 2,
            FloatKind::Single => 4,
            FloatKind::Double => 8,
            FloatKind::Fp128 => 16,
            FloatKind::X86Fp80 => 16,
        },
        TypeData::Pointer => 8,
        TypeData::Array { element, len } => {
            let esz = sizeof_ty(ctx, *element);
            esz * len
        }
        TypeData::Vector { element, len, .. } => {
            let esz = sizeof_ty(ctx, *element);
            esz * (*len as u64)
        }
        TypeData::Struct(st) => struct_total_size(ctx, &st.fields.clone(), st.packed),
        TypeData::Void | TypeData::Label | TypeData::Metadata | TypeData::Function(_) => 0,
    }
}

fn align_of_ty(ctx: &Context, ty: llvm_ir::context::TypeId) -> u64 {
    sizeof_ty(ctx, ty).clamp(1, 8)
}

fn struct_total_size(ctx: &Context, fields: &[llvm_ir::context::TypeId], packed: bool) -> u64 {
    let mut offset = 0u64;
    let mut max_align = 1u64;
    for &f in fields {
        let fsz = sizeof_ty(ctx, f);
        let al = if packed { 1 } else { align_of_ty(ctx, f) };
        if al > max_align {
            max_align = al;
        }
        offset = (offset + al - 1) & !(al - 1);
        offset += fsz;
    }
    if !packed && max_align > 1 {
        offset = (offset + max_align - 1) & !(max_align - 1);
    }
    offset
}

fn struct_field_byte_offset(
    ctx: &Context,
    fields: &[llvm_ir::context::TypeId],
    field_idx: usize,
    packed: bool,
) -> u64 {
    let mut offset = 0u64;
    for (i, &f) in fields.iter().enumerate() {
        let al = if packed { 1 } else { align_of_ty(ctx, f) };
        offset = (offset + al - 1) & !(al - 1);
        if i == field_idx {
            return offset;
        }
        offset += sizeof_ty(ctx, f);
    }
    0
}

/// Compute the addend (in bytes) for a GEP constexpr.
///
/// `base_ty` is the aggregate/element type that the first GEP index scales
/// over.  `indices` are the already-extracted integer index values.
fn gep_byte_offset(ctx: &Context, base_ty: llvm_ir::context::TypeId, indices: &[i64]) -> i64 {
    let mut offset: i64 = 0;
    let mut cur_ty = base_ty;
    for (depth, &idx) in indices.iter().enumerate() {
        if depth == 0 {
            offset += idx * sizeof_ty(ctx, cur_ty) as i64;
        } else {
            match ctx.get_type(cur_ty).clone() {
                TypeData::Array { element, .. } => {
                    offset += idx * sizeof_ty(ctx, element) as i64;
                    cur_ty = element;
                }
                TypeData::Struct(st) => {
                    let fi = idx as usize;
                    let flds: Vec<_> = st.fields.clone();
                    offset +=
                        struct_field_byte_offset(ctx, &flds, fi, st.packed) as i64;
                    if fi < flds.len() {
                        cur_ty = flds[fi];
                    }
                }
                TypeData::Vector { element, .. } => {
                    offset += idx * sizeof_ty(ctx, element) as i64;
                    cur_ty = element;
                }
                _ => break,
            }
        }
    }
    offset
}

fn const_as_i64(ctx: &Context, cid: ConstId) -> i64 {
    match ctx.get_const(cid) {
        ConstantData::Int { val, .. } => *val as i64,
        _ => 0,
    }
}

/// Evaluate `cid` into raw bytes appended to `out`, recording any needed
/// relocations in `relocs` with offsets relative to `sec_base` (the
/// section-absolute byte offset at which `out` starts).
fn eval_const_bytes(
    ctx: &Context,
    cid: ConstId,
    sec_base: u64,
    global_sym_map: &HashMap<String, usize>,
    relocs: &mut Vec<Reloc>,
    out: &mut Vec<u8>,
) {
    let cd = ctx.get_const(cid).clone();
    match cd {
        ConstantData::Int { ty, val } => {
            let byte_count = (match ctx.get_type(ty) {
                TypeData::Integer(b) => *b,
                _ => 64,
            } as u64).div_ceil(8);
            for i in 0..byte_count {
                out.push((val >> (i * 8)) as u8);
            }
        }
        ConstantData::IntWide { ty, words } => {
            let bits = match ctx.get_type(ty) {
                TypeData::Integer(b) => *b as u64,
                _ => 64 * words.len() as u64,
            };
            let byte_count = bits.div_ceil(8);
            let raw: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let take = (byte_count as usize).min(raw.len());
            out.extend_from_slice(&raw[..take]);
        }
        ConstantData::Float { ty, bits } => {
            let sz = sizeof_ty(ctx, ty) as usize;
            for i in 0..sz {
                out.push((bits >> (i * 8)) as u8);
            }
        }
        ConstantData::Null(ty) | ConstantData::Undef(ty) | ConstantData::Poison(ty) => {
            let sz = sizeof_ty(ctx, ty) as usize;
            out.extend(std::iter::repeat_n(0u8, sz));
        }
        ConstantData::ZeroInitializer(ty) => {
            let sz = sizeof_ty(ctx, ty) as usize;
            out.extend(std::iter::repeat_n(0u8, sz));
        }
        ConstantData::Array { elements, .. } | ConstantData::Vector { elements, .. } => {
            for elem in elements {
                eval_const_bytes(ctx, elem, sec_base, global_sym_map, relocs, out);
            }
        }
        ConstantData::Struct { ty, fields } => {
            let (field_tys, is_packed) = match ctx.get_type(ty) {
                TypeData::Struct(st) => (st.fields.clone(), st.packed),
                _ => (vec![], false),
            };
            let struct_start = out.len();
            for (i, fld_cid) in fields.iter().enumerate() {
                let fty = field_tys.get(i).copied().unwrap_or(ctx.i8_ty);
                let al = if is_packed { 1 } else { align_of_ty(ctx, fty) as usize };
                let local_pos = out.len() - struct_start;
                let pad = if al > 0 { (al - local_pos % al) % al } else { 0 };
                out.extend(std::iter::repeat_n(0u8, pad));
                eval_const_bytes(ctx, *fld_cid, sec_base, global_sym_map, relocs, out);
            }
            // Trailing struct padding
            let content = out.len() - struct_start;
            let total = struct_total_size(ctx, &field_tys, is_packed) as usize;
            if total > content {
                out.extend(std::iter::repeat_n(0u8, total - content));
            }
        }
        ConstantData::GlobalRef { name, .. } => {
            push_ptr_reloc(sec_base, &name, 0, global_sym_map, relocs, out);
        }
        ConstantData::Expr { op, ty, operands } => {
            match op {
                ConstExprOp::BitCast | ConstExprOp::AddrSpaceCast => {
                    if let Some(&base_cid) = operands.first() {
                        eval_const_bytes(ctx, base_cid, sec_base, global_sym_map, relocs, out);
                    } else {
                        out.extend(std::iter::repeat_n(0u8, sizeof_ty(ctx, ty) as usize));
                    }
                }
                ConstExprOp::GetElementPtr { base_ty, .. } => {
                    let indices: Vec<i64> =
                        operands[1..].iter().map(|&c| const_as_i64(ctx, c)).collect();
                    let addend = gep_byte_offset(ctx, base_ty, &indices);
                    match ctx.get_const(operands[0]).clone() {
                        ConstantData::GlobalRef { name, .. } => {
                            push_ptr_reloc(sec_base, &name, addend, global_sym_map, relocs, out);
                        }
                        _ => {
                            eval_const_bytes(
                                ctx, operands[0], sec_base, global_sym_map, relocs, out,
                            );
                        }
                    }
                }
                ConstExprOp::IntToPtr => {
                    // Absolute address — no relocation
                    if let Some(&val_cid) = operands.first() {
                        match ctx.get_const(val_cid) {
                            ConstantData::Int { val, .. } => {
                                out.extend_from_slice(&val.to_le_bytes());
                            }
                            _ => out.extend_from_slice(&0u64.to_le_bytes()),
                        }
                    } else {
                        out.extend_from_slice(&0u64.to_le_bytes());
                    }
                }
                ConstExprOp::PtrToInt => {
                    let int_bits = match ctx.get_type(ty) {
                        TypeData::Integer(b) => *b,
                        _ => 64,
                    };
                    let byte_count = (int_bits as usize).div_ceil(8);
                    if let Some(&base_cid) = operands.first() {
                        if let ConstantData::GlobalRef { name, .. } = ctx.get_const(base_cid).clone() {
                            push_ptr_reloc(sec_base, &name, 0, global_sym_map, relocs, out);
                            // push_ptr_reloc always emits 8 bytes; truncate if int is narrower
                            if byte_count < 8 {
                                let extra = 8 - byte_count;
                                let len = out.len();
                                out.truncate(len - extra);
                            }
                            return;
                        }
                    }
                    out.extend(std::iter::repeat_n(0u8, byte_count));
                }
                ConstExprOp::Trunc | ConstExprOp::ZExt | ConstExprOp::SExt => {
                    let dst_bits = match ctx.get_type(ty) {
                        TypeData::Integer(b) => *b as usize,
                        _ => 64,
                    };
                    let dst_bytes = dst_bits.div_ceil(8);
                    if let Some(&src_cid) = operands.first() {
                        let mut src_buf = Vec::new();
                        eval_const_bytes(
                            ctx, src_cid, sec_base, global_sym_map, relocs, &mut src_buf,
                        );
                        let sign_extend = op == ConstExprOp::SExt;
                        let fill = if sign_extend {
                            src_buf.last().map(|&b| if b & 0x80 != 0 { 0xFF } else { 0 }).unwrap_or(0)
                        } else {
                            0
                        };
                        src_buf.resize(dst_bytes, fill);
                        out.extend_from_slice(&src_buf[..dst_bytes.min(src_buf.len())]);
                    } else {
                        out.extend(std::iter::repeat_n(0u8, dst_bytes));
                    }
                }
            }
        }
    }
}

fn push_ptr_reloc(
    sec_base: u64,
    name: &str,
    addend: i64,
    global_sym_map: &HashMap<String, usize>,
    relocs: &mut Vec<Reloc>,
    out: &mut Vec<u8>,
) {
    let reloc_offset = sec_base + out.len() as u64;
    if let Some(&sym_idx) = global_sym_map.get(name) {
        relocs.push(Reloc {
            offset: reloc_offset,
            symbol: sym_idx,
            kind: RelocKind::Abs64,
            addend,
            external_name: None,
        });
    } else {
        relocs.push(Reloc {
            offset: reloc_offset,
            symbol: 0,
            kind: RelocKind::Abs64,
            addend,
            external_name: Some(name.to_string()),
        });
    }
    out.extend_from_slice(&0u64.to_le_bytes());
}

/// Emit all global variables in `module` as data sections with relocations.
///
/// Returns `(sections, symbols)` ready to be merged into an [`ObjectFile`].
/// Symbol indices in the returned relocs are relative to the returned
/// `symbols` vec; callers must adjust them if merging into a larger symbol
/// table.
///
/// Returns `None` when the module contains no global variables.
pub fn emit_globals(
    ctx: &Context,
    module: &Module,
    format: ObjectFormat,
) -> Option<(Vec<Section>, Vec<Symbol>)> {
    if module.globals.is_empty() {
        return None;
    }

    // Map global name → symbol index (within our local symbols vec).
    let mut global_sym_map: HashMap<String, usize> = HashMap::new();
    for (i, gv) in module.globals.iter().enumerate() {
        global_sym_map.insert(gv.name.clone(), i);
    }

    let rodata_name = match format {
        ObjectFormat::MachO => "__const",
        _ => ".rodata",
    };
    let data_name = match format {
        ObjectFormat::MachO => "__data",
        _ => ".data",
    };

    // Build one section for read-only globals, one for mutable globals.
    let has_readonly = module.globals.iter().any(|gv| gv.is_constant);
    let has_rw = module.globals.iter().any(|gv| !gv.is_constant);

    let mut sections: Vec<Section> = Vec::new();
    if has_readonly {
        sections.push(Section {
            name: rodata_name.to_string(),
            data: Vec::new(),
            relocs: Vec::new(),
            debug_rows: Vec::new(),
        });
    }
    if has_rw {
        sections.push(Section {
            name: data_name.to_string(),
            data: Vec::new(),
            relocs: Vec::new(),
            debug_rows: Vec::new(),
        });
    }

    let mut symbols: Vec<Symbol> = Vec::new();

    for gv in &module.globals {
        let sec_idx = if gv.is_constant {
            sections.iter().position(|s| s.name == rodata_name).unwrap()
        } else {
            sections.iter().position(|s| s.name == data_name).unwrap()
        };
        let sec = &mut sections[sec_idx];

        // Natural alignment: min of sizeof(ty), 16.
        let sz = sizeof_ty(ctx, gv.ty) as usize;
        let al = (sz as u64).clamp(1, 16) as usize;
        let pad = if al > 0 { (al - sec.data.len() % al) % al } else { 0 };
        sec.data.extend(std::iter::repeat_n(0u8, pad));

        let global_offset = sec.data.len() as u64;

        if let Some(init_cid) = gv.initializer {
            // sec_base = 0: `sec.data` is the full section buffer, so
            // `out.len()` directly gives the section-absolute byte position.
            eval_const_bytes(
                ctx,
                init_cid,
                0,
                &global_sym_map,
                &mut sec.relocs,
                &mut sec.data,
            );
        } else {
            // Declaration only (extern) — emit zero bytes as placeholder.
            sec.data.extend(std::iter::repeat_n(0u8, sz));
        }

        let emitted_size = sec.data.len() as u64 - global_offset;
        let is_global_sym = !matches!(gv.linkage, Linkage::Private | Linkage::Internal);
        symbols.push(Symbol {
            name: gv.name.clone(),
            section: sec_idx,
            offset: global_offset,
            size: emitted_size,
            global: is_global_sym,
            undefined: false,
        });
    }

    // Resolve external_name relocs within our own global_sym_map.
    // (Relocs referencing *other* globals outside this module are left as
    // external_name for the caller to resolve after merging symbol tables.)
    let sym_count = symbols.len();
    for sec in &mut sections {
        for reloc in &mut sec.relocs {
            if reloc.external_name.is_none() && reloc.symbol >= sym_count {
                // Symbol index is already correct (relative to our vec).
            }
        }
    }

    Some((sections, symbols))
}
