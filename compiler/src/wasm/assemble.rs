//! Assemble an in-process batch into one self-contained module, for hosts
//! that mount whole modules instead of taking function blobs through a
//! hostcall (wasm-jit-runner). The layout mirrors the runner's
//! `build_module` (wasm-jit-runner/src/addfuncs.rs), so the blob bodies'
//! `call` and `call_indirect` immediates mean the same thing: the type
//! section is the extern (helper) types `0..nextern` and then one type per
//! blob, in order (no deduplication); helpers are function imports
//! `0..nextern` and blob `i` is function `nextern + i`.
//!
//! The imports and exports follow the Yurt JIT mount ABI
//! (yurtos-kernel docs/abi/jit-mount.md): the owner's shared linear memory
//! as `env.memory`, its funcref table as `env.__indirect_function_table`,
//! each helper as `yurt.table "<slot>"` (the C function's table index), and
//! blob `i` exported as `__jit_entry_<i>`, which a reserved mount publishes
//! at `base + i`. The carved bodies reference no globals (the batch module
//! defines none), so no global is imported.

use waffle::Type;
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, FunctionSection, ImportSection,
    MemoryType, Module, RefType, TableType, TypeSection, ValType,
};

use super::inprocess::InprocOut;

/// The declared maximum of the `env.memory` import. A shared memory import
/// must declare one, and import matching needs it at or above the owner's
/// own maximum; the wasm32 limit is above every owner's.
const MEMORY_MAX_PAGES: u64 = 65536;

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
    let params = fty.params().iter().map(|&t| enc_valtype(t)).collect::<Result<_, _>>()?;
    let results = fty.results().iter().map(|&t| enc_valtype(t)).collect::<Result<_, _>>()?;
    let body = &blob[reader.current_position()..];
    if body.is_empty() {
        return Err("blob has an empty body".to_string());
    }
    Ok((params, results, body))
}

/// The batch as a Yurt JIT mount module (see the module docs).
pub fn assemble_module(out: &InprocOut) -> Result<Vec<u8>, String> {
    let n_extern = u32::try_from(out.extern_sigs.len()).unwrap();
    let blobs = out
        .blobs
        .iter()
        .map(|b| parse_blob(b))
        .collect::<Result<Vec<_>, _>>()?;

    let mut types = TypeSection::new();
    for sig in &out.extern_sigs {
        let params = sig.params.iter().map(|&t| enc_type(t)).collect::<Result<Vec<_>, _>>()?;
        let results = sig.returns.iter().map(|&t| enc_type(t)).collect::<Result<Vec<_>, _>>()?;
        types.ty().function(params, results);
    }
    for (params, results, _) in &blobs {
        types.ty().function(params.iter().copied(), results.iter().copied());
    }

    let mut imports = ImportSection::new();
    for (i, slot) in out.extern_table_indices.iter().enumerate() {
        imports.import("yurt.table", &slot.to_string(), EntityType::Function(i as u32));
    }
    imports.import(
        "env",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: 0,
            maximum: Some(MEMORY_MAX_PAGES),
            memory64: false,
            shared: true,
            page_size_log2: None,
        }),
    );
    imports.import(
        "env",
        "__indirect_function_table",
        EntityType::Table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: 0,
            maximum: None,
            shared: false,
        }),
    );

    let mut funcs = FunctionSection::new();
    let mut exports = ExportSection::new();
    let mut code = CodeSection::new();
    for (i, (_, _, body)) in blobs.iter().enumerate() {
        let i = u32::try_from(i).unwrap();
        funcs.function(n_extern + i);
        exports.export(&format!("__jit_entry_{i}"), ExportKind::Func, n_extern + i);
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
