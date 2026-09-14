use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type NodeId = String;
pub type SymbolId = String;
pub type TypeId = String;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectInput {
    pub root: String,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub entrypoints: Vec<String>,
    pub stdlib_root: Option<String>,
    pub acton_stdlib_root: Option<String>,
    #[serde(default)]
    pub import_mappings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshot {
    pub version: VersionInfo,
    pub root: String,
    pub files: Vec<SourceFile>,
    pub nodes: Vec<AstNode>,
    pub symbols: Vec<SymbolInfo>,
    pub references: Vec<Reference>,
    pub resolutions: Vec<Resolution>,
    pub types: Vec<TypeInfo>,
    pub node_types: Vec<NodeType>,
    pub call_graph: Vec<CallEdge>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub package_version: &'static str,
    pub acton_revision: &'static str,
    pub tolk_version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    pub path: String,
    pub source: String,
    pub root_node: NodeId,
    pub source_kind: SourceKind,
    pub imports: Vec<ImportInfo>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Workspace,
    Stdlib,
    Acton,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportInfo {
    pub path: String,
    pub target_path: Option<String>,
    pub location: SourceLocation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AstNode {
    pub id: NodeId,
    pub kind: String,
    pub raw_kind: String,
    pub named: bool,
    pub error: bool,
    pub parent_id: Option<NodeId>,
    pub child_ids: Vec<NodeId>,
    pub fields: BTreeMap<String, Vec<NodeId>>,
    pub location: SourceLocation,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInfo {
    pub id: SymbolId,
    pub name: String,
    pub fqn: String,
    pub kind: String,
    pub declaration: SourceLocation,
    pub body: Option<SourceLocation>,
    pub containing_symbol: Option<SymbolId>,
    pub documentation: Option<String>,
    pub flags: SymbolFlags,
    pub node_id: Option<NodeId>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolFlags {
    pub deprecated: bool,
    pub pure: bool,
    pub private: bool,
    pub mutable: bool,
    pub local: bool,
    pub parameter: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub symbol_id: Option<SymbolId>,
    pub name: String,
    pub location: SourceLocation,
    pub context: ReferenceContext,
    pub node_id: Option<NodeId>,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub node_id: NodeId,
    pub symbol_id: Option<SymbolId>,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceContext {
    pub namespace: String,
    pub usage: String,
    pub access: ReferenceAccess,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceAccess {
    pub read: bool,
    pub write: bool,
    pub mutate: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeInfo {
    pub id: TypeId,
    pub display: String,
    pub kind: String,
    pub symbol_id: Option<SymbolId>,
    pub element_types: Vec<TypeId>,
    pub return_type: Option<TypeId>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeType {
    pub node_id: NodeId,
    pub type_id: TypeId,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallEdge {
    pub caller: SymbolId,
    pub callee: SymbolId,
    pub call_site: SourceLocation,
    pub node_id: Option<NodeId>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub phase: String,
    pub source: String,
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub path: String,
    pub range: SourceRange,
    pub byte_range: ByteRange,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    pub start: u32,
    pub end: u32,
}
