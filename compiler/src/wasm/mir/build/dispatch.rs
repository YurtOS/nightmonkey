//! Which family of arms builds each bytecode op. One exhaustive match: a new `JSOp` (the opcode
//! table is regenerated for an engine update) does not compile until it is classified, instead of
//! failing at run time with an op nobody builds.

use super::*;

/// The family of `Run::op` arms that builds an op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpFamily {
    Stack,
    Arith,
    Control,
    Names,
    Elems,
    Objects,
    Misc,
    Super,
    Gen,
    Calls,
    /// An op no family builds: the script is declined (`Run::op`).
    Unsupported,
}

pub(super) fn op_family(op: JSOp) -> OpFamily {
    match op {
        JSOp::Undefined
        | JSOp::Null
        | JSOp::False
        | JSOp::True
        | JSOp::Int32
        | JSOp::Zero
        | JSOp::One
        | JSOp::Int8
        | JSOp::Uint16
        | JSOp::Uint24
        | JSOp::Double
        | JSOp::Symbol
        | JSOp::Void
        | JSOp::NopIsAssignOp
        | JSOp::ToString
        | JSOp::InitPropGetter
        | JSOp::InitHiddenPropGetter
        | JSOp::InitPropSetter
        | JSOp::InitHiddenPropSetter
        | JSOp::RegExp
        | JSOp::Lambda
        | JSOp::BuiltinObject
        | JSOp::JumpTarget
        | JSOp::LoopHead
        | JSOp::GetRval
        | JSOp::SetRval
        | JSOp::ThrowWithStack
        | JSOp::Try
        | JSOp::TryDestructuring
        | JSOp::Finally
        | JSOp::Uninitialized
        | JSOp::InitLexical
        | JSOp::InitAliasedLexical
        | JSOp::CheckLexical
        | JSOp::CheckAliasedLexical
        | JSOp::GetArg
        | JSOp::GetFrameArg
        | JSOp::GetLocal
        | JSOp::GetAliasedVar
        | JSOp::GetAliasedDebugVar
        | JSOp::GetIntrinsic
        | JSOp::SetArg
        | JSOp::SetLocal
        | JSOp::SetAliasedVar
        | JSOp::DebugLeaveLexicalEnv
        | JSOp::Pop
        | JSOp::PopN
        | JSOp::Dup
        | JSOp::Dup2
        | JSOp::DupAt
        | JSOp::Swap
        | JSOp::Pick
        | JSOp::Unpick
        | JSOp::Nop
        | JSOp::Lineno
        | JSOp::NopDestructuring
        | JSOp::Debugger => OpFamily::Stack,
        JSOp::Pos
        | JSOp::Neg
        | JSOp::BitNot
        | JSOp::Not
        | JSOp::BitOr
        | JSOp::BitXor
        | JSOp::BitAnd
        | JSOp::Eq
        | JSOp::Ne
        | JSOp::StrictEq
        | JSOp::StrictNe
        | JSOp::Lt
        | JSOp::Gt
        | JSOp::Le
        | JSOp::Ge
        | JSOp::Lsh
        | JSOp::Rsh
        | JSOp::Ursh
        | JSOp::Add
        | JSOp::Sub
        | JSOp::Inc
        | JSOp::Dec
        | JSOp::Mul
        | JSOp::Div
        | JSOp::Mod
        | JSOp::Pow
        | JSOp::ToNumeric => OpFamily::Arith,
        JSOp::String
        | JSOp::TypeofEq
        | JSOp::StrictConstantEq
        | JSOp::StrictConstantNe
        | JSOp::IsNullOrUndefined
        | JSOp::Goto
        | JSOp::JumpIfFalse
        | JSOp::JumpIfTrue
        | JSOp::And
        | JSOp::Or
        | JSOp::Case
        | JSOp::Default
        | JSOp::TableSwitch
        | JSOp::Return
        | JSOp::RetRval
        | JSOp::BindUnqualifiedGName
        | JSOp::SetGName
        | JSOp::StrictSetGName
        | JSOp::FunctionThis => OpFamily::Control,
        JSOp::GetProp | JSOp::SetProp | JSOp::StrictSetProp | JSOp::GetGName => OpFamily::Names,
        JSOp::Instanceof
        | JSOp::In
        | JSOp::GetElem
        | JSOp::SetElem
        | JSOp::StrictSetElem
        | JSOp::DelProp
        | JSOp::StrictDelProp
        | JSOp::DelElem
        | JSOp::StrictDelElem
        | JSOp::HasOwn => OpFamily::Elems,
        JSOp::Typeof
        | JSOp::TypeofExpr
        | JSOp::ToPropertyKey
        | JSOp::NewInit
        | JSOp::NewObject
        | JSOp::InitProp
        | JSOp::InitHiddenProp
        | JSOp::InitLockedProp
        | JSOp::InitElem
        | JSOp::InitHiddenElem
        | JSOp::InitLockedElem
        | JSOp::NewArray
        | JSOp::InitElemArray
        | JSOp::InitElemInc
        | JSOp::Throw
        | JSOp::ArgumentsLength
        | JSOp::GetActualArg
        | JSOp::Arguments
        | JSOp::Rest => OpFamily::Objects,
        JSOp::BigInt
        | JSOp::GlobalThis
        | JSOp::NewTarget
        | JSOp::CheckPrivateField
        | JSOp::Iter
        | JSOp::MoreIter
        | JSOp::IsNoIter
        | JSOp::EndIter
        | JSOp::CheckObjCoercible
        | JSOp::MutateProto
        | JSOp::Hole
        | JSOp::SetFunName
        | JSOp::CheckClassHeritage
        | JSOp::IsConstructing
        | JSOp::New
        | JSOp::NewContent
        | JSOp::ThrowMsg
        | JSOp::ThrowSetConst
        | JSOp::CheckThis
        | JSOp::BindUnqualifiedName
        | JSOp::BindName
        | JSOp::GetName
        | JSOp::Callee
        | JSOp::SetName
        | JSOp::StrictSetName
        | JSOp::PushLexicalEnv
        | JSOp::PopLexicalEnv
        | JSOp::RecreateLexicalEnv
        | JSOp::FreshenLexicalEnv
        | JSOp::PushClassBodyEnv
        | JSOp::PushVarEnv
        | JSOp::EnterWith
        | JSOp::LeaveWith
        | JSOp::BindVar
        | JSOp::DelName => OpFamily::Misc,
        JSOp::Object
        | JSOp::InitElemGetter
        | JSOp::InitHiddenElemGetter
        | JSOp::InitElemSetter
        | JSOp::InitHiddenElemSetter
        | JSOp::SuperBase
        | JSOp::GetPropSuper
        | JSOp::GetElemSuper
        | JSOp::SetPropSuper
        | JSOp::StrictSetPropSuper
        | JSOp::SetElemSuper
        | JSOp::StrictSetElemSuper
        | JSOp::InitHomeObject
        | JSOp::FunWithProto
        | JSOp::Eval
        | JSOp::StrictEval
        | JSOp::CallSiteObj
        | JSOp::SuperCall
        | JSOp::SuperFun
        | JSOp::Coalesce
        | JSOp::EnvCallee => OpFamily::Super,
        JSOp::DynamicImport
        | JSOp::ObjWithProto
        | JSOp::NewPrivateName
        | JSOp::SpreadEval
        | JSOp::StrictSpreadEval
        | JSOp::CheckThisReinit
        | JSOp::Generator
        | JSOp::InitialYield
        | JSOp::AfterYield
        | JSOp::FinalYieldRval
        | JSOp::Yield
        | JSOp::IsGenClosing
        | JSOp::AsyncAwait
        | JSOp::AsyncResolve
        | JSOp::AsyncReject
        | JSOp::Await
        | JSOp::CanSkipAwait
        | JSOp::MaybeExtractAwaitValue
        | JSOp::ResumeKind
        | JSOp::CheckResumeKind
        | JSOp::Resume
        | JSOp::CheckReturn
        | JSOp::CreateSuppressedError
        | JSOp::GetBoundName
        | JSOp::AddDisposable
        | JSOp::TakeDisposeCapability => OpFamily::Gen,
        JSOp::CloseIter
        | JSOp::OptimizeGetIterator
        | JSOp::CheckIsObj
        | JSOp::Call
        | JSOp::CallContent
        | JSOp::CallIter
        | JSOp::CallContentIter
        | JSOp::CallIgnoresRv
        | JSOp::SpreadCall
        | JSOp::OptimizeSpreadCall
        | JSOp::SpreadNew
        | JSOp::SpreadSuperCall => OpFamily::Calls,
        JSOp::NonSyntacticGlobalThis
        | JSOp::ImportMeta
        | JSOp::ToAsyncIter
        | JSOp::ImplicitThis
        | JSOp::Exception
        | JSOp::ExceptionAndStack
        | JSOp::InitGLexical
        | JSOp::GetImport
        | JSOp::SetIntrinsic
        | JSOp::GlobalOrEvalDeclInstantiation
        | JSOp::ForceInterpreter
        | JSOp::DebugCheckSelfHosted => OpFamily::Unsupported,
    }
}
