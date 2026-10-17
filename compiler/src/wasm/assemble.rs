//! Assemble an in-process batch into one self-contained module.
//!
//! The type layout matches wasm-jit-runner's `build_module`
//! (`wasm-jit-runner/src/addfuncs.rs`): extern (helper) types `0..nextern`,
//! then one type per blob, in order, with no deduplication. Helper `i` is
//! function import `i` and blob `i` is function `nextern + i`, so the blob
//! bodies' `call` and `call_indirect` immediates keep their meaning. Names
//! of those imports and exports are a [`ModuleWrap`]: the Yurt JIT mount
//! ABI is one wrap, and the runner-shaped probe in the in-process tests is
//! another. The runner still builds its own module on the host; this crate
//! does not share that code.

use waffle::{SignatureData, Type};
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, FunctionSection, ImportSection, MemoryType,
    Module, RefType, TableType, TypeSection, ValType,
};

use super::inprocess::InprocOut;

/// The declared maximum of a shared `env.memory` import. Import matching
/// needs it at or above the owner's own maximum; the wasm32 limit is above
/// every owner's.
const MEMORY_MAX_PAGES: u64 = 65536;

/// Import and export names wrapped around an already-carved batch.
/// `func_imports[i]` is helper `i` (type index `i`); `export_names[i]` is
/// blob `i` (type index and function index `nextern + i`).
pub(crate) struct ModuleWrap {
    pub func_imports: Vec<(String, String)>,
    pub memory_module: String,
    pub memory_name: String,
    pub memory: MemoryType,
    pub table_module: String,
    pub table_name: String,
    pub table: TableType,
    pub export_names: Vec<String>,
}

fn enc_type(t: Type) -> Result<ValType, String> {
    match t {
        Type::I32 => Ok(ValType::I32),
        Type::I64 => Ok(ValType::I64),
        Type::F32 => Ok(ValType::F32),
        Type::F64 => Ok(ValType::F64),
        other => Err(format!("unsupported helper type {other:?}")),
    }
}

fn enc_valtype(t: wasmparser::ValType) -> Result<ValType, String> {
    match t {
        wasmparser::ValType::I32 => Ok(ValType::I32),
        wasmparser::ValType::I64 => Ok(ValType::I64),
        wasmparser::ValType::F32 => Ok(ValType::F32),
        wasmparser::ValType::F64 => Ok(ValType::F64),
        other => Err(format!("unsupported blob valtype {other:?}")),
    }
}

/// Split a blob (`0x60 functype ++ code-entry body`) into its type and body.
fn parse_blob(blob: &[u8]) -> Result<(Vec<ValType>, Vec<ValType>, &[u8]), String> {
    let mut reader = wasmparser::BinaryReader::new(blob, 0);
    if reader.read_u8().map_err(|e| format!("blob: {e}"))? != 0x60 {
        return Err("blob does not start with a functype".to_string());
    }
    let fty: wasmparser::FuncType = reader.read().map_err(|e| format!("blob functype: {e}"))?;
    let params = fty
        .params()
        .iter()
        .map(|&t| enc_valtype(t))
        .collect::<Result<_, _>>()?;
    let results = fty
        .results()
        .iter()
        .map(|&t| enc_valtype(t))
        .collect::<Result<_, _>>()?;
    let body = &blob[reader.current_position()..];
    if body.is_empty() {
        return Err("blob has an empty body".to_string());
    }
    Ok((params, results, body))
}

/// Blobs plus helper signatures, wrapped with `wrap`.
pub(crate) fn assemble_blobs(
    blobs: &[Vec<u8>],
    extern_sigs: &[SignatureData],
    wrap: &ModuleWrap,
) -> Result<Vec<u8>, String> {
    if wrap.func_imports.len() != extern_sigs.len() {
        return Err(format!(
            "function imports ({}) != helper signatures ({})",
            wrap.func_imports.len(),
            extern_sigs.len()
        ));
    }
    if wrap.export_names.len() != blobs.len() {
        return Err(format!(
            "export names ({}) != blobs ({})",
            wrap.export_names.len(),
            blobs.len()
        ));
    }
    let n_extern = u32::try_from(extern_sigs.len()).unwrap();
    let parsed = blobs
        .iter()
        .map(|b| parse_blob(b))
        .collect::<Result<Vec<_>, _>>()?;

    let mut types = TypeSection::new();
    for sig in extern_sigs {
        let params = sig
            .params
            .iter()
            .map(|&t| enc_type(t))
            .collect::<Result<Vec<_>, _>>()?;
        let results = sig
            .returns
            .iter()
            .map(|&t| enc_type(t))
            .collect::<Result<Vec<_>, _>>()?;
        types.ty().function(params, results);
    }
    for (params, results, _) in &parsed {
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
    }

    let mut imports = ImportSection::new();
    for (i, (module, name)) in wrap.func_imports.iter().enumerate() {
        imports.import(module, name, EntityType::Function(i as u32));
    }
    imports.import(
        &wrap.memory_module,
        &wrap.memory_name,
        EntityType::Memory(wrap.memory),
    );
    imports.import(
        &wrap.table_module,
        &wrap.table_name,
        EntityType::Table(wrap.table),
    );

    let mut funcs = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();
    for (i, (_, _, body)) in parsed.iter().enumerate() {
        let i = u32::try_from(i).unwrap();
        funcs.function(n_extern + i);
        exports.export(
            &wrap.export_names[i as usize],
            ExportKind::Func,
            n_extern + i,
        );
        // `raw` length-prefixes `locals ++ expr` into a full code entry.
        code.raw(body);
    }

    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&funcs);
    module.section(&exports);
    module.section(&code);
    Ok(module.finish())
}

