//! LTO helpers: embed recoverable IR payloads in object files and run a
//! link-time cross-module optimization pipeline.

use llvm_bitcode::{read_bitcode, write_bitcode};
use llvm_codegen::{ObjectFile, ObjectFormat, Section};
use llvm_ir::{Context, Module};
use llvm_transforms::pipeline::{build_pipeline, OptLevel};

const ELF_LTO_SECTION: &str = ".llvm_ir";
const MACHO_LTO_SECTION: &str = "__LLVM,__bitcode";
const COFF_LTO_SECTION: &str = ".lto$ir";

fn lto_section_name(fmt: ObjectFormat) -> &'static str {
    match fmt {
        ObjectFormat::Elf => ELF_LTO_SECTION,
        ObjectFormat::MachO => MACHO_LTO_SECTION,
        ObjectFormat::Coff => COFF_LTO_SECTION,
    }
}

/// Serialize `module` as LRIR bitcode and embed it into `obj`.
pub fn embed_lto_payload(obj: &mut ObjectFile, ctx: &Context, module: &Module) {
    let section_name = lto_section_name(obj.format).to_string();
    let payload = write_bitcode(ctx, module);
    if let Some(sec) = obj.sections.iter_mut().find(|s| s.name == section_name) {
        sec.data = payload;
        sec.relocs.clear();
        sec.debug_rows.clear();
    } else {
        obj.sections.push(Section {
            name: section_name,
            data: payload,
            relocs: Vec::new(),
            debug_rows: Vec::new(),
        });
    }
}

/// Recover embedded LRIR payload bytes from an object file if present.
pub fn extract_lto_payload(obj: &ObjectFile) -> Option<&[u8]> {
    let section_name = lto_section_name(obj.format);
    obj.sections
        .iter()
        .find(|s| s.name == section_name)
        .map(|s| s.data.as_slice())
}

/// Merge all embedded IR payloads from `objects`, then run LTO passes.
pub fn run_lto_from_objects(objects: &[ObjectFile], level: OptLevel) -> Result<(Context, Module), String> {
    let mut modules = Vec::new();
    for obj in objects {
        if let Some(bytes) = extract_lto_payload(obj) {
            let decoded = read_bitcode(bytes).map_err(|e| format!("failed to decode LTO payload: {e:?}"))?;
            modules.push(decoded);
        }
    }
    if modules.is_empty() {
        return Err("no embedded LTO payloads found".to_string());
    }

    let (mut merged_ctx, mut merged_mod) = modules.remove(0);
    for (ctx, m) in modules {
        merge_module_into(&mut merged_ctx, &mut merged_mod, ctx, m)?;
    }

    let mut pm = build_pipeline(level);
    pm.run_until_fixed_point(&mut merged_ctx, &mut merged_mod, 8);
    Ok((merged_ctx, merged_mod))
}

fn merge_module_into(dst_ctx: &mut Context, dst: &mut Module, src_ctx: Context, mut src: Module) -> Result<(), String> {
    // Merge globals (naive name-based policy).
    for gv in src.globals.drain(..) {
        if dst.get_global_id(&gv.name).is_none() {
            dst.add_global(gv);
        }
    }

    // Merge functions. If dst has a declaration and src has a definition, replace it.
    for f in src.functions.drain(..) {
        match dst.get_function_id(&f.name) {
            None => {
                dst.add_function(f);
            }
            Some(fid) => {
                let existing_is_decl = dst.function(fid).is_declaration;
                if existing_is_decl && !f.is_declaration {
                    dst.functions[fid.0 as usize] = f;
                } else if !existing_is_decl && !f.is_declaration {
                    return Err(format!("duplicate function definition during LTO merge: {}", dst.function(fid).name));
                }
            }
        }
    }

    // Merge lightweight metadata/maps.
    for (k, v) in src.debug_locations {
        dst.debug_locations.entry(k).or_insert(v);
    }
    for (k, v) in src.metadata_nodes {
        dst.metadata_nodes.entry(k).or_insert(v);
    }
    for (k, v) in src.named_metadata {
        if !dst.named_metadata.iter().any(|(dk, _)| *dk == k) {
            dst.named_metadata.push((k, v));
        }
    }
    for (name, ty) in src.named_types {
        if !dst.named_types.iter().any(|(n, _)| *n == name) {
            dst.named_types.push((name, ty));
        }
    }

    // Keep source hints when destination is missing them.
    if dst.source_filename.is_none() {
        dst.source_filename = src.source_filename.take();
    }
    if dst.target_triple.is_none() {
        dst.target_triple = src.target_triple.take();
    }
    if dst.data_layout.is_none() {
        dst.data_layout = src.data_layout.take();
    }

    // Current IR contexts are globally interned per kind, so no explicit remap is needed here.
    let _ = src_ctx;
    let _ = dst_ctx;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use llvm_codegen::{ObjectFormat, Symbol};
    use llvm_ir_parser::parser::parse;

    fn empty_obj(fmt: ObjectFormat) -> ObjectFile {
        ObjectFile {
            format: fmt,
            elf_machine: 62,
            coff_machine: 0x8664,
            sections: vec![Section {
                name: match fmt {
                    ObjectFormat::Elf => ".text".into(),
                    ObjectFormat::MachO => "__text".into(),
                    ObjectFormat::Coff => ".text".into(),
                },
                data: vec![0xC3],
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
    fn embeds_and_extracts_lto_payload() {
        let (ctx, m) = parse("define i32 @main() { entry: ret i32 7 }").expect("parse");
        let mut obj = empty_obj(ObjectFormat::Elf);
        embed_lto_payload(&mut obj, &ctx, &m);
        let bytes = extract_lto_payload(&obj).expect("payload");
        let (_ctx2, m2) = read_bitcode(bytes).expect("decode payload");
        assert!(m2.get_function_id("main").is_some());
    }

    #[test]
    fn lto_merge_upgrades_decl_to_definition() {
        let (ctx_a, m_a) = parse(
            "define i32 @main() { entry: %x = call i32 @callee() ret i32 %x } declare i32 @callee()",
        )
        .expect("parse a");
        let (ctx_b, m_b) = parse("define i32 @callee() { entry: ret i32 42 }").expect("parse b");

        let mut o1 = empty_obj(ObjectFormat::Elf);
        let mut o2 = empty_obj(ObjectFormat::Elf);
        embed_lto_payload(&mut o1, &ctx_a, &m_a);
        embed_lto_payload(&mut o2, &ctx_b, &m_b);

        let (_ctx_m, merged) = run_lto_from_objects(&[o1, o2], OptLevel::O2).expect("run lto");
        let (_, callee) = merged.get_function("callee").expect("callee in merged module");
        assert!(!callee.is_declaration, "callee definition should be available after merge");
    }
}
