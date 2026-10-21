//! Which family of arms lowers each MIR opcode. One exhaustive match: a new `Opcode` does not
//! compile until it is classified, instead of failing at lowering with an op nobody lowers.

use super::*;

/// The family of `Lower::inst` arms that lowers an op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InstFamily {
    Values,
    Control,
    Guards,
    Env,
    Access,
    Calls,
    Runtime,
    Construct,
    Fields,
    /// An opcode no family lowers (reserved variants no builder emits).
    Unsupported,
}

pub(super) fn inst_family(op: &Opcode) -> InstFamily {
    match op {
        Opcode::ConstVal(..)
        | Opcode::ConstI32(..)
        | Opcode::ConstF64(..)
        | Opcode::ConstBool(..)
        | Opcode::Box
        | Opcode::BoxDouble
        | Opcode::Unbox(..)
        | Opcode::I32ToInt
        | Opcode::I32ToF64
        | Opcode::IntToF64
        | Opcode::Weaken
        | Opcode::CheckNative(..)
        | Opcode::I32Wrap(..)
        | Opcode::IntArith(..)
        | Opcode::F64Arith(..)
        | Opcode::F64Neg
        | Opcode::I32Bit(..)
        | Opcode::I32Ushr
        | Opcode::ToInt32
        | Opcode::Cmp(..)
        | Opcode::Math(..)
        | Opcode::JsToBool => InstFamily::Values,
        Opcode::Jump
        | Opcode::Br
        | Opcode::Switch(..)
        | Opcode::SwitchStr(..)
        | Opcode::Return
        | Opcode::Exit { .. }
        | Opcode::ExitThrow { .. }
        | Opcode::GenSuspend { .. }
        | Opcode::IsGenClosing
        | Opcode::Unreachable
        | Opcode::Restamp(..)
        | Opcode::StampFresh(..) => InstFamily::Control,
        Opcode::GuardUnbox(..)
        | Opcode::GuardTags(..)
        | Opcode::F64ToIntExact
        | Opcode::I32Ovf(..)
        | Opcode::JsAdd
        | Opcode::JsBinop(..)
        | Opcode::JsUnop(..)
        | Opcode::JsCompare(..)
        | Opcode::JsTypeofEq(..)
        | Opcode::JsConstantStrictEq(..)
        | Opcode::ArgsMapped(..)
        | Opcode::ArgsMappedSet(..)
        | Opcode::JsToNumeric
        | Opcode::Prim(..) => InstFamily::Guards,
        Opcode::JsGetName(..)
        | Opcode::FrameStore(..)
        | Opcode::JsLambda(..)
        | Opcode::LitNew(..)
        | Opcode::LitInit(..)
        | Opcode::GlobalObject
        | Opcode::EnvCurrent
        | Opcode::EnvCallee(..)
        | Opcode::ObjectLit(..)
        | Opcode::EnvSet
        | Opcode::EnvPop
        | Opcode::EnvParent
        | Opcode::EnvLoad(..)
        | Opcode::EnvStore(..) => InstFamily::Env,
        Opcode::GuardKind(..)
        | Opcode::IntToI32
        | Opcode::GuardRange(..)
        | Opcode::JsGetProp(..)
        | Opcode::GetPropData(..)
        | Opcode::SetPropData(..)
        | Opcode::JsSetProp(..)
        | Opcode::JsGetElem
        | Opcode::GetElemData
        | Opcode::SetElemData(..)
        | Opcode::JsSetElem(..)
        | Opcode::LoadElem
        | Opcode::StoreElem(..)
        | Opcode::LoadTa
        | Opcode::StoreTa
        | Opcode::LengthArray
        | Opcode::LengthString
        | Opcode::LengthTa => InstFamily::Access,
        Opcode::GuardScript(..)
        | Opcode::AccessorProbe(..)
        | Opcode::InlineEnter
        | Opcode::Call
        | Opcode::CallIter
        | Opcode::CallEval(..)
        | Opcode::CreateThis(..)
        | Opcode::NewThis(..)
        | Opcode::NewThisInit { .. }
        | Opcode::FnIsCtor
        | Opcode::ObjEmulatesUndef
        | Opcode::CtorStamp(..)
        | Opcode::CtorPublish(..) => InstFamily::Calls,
        Opcode::JsTypeof
        | Opcode::JsRt(..)
        | Opcode::ArgsObject
        | Opcode::RestArray(..)
        | Opcode::ArgsLength
        | Opcode::FrameNewTarget
        | Opcode::FrameCallee
        | Opcode::IterMore
        | Opcode::IterIsDone
        | Opcode::IterEnd
        | Opcode::IterOptimizable
        | Opcode::ActualArg
        | Opcode::ActualArgOr(..)
        | Opcode::JsIsBuiltin(..)
        | Opcode::ApplyFwd
        | Opcode::JsThrow => InstFamily::Runtime,
        Opcode::GuardLayout { .. }
        | Opcode::GuardCtor { .. }
        | Opcode::MethodLoad { .. }
        | Opcode::CheckFuse(..)
        | Opcode::CheckBinding(..)
        | Opcode::ExitInline { .. }
        | Opcode::InitField(..)
        | Opcode::PublishLayout
        | Opcode::LoadGName(..)
        | Opcode::StoreGName(..)
        | Opcode::Construct(..) => InstFamily::Construct,
        Opcode::ConstStr(..)
        | Opcode::JsBoxThis
        | Opcode::JsBindGName(..)
        | Opcode::JsSetName(..)
        | Opcode::LoadField(..)
        | Opcode::LoadSlot(..)
        | Opcode::StoreSlot(..)
        | Opcode::StoreField(..) => InstFamily::Fields,
        Opcode::ConstObj(..)
        | Opcode::GuardSingleton(..)
        | Opcode::NewObject(..)
        | Opcode::NewArray
        | Opcode::ElementsPtr
        | Opcode::StrCharCodeAt
        | Opcode::CallDirect
        | Opcode::CallNative(..) => InstFamily::Unsupported,
    }
}
