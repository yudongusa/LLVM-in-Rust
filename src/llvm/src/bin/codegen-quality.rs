//! Codegen quality checker: measures instruction counts and compares to baselines.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use llvm_codegen::{
    emit_object,
    isel::{IselBackend, MOpcode, MachineFunction},
    regalloc::{
        allocate_registers, apply_allocation, compute_live_intervals, insert_spill_reloads,
        RegAllocStrategy,
    },
    ObjectFormat,
};
use llvm_ir_parser::parser::parse;
use llvm_target_x86::{
    instructions::{MOV_LOAD_MR, MOV_STORE_RM},
    X86Backend, X86Emitter,
};
use llvm_transforms::{mem2reg::Mem2Reg, pass::FunctionPass};

#[derive(Debug)]
struct KernelMetrics {
    name: String,
    object_path: PathBuf,
    machine_instructions: usize,
    spill_reload_instructions: usize,
    text_bytes: usize,
    prologue_epilogue_bytes: usize,
    frame_bytes: u32,
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn prologue_epilogue_bytes(mf: &MachineFunction) -> usize {
    let n_callee = mf.used_callee_saved.len();
    let needs_frame = mf.frame_size > 0 || n_callee > 0;
    if !needs_frame {
        return 0;
    }

    let raw = mf.frame_size as usize;
    let sub_rsp = if n_callee % 2 == 1 {
        let r = raw % 16;
        match r.cmp(&8) {
            std::cmp::Ordering::Equal => raw,
            std::cmp::Ordering::Less => raw + (8 - r),
            std::cmp::Ordering::Greater => raw + (24 - r),
        }
    } else {
        (raw + 15) & !15
    };

    let push_rbp = 1;
    let mov_rbp_rsp = 3;
    let callee_pushes: usize = mf
        .used_callee_saved
        .iter()
        .map(|pr| if pr.0 >= 8 { 2 } else { 1 })
        .sum();
    let sub = if sub_rsp == 0 {
        0
    } else if sub_rsp <= 127 {
        4
    } else {
        7
    };
    let add = sub;
    let callee_pops = callee_pushes;
    let pop_rbp = 1;

    push_rbp + mov_rbp_rsp + callee_pushes + sub + add + callee_pops + pop_rbp
}

fn count_opcode(mf: &MachineFunction, opcode: MOpcode) -> usize {
    mf.blocks
        .iter()
        .map(|block| {
            block
                .instrs
                .iter()
                .filter(|instr| instr.opcode == opcode)
                .count()
        })
        .sum()
}

fn compile_kernel(path: &Path, emit_dir: &Path) -> Result<KernelMetrics, String> {
    let source = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let (mut ctx, mut module) = parse(&source).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let func_idx = module
        .functions
        .iter()
        .position(|f| f.name == "main" && !f.is_declaration)
        .ok_or_else(|| format!("{} has no @main definition", path.display()))?;

    // Promote alloca/load/store to SSA before lowering so that code-quality
    // metrics reflect post-optimization code rather than alloca-pattern lowering.
    Mem2Reg.run_on_function(&mut ctx, &mut module.functions[func_idx]);

    let func = &module.functions[func_idx];
    let mut backend = X86Backend::default();
    let mut mf = backend.lower_function(&ctx, &module, func);
    let intervals = compute_live_intervals(&mf);
    let mut result = allocate_registers(
        &intervals,
        &mf.allocatable_pregs,
        &mf.allocatable_fp_pregs,
        RegAllocStrategy::LinearScan,
    );
    insert_spill_reloads(&mut mf, &mut result, MOV_LOAD_MR, MOV_STORE_RM, MOV_LOAD_MR, MOV_STORE_RM);
    apply_allocation(&mut mf, &result);

    let machine_instructions: usize = mf.blocks.iter().map(|b| b.instrs.len()).sum();
    let spill_reload_instructions =
        count_opcode(&mf, MOV_LOAD_MR) + count_opcode(&mf, MOV_STORE_RM);
    let prologue_epilogue_bytes = prologue_epilogue_bytes(&mf);
    let frame_bytes = mf.frame_size;

    let mut emitter = X86Emitter::new(ObjectFormat::Elf);
    let object = emit_object(&mf, &mut emitter);
    let text_bytes = object
        .sections
        .iter()
        .find(|section| section.name == ".text")
        .map(|section| section.data.len())
        .unwrap_or(0);

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid kernel filename {}", path.display()))?;
    fs::create_dir_all(emit_dir).map_err(|e| format!("create {}: {e}", emit_dir.display()))?;
    let object_path = emit_dir.join(format!("{stem}.o"));
    fs::write(&object_path, object.to_bytes())
        .map_err(|e| format!("write {}: {e}", object_path.display()))?;

    Ok(KernelMetrics {
        name: stem.to_owned(),
        object_path,
        machine_instructions,
        spill_reload_instructions,
        text_bytes,
        prologue_epilogue_bytes,
        frame_bytes,
    })
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let bench_dir = args.next().unwrap_or_else(|| "bench/codegen".to_owned());
    let emit_dir = args
        .next()
        .unwrap_or_else(|| "target/codegen-quality/ours".to_owned());

    let mut paths: Vec<PathBuf> = match fs::read_dir(&bench_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("ll"))
            .collect(),
        Err(e) => {
            eprintln!("failed to read {bench_dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    paths.sort();

    let mut metrics = Vec::new();
    for path in &paths {
        match compile_kernel(path, Path::new(&emit_dir)) {
            Ok(m) => metrics.push(m),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }

    println!("{{");
    println!("  \"target\": \"x86_64-unknown-linux-gnu\",");
    println!("  \"object_format\": \"elf\",");
    println!("  \"kernels\": {{");
    for (idx, m) in metrics.iter().enumerate() {
        let comma = if idx + 1 == metrics.len() { "" } else { "," };
        println!("    \"{}\": {{", json_escape(&m.name));
        println!(
            "      \"object_path\": \"{}\",",
            json_escape(&m.object_path.display().to_string())
        );
        println!(
            "      \"machine_instructions\": {},",
            m.machine_instructions
        );
        println!(
            "      \"spill_reload_instructions\": {},",
            m.spill_reload_instructions
        );
        println!("      \"text_bytes\": {},", m.text_bytes);
        println!(
            "      \"prologue_epilogue_bytes\": {},",
            m.prologue_epilogue_bytes
        );
        println!("      \"frame_bytes\": {}", m.frame_bytes);
        println!("    }}{comma}");
    }
    println!("  }}");
    println!("}}");

    ExitCode::SUCCESS
}
