//! Integrated assembler (MC) stage.
//!
//! This module makes the direct machine-code emission path explicit:
//! machine IR -> target encoder (`Emitter`) -> object sections/symbols ->
//! object bytes. No textual assembly round-trip is required.

use crate::emit::{emit_object, Emitter, ObjectFile};
use crate::isel::MachineFunction;

/// Summary metrics for one assembly invocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct McAssemblyReport {
    pub section_count: usize,
    pub symbol_count: usize,
    pub reloc_count: usize,
    pub bytes: usize,
}

/// Result of integrated assembly.
#[derive(Clone, Debug)]
pub struct AssembledObject {
    pub object: ObjectFile,
    pub bytes: Vec<u8>,
    pub report: McAssemblyReport,
}

/// Pluggable assembler interface for machine-code/object emission.
pub trait McAssembler {
    fn assemble_object(&mut self, mf: &MachineFunction, emitter: &mut dyn Emitter) -> ObjectFile;

    fn assemble(&mut self, mf: &MachineFunction, emitter: &mut dyn Emitter) -> AssembledObject {
        let object = self.assemble_object(mf, emitter);
        let bytes = object.to_bytes();
        let reloc_count = object.sections.iter().map(|s| s.relocs.len()).sum();
        let report = McAssemblyReport {
            section_count: object.sections.len(),
            symbol_count: object.symbols.len(),
            reloc_count,
            bytes: bytes.len(),
        };
        AssembledObject {
            object,
            bytes,
            report,
        }
    }
}

/// Default integrated assembler: directly lowers machine IR to object bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct IntegratedAssembler;

impl McAssembler for IntegratedAssembler {
    fn assemble_object(&mut self, mf: &MachineFunction, emitter: &mut dyn Emitter) -> ObjectFile {
        emit_object(mf, emitter)
    }
}

/// Convenience wrapper that assembles machine IR directly into an object.
pub fn assemble_object(mf: &MachineFunction, emitter: &mut dyn Emitter) -> ObjectFile {
    IntegratedAssembler.assemble_object(mf, emitter)
}

/// Convenience wrapper that assembles machine IR directly into raw bytes.
pub fn assemble_bytes(mf: &MachineFunction, emitter: &mut dyn Emitter) -> Vec<u8> {
    IntegratedAssembler.assemble(mf, emitter).bytes
}

/// Convenience wrapper returning object, bytes, and assembly report.
pub fn assemble_with_report(mf: &MachineFunction, emitter: &mut dyn Emitter) -> AssembledObject {
    IntegratedAssembler.assemble(mf, emitter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit::{ObjectFormat, Section, Symbol};
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

    #[test]
    fn integrated_assembler_produces_nonempty_bytes() {
        let mut mf = MachineFunction::new("f".into());
        mf.blocks.push(MachineBlock {
            label: "entry".into(),
            instrs: vec![],
        });
        let mut emitter = NopEmitter;
        let assembled = assemble_with_report(&mf, &mut emitter);
        assert!(!assembled.bytes.is_empty());
        assert_eq!(
            assembled.report.section_count,
            assembled.object.sections.len()
        );
    }

    #[test]
    fn report_counts_sections_symbols_and_relocs() {
        let object = ObjectFile {
            format: ObjectFormat::Elf,
            elf_machine: 62,
            coff_machine: 0x8664,
            sections: vec![Section {
                name: ".text".into(),
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
        };
        let bytes = object.to_bytes();
        let report = McAssemblyReport {
            section_count: object.sections.len(),
            symbol_count: object.symbols.len(),
            reloc_count: object.sections.iter().map(|s| s.relocs.len()).sum(),
            bytes: bytes.len(),
        };
        assert_eq!(report.section_count, 1);
        assert_eq!(report.symbol_count, 1);
        assert_eq!(report.reloc_count, 0);
        assert!(report.bytes > 0);
    }
}
