//! End-to-end IR-to-object compilation pipeline.
//!
//! This module exposes [`compile_ir_to_object`] which drives the full pipeline
//! from LLVM IR source text to a raw object file (ELF / Mach-O / COFF).
//! Multiple non-declaration functions in the input module are all compiled and
//! merged into a single `.text` section.  Global variables are emitted into
//! `.rodata` / `.data` sections with proper relocation entries.

use llvm_codegen::{
    emit_globals, emit_object,
    isel::IselBackend,
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFile, ObjectFormat, Reloc, Section, Symbol,
};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM, MOVSD_LOAD_MR, MOVSD_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{build_pipeline, OptLevel};

/// Compile LLVM IR text into a native object file.
///
/// All non-declaration functions in the module are compiled and merged into a
/// single `.text` section.  Symbols and relocations from every function are
/// accumulated into the same object so the result is directly linkable.
///
/// # Errors
/// Returns a human-readable error string on parse failure, or if the module
/// contains no non-declaration functions.
pub fn compile_ir_to_object(
    src: &str,
    opt_level: OptLevel,
    fmt: ObjectFormat,
) -> Result<Vec<u8>, String> {
    let (mut ctx, mut module) =
        parse(src).map_err(|e| format!("parse error: {e}"))?;

    let mut pm = build_pipeline(opt_level);
    pm.run_until_fixed_point(&mut ctx, &mut module, 8);

    let non_decls: Vec<usize> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.is_declaration)
        .map(|(i, _)| i)
        .collect();

    if non_decls.is_empty() {
        return Err("module has no non-declaration functions to compile".to_string());
    }

    let mut accumulated_text: Vec<u8> = Vec::new();
    let mut unified_symbols: Vec<Symbol> = Vec::new();
    let mut accumulated_relocs: Vec<Reloc> = Vec::new();
    let elf_machine = match fmt {
        ObjectFormat::Elf => 62_u16, // EM_X86_64
        _ => 0,
    };
    let coff_machine = 0x8664_u16;

    for func_idx in non_decls {
        let func = &module.functions[func_idx];
        let mut backend = X86Backend::default();
        let mut mf = backend.lower_function(&ctx, &module, func);
        let intervals = compute_live_intervals(&mf);
        let strategy = regalloc_strategy_for(opt_level);
        let mut result = allocate_registers(
            &intervals,
            &mf.allocatable_pregs,
            &mf.allocatable_fp_pregs,
            strategy,
        );
        insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM, MOVSD_LOAD_MR, MOVSD_STORE_RM);
        apply_allocation(&mut mf, &result);

        let mut emitter = X86Emitter::new(fmt);
        let func_obj = emit_object(&mf, &mut emitter);

        let text_offset = accumulated_text.len() as u64;

        // Find the text section from this function's ObjectFile.
        // Name is ".text" on ELF/COFF and "__text" on Mach-O.
        let (text_section, other_sections): (Vec<_>, Vec<_>) = func_obj
            .sections
            .into_iter()
            .partition(|s| s.name == ".text" || s.name == "__text");
        let text = text_section
            .into_iter()
            .next()
            .ok_or_else(|| format!("no text section for function {}", func.name))?;

        // Merge symbols: build a remap table from old index → unified index.
        // Defined (non-undefined) symbols get their offset adjusted by text_offset.
        let old_sym_count = func_obj.symbols.len();
        let mut sym_remap: Vec<usize> = Vec::with_capacity(old_sym_count);
        for mut sym in func_obj.symbols {
            // Try to find an existing symbol with the same name.
            let pos = unified_symbols.iter().position(|s| s.name == sym.name);
            let new_idx = if let Some(p) = pos {
                p
            } else {
                let new_idx = unified_symbols.len();
                if !sym.undefined {
                    sym.offset += text_offset;
                }
                unified_symbols.push(sym);
                new_idx
            };
            sym_remap.push(new_idx);
        }

        // Merge any undefined symbols that were added to `other_sections` (e.g.
        // external_name relocs resolved during emit_object). These symbols ended
        // up in `func_obj.symbols` which we already consumed above, so nothing
        // extra here — they were handled in the loop above.
        let _ = other_sections; // other sections (unwind, debug) intentionally dropped for merged output

        // Merge relocations, adjusting offsets and remapping symbol indices.
        for mut reloc in text.relocs {
            reloc.offset += text_offset;
            if reloc.symbol < sym_remap.len() {
                reloc.symbol = sym_remap[reloc.symbol];
            }
            accumulated_relocs.push(reloc);
        }

        accumulated_text.extend(text.data);
    }

    let text_section_name = match fmt {
        ObjectFormat::MachO => "__text",
        _ => ".text",
    };
    let mut merged_sections = vec![Section {
        name: text_section_name.to_string(),
        data: accumulated_text,
        relocs: accumulated_relocs,
        debug_rows: vec![],
    }];

    // Emit global variables (.rodata / .data sections with relocations).
    if let Some((global_secs, global_syms)) = emit_globals(&ctx, &module, fmt) {
        // Remap symbol indices inside global_secs.relocs: they currently point
        // into global_syms (0-based); after merging, they point into
        // unified_symbols starting at `sym_base`.
        let sym_base = unified_symbols.len();
        let sec_base = merged_sections.len();

        for mut sec in global_secs {
            for reloc in &mut sec.relocs {
                if reloc.external_name.is_none() {
                    reloc.symbol += sym_base;
                }
            }
            merged_sections.push(sec);
        }
        for mut sym in global_syms {
            // Adjust section index to be relative to the merged sections vec.
            sym.section += sec_base;
            unified_symbols.push(sym);
        }

        // Resolve external_name relocs in the newly-added sections.
        for sec in merged_sections.iter_mut().skip(sec_base) {
            for reloc in &mut sec.relocs {
                if let Some(ref ext) = reloc.external_name.clone() {
                    let idx = unified_symbols
                        .iter()
                        .position(|s| &s.name == ext)
                        .unwrap_or_else(|| {
                            let i = unified_symbols.len();
                            unified_symbols.push(Symbol {
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
                    reloc.external_name = None;
                }
            }
        }
    }

    let merged = ObjectFile {
        format: fmt,
        elf_machine,
        coff_machine,
        sections: merged_sections,
        symbols: unified_symbols,
    };

    Ok(merged.to_bytes())
}

/// Return the register allocation strategy appropriate for `level`.
///
/// O0/O1 use linear scan (fast compile, acceptable spill count).
/// O2/O3 use graph coloring (Chaitin-Briggs), which produces fewer spills at
/// the cost of slightly longer compile time.
pub fn regalloc_strategy_for(level: OptLevel) -> RegAllocStrategy {
    match level {
        OptLevel::O0 | OptLevel::O1 => RegAllocStrategy::LinearScan,
        OptLevel::O2 | OptLevel::O3 => RegAllocStrategy::GraphColor,
    }
}

/// Detect the host's native object format.
pub fn host_object_format() -> ObjectFormat {
    if cfg!(target_os = "linux") {
        ObjectFormat::Elf
    } else if cfg!(target_os = "windows") {
        ObjectFormat::Coff
    } else {
        ObjectFormat::MachO
    }
}