fn yurt_wrap(out: &InprocOut) -> ModuleWrap {
    ModuleWrap {
        func_imports: out
            .extern_table_indices
            .iter()
            .map(|slot| ("yurt.table".to_string(), slot.to_string()))
            .collect(),
        memory_module: "env".to_string(),
        memory_name: "memory".to_string(),
        memory: MemoryType {
            minimum: 0,
            maximum: Some(MEMORY_MAX_PAGES),
            memory64: false,
            shared: true,
            page_size_log2: None,
        },
        table_module: "env".to_string(),
        table_name: "__indirect_function_table".to_string(),
        table: TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: 0,
            maximum: None,
            shared: false,
        },
        export_names: (0..out.blobs.len())
            .map(|i| format!("__jit_entry_{i}"))
            .collect(),
    }
}

/// The batch as a Yurt JIT mount module (see the module docs).
pub fn assemble_module(out: &InprocOut) -> Result<Vec<u8>, String> {
    assemble_blobs(&out.blobs, &out.extern_sigs, &yurt_wrap(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmparser::{Operator, Parser, Payload, TypeRef};

    #[test]
    fn installed_batch_releases_code_but_keeps_environment_and_script_map() {
        let mut out = super::super::inprocess::toy_inproc_out();
        out.module = Some(assemble_module(&out).unwrap());
        let desc = out.env_desc.clone();
        let ptr = out.env_desc.as_ptr();
        let scripts = out.scripts.clone();
        crate::night_inproc_release_code(&mut out);
        assert_eq!(out.blobs.capacity(), 0);
        assert!(out.module.is_none());
        assert_eq!(out.extern_sigs.capacity(), 0);
        assert_eq!(out.extern_table_indices.capacity(), 0);
        assert_eq!(out.env_desc, desc);
        assert_eq!(out.env_desc.as_ptr(), ptr);
        assert_eq!(out.scripts, scripts);
    }

    #[test]
    fn yurt_module_instantiates_and_calls_through_owner_table() {
        use wasmtime::{
            Engine, Func, Linker, MemoryType as HostMemoryType, Module as HostModule, Ref,
            RefType as HostRefType, SharedMemory, Store, Table, TableType as HostTableType,
        };
        // Blob 0 calls yurt.table[42]; blob 1 calls blob 0 indirectly using
        // its baked type index 1 and reserved absolute table slot 50.
        let out = InprocOut {
            blobs: vec![
                vec![0x60, 1, 0x7f, 1, 0x7f, 0, 0x20, 0, 0x10, 0, 0x0b],
                vec![
                    0x60, 1, 0x7f, 1, 0x7f, 0, 0x20, 0, 0x41, 50, 0x11, 1, 0, 0x0b,
                ],
            ],
            extern_table_indices: vec![42],
            extern_sigs: vec![SignatureData {
                params: vec![Type::I32],
                returns: vec![Type::I32],
            }],
            scripts: vec![],
            env_desc: vec![],
            module: None,
        };
        let mut config = wasmtime::Config::new();
        config.shared_memory(true);
        let engine = Engine::new(&config).unwrap();
        let module = HostModule::new(&engine, assemble_module(&out).unwrap()).unwrap();
        let mut store = Store::new(&engine, ());
        let helper = Func::wrap(&mut store, |x: i32| x + 1);
        let memory = SharedMemory::new(&engine, HostMemoryType::shared(1, 32)).unwrap();
        let table = Table::new(
            &mut store,
            HostTableType::new(HostRefType::FUNCREF, 50, None),
            Ref::Func(None),
        )
        .unwrap();
        table.set(&mut store, 42, Ref::Func(Some(helper))).unwrap();
        let base = table.grow(&mut store, 2, Ref::Func(None)).unwrap();
        assert_eq!(base, 50);
        let mut linker = Linker::new(&engine);
        linker
            .define(&mut store, "yurt.table", "42", helper)
            .unwrap();
        linker.define(&mut store, "env", "memory", memory).unwrap();
        linker
            .define(&mut store, "env", "__indirect_function_table", table)
            .unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        for i in 0..2 {
            let f = instance
                .get_func(&mut store, &format!("__jit_entry_{i}"))
                .unwrap();
            table.set(&mut store, base + i, Ref::Func(Some(f))).unwrap();
        }
        let entry = instance
            .get_typed_func::<i32, i32>(&mut store, "__jit_entry_1")
            .unwrap();
        assert_eq!(entry.call(&mut store, 41).unwrap(), 42);
    }

    /// Type index `nextern + i` is blob `i`'s functype, and function `i`
    /// uses that type. A same-arity mixup still validates; this is the
    /// check that catches it.
    fn assert_blob_types(bytes: &[u8], blobs: &[Vec<u8>], n_extern: usize) {
        let mut types: Vec<(Vec<ValType>, Vec<ValType>)> = Vec::new();
        let mut func_tys: Vec<u32> = Vec::new();
        for payload in Parser::new(0).parse_all(bytes) {
            match payload.unwrap() {
                Payload::TypeSection(reader) => {
                    for rec in reader {
                        for ty in rec.unwrap().into_types() {
                            match &ty.composite_type.inner {
                                wasmparser::CompositeInnerType::Func(fty) => {
                                    let params = fty
                                        .params()
                                        .iter()
                                        .map(|&t| enc_valtype(t).unwrap())
                                        .collect();
                                    let results = fty
                                        .results()
                                        .iter()
                                        .map(|&t| enc_valtype(t).unwrap())
                                        .collect();
                                    types.push((params, results));
                                }
                                other => panic!("non-function type {other:?}"),
                            }
                        }
                    }
                }
                Payload::FunctionSection(reader) => {
                    for idx in reader {
                        func_tys.push(idx.unwrap());
                    }
                }
                _ => {}
            }
        }
        assert_eq!(func_tys.len(), blobs.len());
        for (i, blob) in blobs.iter().enumerate() {
            let (params, results, _) = parse_blob(blob).unwrap();
            assert_eq!(types[n_extern + i].0, params, "blob {i} params");
            assert_eq!(types[n_extern + i].1, results, "blob {i} results");
            assert_eq!(func_tys[i], (n_extern + i) as u32, "blob {i} type index");
        }
    }

    #[test]
    fn yurt_module_keeps_blob_types_and_mount_abi() {
        let out = super::super::inprocess::toy_inproc_out();
        let bytes = assemble_module(&out).expect("assemble");
        wasmparser::validate(&bytes).expect("self-assembled module validates");
        assert_blob_types(&bytes, &out.blobs, out.extern_sigs.len());

        let (mut helpers, mut memory, mut table, mut entries) = (0usize, false, false, 0usize);
        for payload in Parser::new(0).parse_all(&bytes) {
            match payload.unwrap() {
                Payload::ImportSection(reader) => {
                    for import in reader.into_imports() {
                        let import = import.unwrap();
                        match (import.module, import.name, import.ty) {
                            ("yurt.table", slot, TypeRef::Func(_)) => {
                                assert_eq!(slot, out.extern_table_indices[helpers].to_string());
                                helpers += 1;
                            }
                            ("env", "memory", TypeRef::Memory(m)) => memory = m.shared,
                            ("env", "__indirect_function_table", TypeRef::Table(_)) => table = true,
                            other => panic!("unexpected import {other:?}"),
                        }
                    }
                }
                Payload::ExportSection(reader) => {
                    for export in reader {
                        assert_eq!(export.unwrap().name, format!("__jit_entry_{entries}"));
                        entries += 1;
                    }
                }
                Payload::CodeSectionEntry(body) => {
                    let mut ops = body.get_operators_reader().unwrap();
                    while !ops.eof() {
                        if let op @ (Operator::GlobalGet { .. }
                        | Operator::GlobalSet { .. }
                        | Operator::MemoryGrow { .. }
                        | Operator::TableSet { .. }
                        | Operator::TableGrow { .. }
                        | Operator::TableFill { .. }
                        | Operator::TableInit { .. }
                        | Operator::TableCopy { .. }) = ops.read().unwrap()
                        {
                            panic!("blob uses {op:?}");
                        }
                    }
                }
                _ => {}
            }
        }
        assert_eq!(helpers, out.extern_table_indices.len());
        assert!(memory && table);
        assert_eq!(entries, out.blobs.len());
    }
}
