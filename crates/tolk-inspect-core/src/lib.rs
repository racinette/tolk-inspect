mod model;

pub use model::*;

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tolk_analysis::{
    AnalysisDb, ConstantEvaluationContext, ConstantEvaluator, ConstantValue as ActonConstantValue,
    UseFlags,
};
use tolk_dataflow::{
    ControlFlowGraph as ActonControlFlowGraph, EdgeKind as ActonEdgeKind,
    FlowNodeKind as ActonFlowNodeKind, build_cfg_for_function_with_source,
};
use tolk_linter::diagnostic::{
    Applicability as LintApplicability, DiagnosticTag as LintDiagnosticTag,
    Severity as LintSeverity,
};
use tolk_linter::{Checker, RuleSettingsBuilder};
use tolk_resolver::{
    FileDb, FileId, NameUse, NameUseKind, ProjectIndex, ProjectSource, ProjectSourceProvider,
    Resolved, Span, Symbol, SymbolKind,
};
use tolk_ty::{FileBodyTypes, TyData, TyId, TypeDb, TypeInterner, WorkspaceBodyTypes, infer};
use tree_sitter::Node;

pub const ACTON_REVISION: &str = "17654feb713c5824ee4cc0259b7be9b5f72898ba";
pub const TOLK_VERSION: &str = "1.4.2";

type ResolutionsBySpan = HashMap<(FileId, u32, u32), Resolved>;
type CallableState = HashMap<tolk_resolver::resolve_index::LocalDefId, CallableValue>;
type ParameterValues = HashMap<SymbolId, Vec<Option<CallableValue>>>;
type ReturnValues = HashMap<SymbolId, CallableValue>;
type MutationValues = HashMap<SymbolId, Vec<Option<CallableValue>>>;
type CaptureValues =
    HashMap<SymbolId, HashMap<tolk_resolver::resolve_index::LocalDefId, CallableValue>>;
type CollectionKinds = HashMap<(FileId, u32, u32), CollectionKind>;
type ConstantKeys = HashMap<tolk_resolver::SymbolId, String>;

const DYNAMIC_MEMBER: &str = "\0dynamic";
const MAP_VALUE_MEMBER: &str = "\0map-value";
const MAX_TRACKED_ARRAY_LENGTHS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionKind {
    Array,
    Map,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CallableValue {
    targets: BTreeSet<SymbolId>,
    complete: bool,
    members: BTreeMap<String, CallableValue>,
    members_complete: bool,
    array_lengths: Option<BTreeSet<usize>>,
    bottom: bool,
}

impl CallableValue {
    fn target(target: SymbolId) -> Self {
        Self {
            targets: BTreeSet::from([target]),
            complete: true,
            ..Self::default()
        }
    }

    fn non_callable() -> Self {
        Self {
            complete: true,
            ..Self::default()
        }
    }

    fn bottom() -> Self {
        Self {
            complete: true,
            members_complete: true,
            bottom: true,
            ..Self::default()
        }
    }

    fn member(&self, key: &str) -> Self {
        if self.bottom {
            return Self::bottom();
        }
        let exact = self.members.get(key).cloned().unwrap_or_else(|| {
            if self.members_complete {
                Self::non_callable()
            } else {
                Self::default()
            }
        });
        self.members
            .get(DYNAMIC_MEMBER)
            .map_or(exact.clone(), |dynamic| {
                merged_callable_value(&exact, dynamic)
            })
    }

    fn any_member(&self) -> Self {
        if self.bottom {
            return Self::bottom();
        }
        let mut result = if self.members_complete {
            Self::non_callable()
        } else {
            Self::default()
        };
        for value in self.members.values() {
            result = merged_callable_value(&result, value);
        }
        result
    }

    fn set_dynamic_member(&mut self, value: Self) {
        let current = self
            .members
            .entry(DYNAMIC_MEMBER.into())
            .or_insert_with(Self::bottom);
        *current = merged_callable_value(current, &value);
    }

    fn push_array_member(&mut self, value: Self) {
        let Some(lengths) = self.array_lengths.clone() else {
            self.set_dynamic_member(value);
            return;
        };
        for length in &lengths {
            let current = self
                .members
                .entry(format!("int:{length}"))
                .or_insert_with(Self::bottom);
            *current = merged_callable_value(current, &value);
        }
        let next_lengths = lengths
            .into_iter()
            .map(|length| length.saturating_add(1))
            .collect::<BTreeSet<_>>();
        if next_lengths.len() > MAX_TRACKED_ARRAY_LENGTHS {
            self.array_lengths = None;
            self.set_dynamic_member(value);
        } else {
            self.array_lengths = Some(next_lengths);
        }
    }

    fn set_member_path(&mut self, path: &[String], value: Self) {
        if self.bottom {
            *self = Self::default();
        }
        let Some((head, tail)) = path.split_first() else {
            *self = value;
            return;
        };
        if tail.is_empty() {
            self.members.insert(head.clone(), value);
        } else {
            self.members
                .entry(head.clone())
                .or_default()
                .set_member_path(tail, value);
        }
    }

    fn limit_member_depth(&mut self, remaining: usize) {
        if self.bottom || self.members.is_empty() {
            return;
        }
        if remaining == 0 {
            self.members.clear();
            self.members_complete = false;
            self.array_lengths = None;
            return;
        }
        for member in self.members.values_mut() {
            member.limit_member_depth(remaining - 1);
        }
    }
}

#[derive(Debug, Clone)]
struct LocalPath {
    root: tolk_resolver::resolve_index::LocalDefId,
    members: Vec<String>,
}

#[derive(Debug, Clone)]
struct ProgramCall {
    file_id: FileId,
    caller: SymbolId,
    span: Span,
}

#[derive(Debug, Clone)]
struct CallableDefinition {
    file_id: FileId,
    acton_symbol: Option<tolk_resolver::SymbolId>,
    graph: Arc<ActonControlFlowGraph>,
    parameters: Vec<tolk_resolver::resolve_index::LocalDefId>,
    mutable_parameters: Vec<bool>,
    captures: Vec<tolk_resolver::resolve_index::LocalDefId>,
}

#[derive(Debug, Clone)]
struct LambdaCreation {
    file_id: FileId,
    owner: SymbolId,
    lambda: SymbolId,
    span: Span,
}

#[derive(Debug, Default)]
struct ProgramAnalysis {
    call_sites: Vec<CallSite>,
    parameters: ParameterValues,
    returns: ReturnValues,
    mutations: MutationValues,
    captures: CaptureValues,
}

#[derive(Clone)]
struct LambdaAsFunction<'tree>(tolk_syntax::Lambda<'tree>);

impl<'tree> tolk_syntax::TryFromNode<'tree> for LambdaAsFunction<'tree> {
    type Error = tolk_syntax::InvalidNodeKindError;

    fn try_from_node(node: Node<'tree>) -> std::result::Result<Self, Self::Error> {
        <tolk_syntax::Lambda<'tree> as tolk_syntax::TryFromNode<'tree>>::try_from_node(node)
            .map(Self)
    }
}

impl<'tree> tolk_syntax::AstNode<'tree> for LambdaAsFunction<'tree> {
    fn syntax(&self) -> Node<'tree> {
        self.0.0
    }
}

impl<'tree> tolk_syntax::HasName<'tree> for LambdaAsFunction<'tree> {
    type Name = tolk_syntax::Ident<'tree>;

    fn name(&self) -> Option<Self::Name> {
        None
    }
}

impl<'tree> tolk_syntax::FunctionLike<'tree> for LambdaAsFunction<'tree> {
    fn return_type(&self) -> Option<tolk_syntax::Type<'tree>> {
        self.0.return_type()
    }

    fn body(&self) -> Option<tolk_syntax::FuncBody<'tree>> {
        self.0.body().map(tolk_syntax::FuncBody::Block)
    }

    fn parameters(&self) -> tolk_syntax::AstChildren<'tree, tolk_syntax::Parameter<'tree>> {
        // Acton's CFG builder currently consumes only `body()`. Lambda parameters use a
        // distinct AST wrapper and are mapped to resolver locals by `tolk-inspect`.
        tolk_syntax::AstChildren::default()
    }
}

#[derive(Debug)]
struct MemoryProvider {
    files: BTreeMap<PathBuf, Arc<str>>,
}

impl ProjectSourceProvider for MemoryProvider {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        normalize_path(path)
    }

    fn source(&self, path: &Path) -> Result<Option<ProjectSource>> {
        Ok(self.files.get(path).cloned().map(ProjectSource::Text))
    }
}

pub fn inspect(input: ProjectInput) -> Result<ProjectSnapshot> {
    let control_flow = input.control_flow;
    let root = normalize_logical_path(Path::new("/"), &input.root)?;
    let stdlib_root = input
        .stdlib_root
        .as_deref()
        .map(|path| normalize_logical_path(&root, path))
        .transpose()?;
    let acton_root = input
        .acton_stdlib_root
        .as_deref()
        .map(|path| normalize_logical_path(&root, path))
        .transpose()?;

    let mut files = BTreeMap::new();
    for (path, source) in input.files {
        let path = normalize_logical_path(&root, &path)?;
        if files.insert(path.clone(), Arc::from(source)).is_some() {
            bail!(
                "duplicate logical path after normalization: {}",
                path.display()
            );
        }
    }
    if files.is_empty() {
        bail!("files must contain at least one Tolk source");
    }

    let mut roots = input
        .entrypoints
        .iter()
        .map(|path| normalize_logical_path(&root, path))
        .collect::<Result<Vec<_>>>()?;
    if roots.is_empty() {
        roots = files
            .keys()
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "tolk")
                    && stdlib_root
                        .as_ref()
                        .is_none_or(|stdlib| !path.starts_with(stdlib))
                    && acton_root
                        .as_ref()
                        .is_none_or(|acton| !path.starts_with(acton))
            })
            .cloned()
            .collect();
    }
    roots.sort();
    roots.dedup();
    let Some(first_root) = roots.first().cloned() else {
        bail!("entrypoints must contain at least one Tolk source");
    };
    for entrypoint in &roots {
        if !files.contains_key(entrypoint) {
            bail!(
                "entrypoint is not present in files: {}",
                entrypoint.display()
            );
        }
    }

    let mappings = input
        .import_mappings
        .into_iter()
        .map(|(key, value)| {
            let path = normalize_logical_path(&root, &value)?;
            Ok((key, path.to_string_lossy().into_owned()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let provider = MemoryProvider { files };
    let no_stdlib = root.join(".__tolk_inspect_no_stdlib__");
    let file_db = FileDb::new(stdlib_root.clone().unwrap_or(no_stdlib), acton_root);
    let mut builder = ProjectIndex::builder(&file_db, first_root)
        .with_additional_roots(roots.into_iter().skip(1))
        .with_mappings(&Some(mappings));
    if let Some(stdlib) = stdlib_root {
        builder = builder.with_stdlib(stdlib);
    }
    let mut project = builder
        .build_with_provider(&provider)
        .context("failed to construct the Tolk project")?;
    tolk_resolver::resolve(&file_db, &mut project);

    SnapshotBuilder::new(root, &file_db, &project, control_flow).build()
}

struct SnapshotBuilder<'a> {
    root: PathBuf,
    file_db: &'a FileDb,
    project: &'a ProjectIndex,
    paths: HashMap<FileId, String>,
    node_ids: HashMap<(FileId, u32, u32), NodeId>,
    symbol_ids: HashMap<tolk_resolver::SymbolId, SymbolId>,
    local_ids: HashMap<tolk_resolver::resolve_index::LocalDefId, SymbolId>,
    lambda_ids: HashMap<(FileId, u32, u32), SymbolId>,
    nodes: Vec<AstNode>,
    symbols: Vec<SymbolInfo>,
    diagnostics: Vec<Diagnostic>,
    control_flow: ControlFlowScope,
    collection_kinds: CollectionKinds,
    constant_keys: ConstantKeys,
}

impl<'a> SnapshotBuilder<'a> {
    fn new(
        root: PathBuf,
        file_db: &'a FileDb,
        project: &'a ProjectIndex,
        control_flow: ControlFlowScope,
    ) -> Self {
        Self {
            root,
            file_db,
            project,
            paths: HashMap::new(),
            node_ids: HashMap::new(),
            symbol_ids: HashMap::new(),
            local_ids: HashMap::new(),
            lambda_ids: HashMap::new(),
            nodes: vec![],
            symbols: vec![],
            diagnostics: vec![],
            control_flow,
            collection_kinds: CollectionKinds::new(),
            constant_keys: ConstantKeys::new(),
        }
    }

    fn build(mut self) -> Result<ProjectSnapshot> {
        let mut indexed_files = self.project.files().values().cloned().collect::<Vec<_>>();
        indexed_files.sort_by(|a, b| a.path.cmp(&b.path));
        for file in &indexed_files {
            self.paths
                .insert(file.id, file.path.to_string_lossy().into_owned());
        }

        let mut source_files = Vec::with_capacity(indexed_files.len());
        for file in &indexed_files {
            let info = self
                .file_db
                .get_by_id(file.id)
                .context("indexed file missing from database")?;
            let root = info.source().tree.root_node();
            self.collect_node(file.id, &info.source().source, root, None, 0);
            let root_node = format!(
                "n:{}:{}:{}:{}:0",
                self.paths[&file.id],
                root.start_byte(),
                root.end_byte(),
                root.kind()
            );

            for error in info.source().errors() {
                let start = point_to_byte(
                    &info.source().source,
                    error.span.start.row,
                    error.span.start.column,
                );
                let end = point_to_byte(
                    &info.source().source,
                    error.span.end.row,
                    error.span.end.column,
                );
                self.diagnostics.push(Diagnostic {
                    phase: "parse".into(),
                    source: "tolk-syntax".into(),
                    severity: "error".into(),
                    code: Some(
                        match error.kind {
                            tolk_syntax::ParseErrorKind::Unexpected => "unexpected-syntax",
                            tolk_syntax::ParseErrorKind::Missing => "missing-syntax",
                        }
                        .into(),
                    ),
                    message: error.message,
                    location: Some(self.location(file.id, start, end)),
                    help: None,
                    annotations: vec![],
                    fixes: vec![],
                });
            }

            let mut imports = vec![];
            for import in self.project.imports_of(file.id).unwrap_or_default() {
                let location = self.location(
                    file.id,
                    import.import().span.start(),
                    import.import().span.end(),
                );
                if import.target().is_none() {
                    self.diagnostics.push(Diagnostic {
                        phase: "project".into(),
                        source: "tolk-resolver".into(),
                        severity: "error".into(),
                        code: Some("unresolved-import".into()),
                        message: format!("cannot resolve import `{}`", import.import().path),
                        location: Some(location.clone()),
                        help: None,
                        annotations: vec![],
                        fixes: vec![],
                    });
                }
                imports.push(ImportInfo {
                    path: import.import().path.to_string(),
                    target_path: import.target().and_then(|id| self.paths.get(&id).cloned()),
                    location,
                });
            }
            source_files.push(SourceFile {
                path: self.paths[&file.id].clone(),
                source: info.source().source.to_string(),
                root_node,
                source_kind: match file.source_kind {
                    tolk_resolver::file_index::FileSource::Workspace => SourceKind::Workspace,
                    tolk_resolver::file_index::FileSource::Stdlib => SourceKind::Stdlib,
                    tolk_resolver::file_index::FileSource::Acton => SourceKind::Acton,
                },
                imports,
            });
        }

        self.collect_symbols(&indexed_files);
        self.collect_lambda_symbols(&indexed_files);
        let constant_values = self.collect_constant_values(&indexed_files);

        let mut interner = TypeInterner::new();
        let mut type_db = TypeDb::new(&mut interner, self.file_db, self.project);
        let mut body_types = WorkspaceBodyTypes::default();
        let mut top_types = Vec::new();
        for file in &indexed_files {
            for symbol in all_global_symbols(&file.decls) {
                if let Some(ty) = type_db.get_top_level_type(Some(&symbol.kind), symbol.id) {
                    top_types.push((symbol.id, ty));
                }
            }
            let Some(info) = self.file_db.get_by_id(file.id) else {
                continue;
            };
            let mut bodies = FileBodyTypes::default();
            for declaration in info.source().top_levels() {
                let Some(symbol) = info.find_declaration(&declaration) else {
                    continue;
                };
                bodies.insert(
                    symbol.id,
                    infer(&mut type_db, file.id, symbol.id, &declaration),
                );
            }
            body_types.insert(file.id, bodies);
        }
        self.collection_kinds = collection_kinds(&body_types, &type_db);
        self.constant_keys = self.collect_constant_keys(&indexed_files);
        let mut analysis_db = AnalysisDb::new();
        let control_flow_graphs =
            self.collect_control_flow_graphs(&mut analysis_db, &type_db, &indexed_files);
        let (references, resolutions, call_sites, call_graph) =
            self.collect_references_and_calls(&body_types, &mut analysis_db, &type_db);
        drop(type_db);
        let workspace_file_ids = indexed_files
            .iter()
            .filter(|file| file.source_kind == tolk_resolver::file_index::FileSource::Workspace)
            .map(|file| file.id)
            .collect::<std::collections::HashSet<_>>();
        let lint_diagnostics = {
            let mut lint_interner = interner.clone();
            let mut lint_type_db = TypeDb::new(&mut lint_interner, self.file_db, self.project);
            let mut checker = Checker::new(self.file_db, &mut lint_type_db, &body_types)
                .with_settings(RuleSettingsBuilder::default().build())
                .with_project_root(self.root.clone());
            checker.run_once();
            for file_id in &workspace_file_ids {
                if let Some(file) = self.file_db.get_by_id(*file_id) {
                    checker.process_file(file.source(), *file_id);
                }
            }
            checker.apply_suppressions();
            checker.diagnostics
        };
        self.collect_lint_diagnostics(lint_diagnostics, &workspace_file_ids);

        let mut type_builder = TypeBuilder::new(&interner, &self.symbol_ids);
        let mut node_types = Vec::new();
        for (symbol_id, ty) in top_types {
            if let Some(symbol) = self.project.resolve_symbol(symbol_id) {
                if let Some(node_id) = self.exact_node(symbol_id.file_id, symbol.body_span) {
                    node_types.push(NodeType {
                        node_id,
                        type_id: type_builder.convert(ty),
                    });
                }
                if let Some(node_id) = self.exact_node(symbol_id.file_id, symbol.name_span) {
                    node_types.push(NodeType {
                        node_id,
                        type_id: type_builder.convert(ty),
                    });
                }
            }
        }
        for file in &indexed_files {
            let file_id = file.id;
            let Some(bodies) = body_types.get(&file_id) else {
                continue;
            };
            let mut ordered = bodies.values().collect::<Vec<_>>();
            ordered
                .sort_by_key(|inference| inference.expression_types.keys().map(|s| s.start).min());
            for inference in ordered {
                let mut expression_types = inference.expression_types.iter().collect::<Vec<_>>();
                expression_types.sort_by_key(|(span, _)| (span.start, span.end));
                for (&span, &ty) in expression_types {
                    if let Some(node_id) = self.exact_node(file_id, span) {
                        node_types.push(NodeType {
                            node_id,
                            type_id: type_builder.convert(ty),
                        });
                    }
                }
            }
        }
        node_types.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        node_types.dedup_by(|a, b| a.node_id == b.node_id);

        for error in self.project.errors() {
            self.diagnostics.push(Diagnostic {
                phase: "project".into(),
                source: "tolk-resolver".into(),
                severity: "error".into(),
                code: Some("project-index".into()),
                message: error.clone(),
                location: None,
                help: None,
                annotations: vec![],
                fixes: vec![],
            });
        }
        self.diagnostics
            .sort_by(|a, b| diagnostic_key(a).cmp(&diagnostic_key(b)));

        Ok(ProjectSnapshot {
            version: VersionInfo {
                package_version: env!("CARGO_PKG_VERSION"),
                acton_revision: ACTON_REVISION,
                tolk_version: TOLK_VERSION,
            },
            root: self.root.to_string_lossy().into_owned(),
            files: source_files,
            nodes: self.nodes,
            symbols: self.symbols,
            references,
            resolutions,
            types: type_builder.types,
            node_types,
            constant_values,
            control_flow_graphs,
            call_sites,
            call_graph,
            diagnostics: self.diagnostics,
        })
    }

    fn collect_node(
        &mut self,
        file_id: FileId,
        source: &str,
        node: Node<'_>,
        parent_id: Option<NodeId>,
        sibling: usize,
    ) {
        let id = format!(
            "n:{}:{}:{}:{}:{}",
            self.paths[&file_id],
            node.start_byte(),
            node.end_byte(),
            node.kind(),
            sibling
        );
        self.node_ids.insert(
            (file_id, node.start_byte() as u32, node.end_byte() as u32),
            id.clone(),
        );
        let mut child_ids = vec![];
        let mut fields = BTreeMap::<String, Vec<NodeId>>::new();
        for index in 0..node.child_count() {
            let Some(child) = node.child(index as u32) else {
                continue;
            };
            if !child.is_named() && !child.is_error() && !child.is_missing() {
                continue;
            }
            let child_id = format!(
                "n:{}:{}:{}:{}:{}",
                self.paths[&file_id],
                child.start_byte(),
                child.end_byte(),
                child.kind(),
                index
            );
            child_ids.push(child_id.clone());
            if let Some(field) = node.field_name_for_child(index as u32) {
                fields
                    .entry(field.to_string())
                    .or_default()
                    .push(child_id.clone());
            }
            self.collect_node(file_id, source, child, Some(id.clone()), index);
        }
        let text = source
            .get(node.start_byte()..node.end_byte())
            .unwrap_or("")
            .to_owned();
        self.nodes.push(AstNode {
            id,
            kind: camel_kind(node.kind()),
            raw_kind: node.kind().into(),
            named: node.is_named(),
            error: node.is_error() || node.is_missing(),
            parent_id,
            child_ids,
            fields,
            location: self.location(file_id, node.start_byte(), node.end_byte()),
            text,
        });
    }

    fn collect_symbols(&mut self, files: &[Arc<tolk_resolver::FileIndex>]) {
        for file in files {
            for symbol in all_global_symbols(&file.decls) {
                self.symbol_ids.insert(
                    symbol.id,
                    global_symbol_id(&self.paths[&file.id], symbol.id),
                );
            }
        }
        for file in files {
            if let Some(contract) = &file.contract {
                let contract_id = format!("c:{}", self.paths[&file.id]);
                self.symbols.push(SymbolInfo {
                    id: contract_id.clone(),
                    name: contract.name.to_string(),
                    fqn: contract.name.to_string(),
                    kind: "contract".into(),
                    declaration: self.location(
                        file.id,
                        contract.name_span.start(),
                        contract.name_span.end(),
                    ),
                    body: Some(self.location(
                        file.id,
                        contract.body_span.start(),
                        contract.body_span.end(),
                    )),
                    containing_symbol: None,
                    documentation: (!contract.doc.is_empty()).then(|| contract.doc.to_string()),
                    flags: SymbolFlags::default(),
                    node_id: self
                        .exact_node(file.id, contract.body_span)
                        .or_else(|| self.exact_node(file.id, contract.name_span)),
                });
                for field in &contract.fields {
                    self.symbols.push(SymbolInfo {
                        id: format!("{contract_id}:{}", field.name_span.start),
                        name: field.name.to_string(),
                        fqn: format!("{}.{}", contract.name, field.name),
                        kind: "contractField".into(),
                        declaration: self.location(
                            file.id,
                            field.name_span.start(),
                            field.name_span.end(),
                        ),
                        body: Some(self.location(
                            file.id,
                            field.body_span.start(),
                            field.body_span.end(),
                        )),
                        containing_symbol: Some(contract_id.clone()),
                        documentation: (!field.doc.is_empty()).then(|| field.doc.to_string()),
                        flags: SymbolFlags::default(),
                        node_id: self
                            .exact_node(file.id, field.body_span)
                            .or_else(|| self.exact_node(file.id, field.name_span)),
                    });
                }
            }
            for symbol in &file.decls {
                self.push_global_symbol(file.id, symbol, None);
            }
            if let Some(resolve) = self.project.get_resolved_uses(file.id) {
                let mut locals = resolve.locals.iter().collect::<Vec<_>>();
                locals.sort_by_key(|local| local.def_span.start);
                for local in locals {
                    let id = format!("l:{}:{}", self.paths[&file.id], local.id.local);
                    self.local_ids.insert(local.id, id.clone());
                    let containing_decl = self.file_db.get_by_id(file.id).and_then(|info| {
                        info.find_symbol_at(local.def_span.start())
                            .map(|symbol| symbol.id)
                    });
                    let containing =
                        containing_decl.and_then(|id| self.symbol_ids.get(&id).cloned());
                    let fqn = containing_decl
                        .and_then(|id| self.project.resolve_symbol(id))
                        .map_or_else(
                            || local.name.to_string(),
                            |owner| format!("{}::{}", owner.fqn, local.name),
                        );
                    let (kind, mutable, parameter) = match local.kind {
                        tolk_resolver::resolve_index::LocalDefKind::Param {
                            is_mutable, ..
                        } => ("parameter", is_mutable, true),
                        tolk_resolver::resolve_index::LocalDefKind::Var { is_mutable, .. } => {
                            ("localVariable", is_mutable, false)
                        }
                        tolk_resolver::resolve_index::LocalDefKind::Catch => {
                            ("catchVariable", false, false)
                        }
                        tolk_resolver::resolve_index::LocalDefKind::TypeParameter => {
                            ("typeParameter", false, false)
                        }
                    };
                    let location =
                        self.location(file.id, local.def_span.start(), local.def_span.end());
                    self.symbols.push(SymbolInfo {
                        id,
                        name: local.name.to_string(),
                        fqn,
                        kind: kind.into(),
                        declaration: location,
                        body: None,
                        containing_symbol: containing,
                        documentation: None,
                        flags: SymbolFlags {
                            mutable,
                            local: true,
                            parameter,
                            ..Default::default()
                        },
                        node_id: self.exact_node(file.id, local.def_span),
                    });
                }
            }
        }
        self.symbols.sort_by(|a, b| a.id.cmp(&b.id));
    }

    fn collect_lambda_symbols(&mut self, files: &[Arc<tolk_resolver::FileIndex>]) {
        #[derive(Clone)]
        struct LambdaDescriptor {
            file_id: FileId,
            path: String,
            start: u32,
            end: u32,
            node_id: NodeId,
            body: Option<SourceLocation>,
        }

        let file_ids = files
            .iter()
            .map(|file| (self.paths[&file.id].clone(), file.id))
            .collect::<HashMap<_, _>>();
        let lambdas = self
            .nodes
            .iter()
            .filter(|node| node.raw_kind == "lambda_expression")
            .filter_map(|node| {
                let file_id = *file_ids.get(&node.location.path)?;
                let body = node
                    .fields
                    .get("body")
                    .and_then(|ids| ids.first())
                    .and_then(|id| self.nodes.iter().find(|candidate| candidate.id == *id))
                    .map(|body| body.location.clone());
                Some(LambdaDescriptor {
                    file_id,
                    path: node.location.path.clone(),
                    start: node.location.byte_range.start,
                    end: node.location.byte_range.end,
                    node_id: node.id.clone(),
                    body,
                })
            })
            .collect::<Vec<_>>();

        for lambda in &lambdas {
            self.lambda_ids.insert(
                (lambda.file_id, lambda.start, lambda.end),
                format!("lambda:{}:{}:{}", lambda.path, lambda.start, lambda.end),
            );
        }
        for lambda in &lambdas {
            let id = self.lambda_ids[&(lambda.file_id, lambda.start, lambda.end)].clone();
            let global_owner = self
                .file_db
                .get_by_id(lambda.file_id)
                .and_then(|file| {
                    file.find_symbol_at(lambda.start as usize)
                        .map(|symbol| symbol.id)
                })
                .and_then(|symbol| self.symbol_ids.get(&symbol).cloned());
            let containing_symbol = lambdas
                .iter()
                .filter(|candidate| {
                    candidate.file_id == lambda.file_id
                        && candidate.start < lambda.start
                        && candidate.end > lambda.end
                })
                .min_by_key(|candidate| candidate.end - candidate.start)
                .map(|candidate| {
                    self.lambda_ids[&(candidate.file_id, candidate.start, candidate.end)].clone()
                })
                .or(global_owner.clone());
            let owner_fqn = global_owner
                .as_ref()
                .and_then(|owner| self.symbols.iter().find(|symbol| symbol.id == *owner))
                .map_or("<unknown>", |symbol| symbol.fqn.as_str());
            let declaration =
                self.location(lambda.file_id, lambda.start as usize, lambda.end as usize);
            self.symbols.push(SymbolInfo {
                id,
                name: "<lambda>".into(),
                fqn: format!(
                    "{owner_fqn}::<lambda@{}:{}>",
                    declaration.range.start.line + 1,
                    declaration.range.start.character + 1
                ),
                kind: "lambda".into(),
                declaration,
                body: lambda.body.clone(),
                containing_symbol,
                documentation: None,
                flags: SymbolFlags {
                    private: true,
                    local: true,
                    ..Default::default()
                },
                node_id: Some(lambda.node_id.clone()),
            });
        }

        let lambda_owners = self
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == "lambda")
            .map(|symbol| {
                (
                    symbol.id.clone(),
                    symbol.fqn.clone(),
                    symbol.declaration.path.clone(),
                    symbol.declaration.byte_range,
                )
            })
            .collect::<Vec<_>>();
        for symbol in self.symbols.iter_mut().filter(|symbol| {
            symbol.flags.local && symbol.kind != "lambda" && symbol.kind != "typeParameter"
        }) {
            let owner = lambda_owners
                .iter()
                .filter(|(_, _, path, range)| {
                    *path == symbol.declaration.path
                        && range.start <= symbol.declaration.byte_range.start
                        && range.end >= symbol.declaration.byte_range.end
                })
                .min_by_key(|(_, _, _, range)| range.end - range.start);
            if let Some((id, fqn, _, _)) = owner {
                symbol.containing_symbol = Some(id.clone());
                symbol.fqn = format!("{fqn}::{}", symbol.name);
            }
        }
        self.symbols.sort_by(|left, right| left.id.cmp(&right.id));
    }

    fn push_global_symbol(
        &mut self,
        file_id: FileId,
        symbol: &Symbol,
        containing: Option<SymbolId>,
    ) {
        let id = self.symbol_ids[&symbol.id].clone();
        self.symbols.push(SymbolInfo {
            id: id.clone(),
            name: symbol.name.to_string(),
            fqn: symbol.fqn.to_string(),
            kind: symbol_kind(&symbol.kind).into(),
            declaration: self.location(file_id, symbol.name_span.start(), symbol.name_span.end()),
            body: Some(self.location(file_id, symbol.body_span.start(), symbol.body_span.end())),
            containing_symbol: containing,
            documentation: (!symbol.doc.is_empty()).then(|| symbol.doc.to_string()),
            flags: SymbolFlags {
                deprecated: symbol.is_deprecated,
                pure: symbol.is_pure,
                private: symbol.is_private,
                ..Default::default()
            },
            node_id: self
                .exact_node(file_id, symbol.body_span)
                .or_else(|| self.exact_node(file_id, symbol.name_span)),
        });
        match &symbol.kind {
            SymbolKind::Struct { fields, .. } => {
                for nested in fields {
                    self.push_global_symbol(file_id, nested, Some(id.clone()));
                }
            }
            SymbolKind::Enum { members } => {
                for nested in members {
                    self.push_global_symbol(file_id, nested, Some(id.clone()));
                }
            }
            _ => {}
        }
    }

    fn collect_references_and_calls(
        &mut self,
        bodies: &WorkspaceBodyTypes,
        analysis_db: &mut AnalysisDb,
        type_db: &TypeDb<'_>,
    ) -> (
        Vec<Reference>,
        Vec<Resolution>,
        Vec<CallSite>,
        Vec<CallEdge>,
    ) {
        let mut uses = BTreeMap::<(FileId, u32, u32, String), &NameUse>::new();
        for (&file_id, index) in self.project.resolved_uses() {
            for usage in &index.uses {
                uses.insert(
                    (
                        file_id,
                        usage.span.start,
                        usage.span.end,
                        usage.name.to_string(),
                    ),
                    usage,
                );
            }
        }
        for (&file_id, file_bodies) in bodies {
            for inference in file_bodies.values() {
                for usage in &inference.resolved_refs {
                    let key = (
                        file_id,
                        usage.span.start,
                        usage.span.end,
                        usage.name.to_string(),
                    );
                    let should_replace = uses.get(&key).is_none_or(|existing| {
                        matches!(existing.resolved, Resolved::Unresolved)
                            && !matches!(usage.resolved, Resolved::Unresolved)
                    });
                    if should_replace {
                        uses.insert(key, usage);
                    }
                }
            }
        }

        let resolved_by_span = uses
            .iter()
            .map(|(&(file_id, start, end, _), usage)| {
                ((file_id, start, end), usage.resolved.clone())
            })
            .collect::<ResolutionsBySpan>();
        let mut references = vec![];
        let mut resolutions = vec![];
        for ((file_id, _, _, _), usage) in uses {
            let symbol_id = match usage.resolved {
                Resolved::Global(id) => self.symbol_ids.get(&id).cloned(),
                Resolved::Local(id) => self.local_ids.get(&id).cloned(),
                Resolved::Unresolved => None,
            };
            let node_id = self
                .exact_node(file_id, usage.span)
                .or_else(|| self.smallest_node(file_id, usage.span));
            let is_call = self.call_node(file_id, usage.span);
            let access_flags = analysis_db
                .use_facts(self.file_db, self.project, bodies, file_id)
                .and_then(|facts| facts.per_usage.get(&usage.span).copied())
                .unwrap_or(UseFlags::READ);
            let access = ReferenceAccess {
                read: access_flags.contains(UseFlags::READ),
                write: access_flags.contains(UseFlags::WRITE),
                mutate: access_flags.contains(UseFlags::MUTATE),
            };
            let namespace = match usage.kind {
                NameUseKind::Value | NameUseKind::LocalValue => "value",
                NameUseKind::Type => "type",
                NameUseKind::Mixed => "mixed",
            };
            references.push(Reference {
                symbol_id: symbol_id.clone(),
                name: usage.name.to_string(),
                location: self.location(file_id, usage.span.start(), usage.span.end()),
                context: ReferenceContext {
                    namespace: namespace.into(),
                    usage: if is_call.is_some() {
                        "call"
                    } else if access.mutate {
                        "mutate"
                    } else if access.write {
                        "write"
                    } else {
                        "read"
                    }
                    .into(),
                    access,
                },
                node_id: node_id.clone(),
                resolved: symbol_id.is_some(),
            });
            if let Some(node_id) = node_id {
                resolutions.push(Resolution {
                    node_id,
                    symbol_id: symbol_id.clone(),
                    resolved: symbol_id.is_some(),
                });
            }
            if symbol_id.is_none() {
                self.diagnostics.push(Diagnostic {
                    phase: "resolution".into(),
                    source: "tolk-resolver".into(),
                    severity: "warning".into(),
                    code: Some("unresolved-name".into()),
                    message: format!("unresolved {} name `{}`", namespace, usage.name),
                    location: Some(self.location(file_id, usage.span.start(), usage.span.end())),
                    help: None,
                    annotations: vec![],
                    fixes: vec![],
                });
            }
        }
        references.sort_by(|a, b| reference_key(a).cmp(&reference_key(b)));
        resolutions.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        resolutions.dedup_by(|a, b| a.node_id == b.node_id && a.symbol_id == b.symbol_id);
        let (program_calls, lambda_creations) = self.collect_program_calls();
        let mut call_sites = self.collect_program_call_sites(
            type_db,
            analysis_db,
            &resolved_by_span,
            &program_calls,
            &lambda_creations,
        );
        normalize_call_sites(&mut call_sites);
        let mut calls = call_sites
            .iter()
            .flat_map(|call_site| {
                call_site.targets.iter().map(|callee| CallEdge {
                    caller: call_site.caller.clone(),
                    callee: callee.clone(),
                    call_site: call_site.location.clone(),
                    node_id: call_site.node_id.clone(),
                    dispatch: call_site.dispatch.clone(),
                })
            })
            .collect::<Vec<_>>();
        calls.sort_by(|a, b| call_key(a).cmp(&call_key(b)));
        calls.dedup_by(|a, b| call_key(a) == call_key(b));
        (references, resolutions, call_sites, calls)
    }

    fn collect_program_calls(&self) -> (Vec<ProgramCall>, Vec<LambdaCreation>) {
        let mut calls = vec![];
        let mut creations = vec![];
        for file in self.project.files().values() {
            let Some(info) = self.file_db.get_by_id(file.id) else {
                continue;
            };
            for symbol in all_global_symbols(&file.decls) {
                if !self.is_callable_symbol(symbol.id) {
                    continue;
                }
                let Some(declaration) = info.find_syntax_declaration(symbol.id) else {
                    continue;
                };
                collect_owned_call_nodes(
                    declaration.syntax(),
                    file.id,
                    &self.symbol_ids[&symbol.id],
                    &self.lambda_ids,
                    &mut calls,
                    &mut creations,
                );
            }
        }
        calls.sort_by(|left, right| {
            (left.file_id, left.span.start, left.span.end, &left.caller).cmp(&(
                right.file_id,
                right.span.start,
                right.span.end,
                &right.caller,
            ))
        });
        calls.dedup_by(|left, right| {
            (left.file_id, left.span, &left.caller) == (right.file_id, right.span, &right.caller)
        });
        creations.sort_by(|left, right| {
            (&left.owner, left.file_id, left.span.start, &left.lambda).cmp(&(
                &right.owner,
                right.file_id,
                right.span.start,
                &right.lambda,
            ))
        });
        creations.dedup_by(|left, right| {
            left.owner == right.owner && left.lambda == right.lambda && left.span == right.span
        });
        (calls, creations)
    }

    fn callable_definitions(
        &self,
        type_db: &TypeDb<'_>,
        analysis_db: &mut AnalysisDb,
    ) -> BTreeMap<SymbolId, CallableDefinition> {
        let mut definitions = BTreeMap::new();
        for file in self.project.files().values() {
            for symbol in all_global_symbols(&file.decls) {
                if !self.is_callable_symbol(symbol.id) {
                    continue;
                }
                let Some(graph) = analysis_db.cfg_for_symbol(type_db, symbol.id) else {
                    continue;
                };
                let Some(info) = self.file_db.get_by_id(file.id) else {
                    continue;
                };
                let Some(declaration) = info.find_syntax_declaration(symbol.id) else {
                    continue;
                };
                let parameters = self.callable_parameters(file.id, declaration.syntax());
                let mut mutable_parameters =
                    self.callable_parameter_mutability(file.id, declaration.syntax());
                mutable_parameters.resize(parameters.len(), false);
                definitions.insert(
                    self.symbol_ids[&symbol.id].clone(),
                    CallableDefinition {
                        file_id: file.id,
                        acton_symbol: Some(symbol.id),
                        graph,
                        parameters,
                        mutable_parameters,
                        captures: vec![],
                    },
                );
            }
        }
        for (&(file_id, start, end), public_id) in &self.lambda_ids {
            let span = Span { start, end };
            let Some(file) = self.file_db.get_by_id(file_id) else {
                continue;
            };
            let Some(node) = file.find_node_at_span(span) else {
                continue;
            };
            let Some(graph) = self.lambda_cfg(file_id, span) else {
                continue;
            };
            definitions.insert(
                public_id.clone(),
                CallableDefinition {
                    file_id,
                    acton_symbol: None,
                    graph,
                    parameters: self.callable_parameters(file_id, node),
                    mutable_parameters: self.callable_parameter_mutability(file_id, node),
                    captures: self.lambda_captures(file_id, span),
                },
            );
        }
        definitions
    }

    fn callable_parameters(
        &self,
        file_id: FileId,
        declaration: Node<'_>,
    ) -> Vec<tolk_resolver::resolve_index::LocalDefId> {
        let Some(resolve) = self.project.get_resolved_uses(file_id) else {
            return vec![];
        };
        let Some(parameters) = declaration.child_by_field_name("parameters") else {
            return vec![];
        };
        let mut result = vec![];
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let Some(name) = parameter.child_by_field_name("name") else {
                continue;
            };
            if let Some(local) = resolve.find_local_at(name.start_byte()) {
                result.push(local.id);
            }
        }
        result
    }

    fn callable_parameter_mutability(&self, file_id: FileId, declaration: Node<'_>) -> Vec<bool> {
        let Some(parameters) = declaration.child_by_field_name("parameters") else {
            return vec![];
        };
        let mut cursor = parameters.walk();
        parameters
            .named_children(&mut cursor)
            .map(|parameter| {
                self.node_text(file_id, parameter)
                    .is_some_and(|text| text.trim_start().starts_with("mutate "))
            })
            .collect()
    }

    fn lambda_captures(
        &self,
        file_id: FileId,
        lambda_span: Span,
    ) -> Vec<tolk_resolver::resolve_index::LocalDefId> {
        let Some(resolve) = self.project.get_resolved_uses(file_id) else {
            return vec![];
        };
        let mut captures = resolve
            .uses
            .iter()
            .filter(|usage| {
                usage.span.start >= lambda_span.start && usage.span.end <= lambda_span.end
            })
            .filter_map(|usage| match usage.resolved {
                Resolved::Local(local) => Some(local),
                Resolved::Global(_) | Resolved::Unresolved => None,
            })
            .filter(|local| {
                resolve.find_local(*local).is_some_and(|definition| {
                    definition.def_span.start < lambda_span.start
                        || definition.def_span.end > lambda_span.end
                })
            })
            .collect::<Vec<_>>();
        captures.sort_by_key(|local| (local.file_id, local.local));
        captures.dedup();
        captures
    }

    fn lambda_cfg(&self, file_id: FileId, span: Span) -> Option<Arc<ActonControlFlowGraph>> {
        let file = self.file_db.get_by_id(file_id)?;
        let node = file.find_node_at_span(span)?;
        let lambda =
            <LambdaAsFunction<'_> as tolk_syntax::TryFromNode<'_>>::try_from_node(node).ok()?;
        let resolve = self.project.get_resolved_uses(file_id)?;
        build_cfg_for_function_with_source(&lambda, resolve, Some(&file.source().source))
            .map(Arc::new)
    }

    fn collect_program_call_sites(
        &self,
        type_db: &TypeDb<'_>,
        analysis_db: &mut AnalysisDb,
        resolved_by_span: &ResolutionsBySpan,
        calls: &[ProgramCall],
        lambda_creations: &[LambdaCreation],
    ) -> Vec<CallSite> {
        let definitions = self.callable_definitions(type_db, analysis_db);
        let mut calls_by_caller = HashMap::<SymbolId, Vec<ProgramCall>>::new();
        for call in calls {
            calls_by_caller
                .entry(call.caller.clone())
                .or_default()
                .push(call.clone());
        }
        let mut parameters = ParameterValues::new();
        let mut captures = CaptureValues::new();
        let mut returns = definitions
            .keys()
            .map(|symbol| (symbol.clone(), CallableValue::bottom()))
            .collect::<ReturnValues>();
        let mut mutations = definitions
            .iter()
            .map(|(symbol, definition)| {
                (
                    symbol.clone(),
                    definition
                        .mutable_parameters
                        .iter()
                        .map(|is_mutable| is_mutable.then(CallableValue::bottom))
                        .collect(),
                )
            })
            .collect::<MutationValues>();
        let mut call_sites = vec![];
        let max_iterations = definitions.len().saturating_mul(4).max(8);

        for _ in 0..max_iterations {
            let analysis = self.analyze_program_calls(
                resolved_by_span,
                &calls_by_caller,
                &definitions,
                &parameters,
                &returns,
                &mutations,
                &captures,
                lambda_creations,
                false,
            );
            let stable = analysis.parameters == parameters
                && analysis.returns == returns
                && analysis.mutations == mutations
                && analysis.captures == captures;
            parameters = analysis.parameters;
            returns = analysis.returns;
            mutations = analysis.mutations;
            captures = analysis.captures;
            call_sites = analysis.call_sites;
            if stable {
                break;
            }
        }
        // Parameters with no whole-program inputs are true external origins, not lattice
        // bottom. Re-run from the discovered internal summaries so that their uncertainty
        // propagates through callers without contaminating recursive fixed points.
        for _ in 0..max_iterations {
            let analysis = self.analyze_program_calls(
                resolved_by_span,
                &calls_by_caller,
                &definitions,
                &parameters,
                &returns,
                &mutations,
                &captures,
                lambda_creations,
                true,
            );
            let stable = analysis.parameters == parameters
                && analysis.returns == returns
                && analysis.mutations == mutations
                && analysis.captures == captures;
            parameters = analysis.parameters;
            returns = analysis.returns;
            mutations = analysis.mutations;
            captures = analysis.captures;
            call_sites = analysis.call_sites;
            if stable {
                break;
            }
        }
        call_sites
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_program_calls(
        &self,
        resolved_by_span: &ResolutionsBySpan,
        calls_by_caller: &HashMap<SymbolId, Vec<ProgramCall>>,
        definitions: &BTreeMap<SymbolId, CallableDefinition>,
        parameter_values: &ParameterValues,
        return_values: &ReturnValues,
        mutation_values: &MutationValues,
        capture_values: &CaptureValues,
        lambda_creations: &[LambdaCreation],
        external_unknown: bool,
    ) -> ProgramAnalysis {
        let mut result = ProgramAnalysis {
            parameters: definitions
                .iter()
                .map(|(id, definition)| (id.clone(), vec![None; definition.parameters.len()]))
                .collect(),
            mutations: definitions
                .iter()
                .map(|(symbol, definition)| {
                    (
                        symbol.clone(),
                        vec![None; definition.mutable_parameters.len()],
                    )
                })
                .collect(),
            ..Default::default()
        };

        for (public_id, definition) in definitions {
            let graph = definition.graph.as_ref();
            let mut initial = graph
                .all_locals()
                .into_iter()
                .map(|local| (local, CallableValue::default()))
                .collect::<CallableState>();
            let values = parameter_values.get(public_id);
            for (index, &parameter) in definition.parameters.iter().enumerate() {
                initial.insert(
                    parameter,
                    values
                        .and_then(|values| values.get(index))
                        .and_then(Clone::clone)
                        .unwrap_or_else(|| {
                            if external_unknown {
                                CallableValue::default()
                            } else {
                                CallableValue::bottom()
                            }
                        }),
                );
            }
            for &capture in &definition.captures {
                initial.insert(
                    capture,
                    capture_values
                        .get(public_id)
                        .and_then(|values| values.get(&capture))
                        .cloned()
                        .unwrap_or_else(|| {
                            if external_unknown {
                                CallableValue::default()
                            } else {
                                CallableValue::bottom()
                            }
                        }),
                );
            }
            let states = self.callable_states(
                definition.file_id,
                graph,
                resolved_by_span,
                initial,
                return_values,
                mutation_values,
            );
            if let Some(exit_state) = states[graph.exit().index()].as_ref()
                && let Some(outputs) = result.mutations.get_mut(public_id)
            {
                for (index, (&parameter, &is_mutable)) in definition
                    .parameters
                    .iter()
                    .zip(&definition.mutable_parameters)
                    .enumerate()
                {
                    if is_mutable {
                        outputs[index] = Some(
                            exit_state
                                .get(&parameter)
                                .cloned()
                                .unwrap_or_else(CallableValue::default),
                        );
                    }
                }
            }
            result.returns.insert(
                public_id.clone(),
                self.callable_return_value(
                    definition.file_id,
                    graph,
                    &states,
                    resolved_by_span,
                    return_values,
                ),
            );

            for creation in lambda_creations
                .iter()
                .filter(|creation| creation.owner == *public_id)
            {
                let Some(flow_node) = containing_flow_node(graph, creation.span) else {
                    continue;
                };
                let Some(state) = states[flow_node.id.index()].as_ref() else {
                    continue;
                };
                let Some(lambda) = definitions.get(&creation.lambda) else {
                    continue;
                };
                for &capture in &lambda.captures {
                    let value = state.get(&capture).cloned().unwrap_or_else(|| {
                        if external_unknown {
                            CallableValue::default()
                        } else {
                            CallableValue::bottom()
                        }
                    });
                    let values = result.captures.entry(creation.lambda.clone()).or_default();
                    let current = values.entry(capture).or_insert_with(CallableValue::bottom);
                    *current = merged_callable_value(current, &value);
                }
            }

            for call in calls_by_caller.get(public_id).into_iter().flatten() {
                let Some(flow_node) = containing_flow_node(graph, call.span) else {
                    continue;
                };
                let Some(state) = states[flow_node.id.index()].as_ref() else {
                    continue;
                };
                let Some(file) = self.file_db.get_by_id(call.file_id) else {
                    continue;
                };
                let Some(call_node) = file.find_node_at_span(call.span) else {
                    continue;
                };
                let Some(callee) = call_node.child_by_field_name("callee") else {
                    continue;
                };
                let direct = self.direct_callable_target(call.file_id, callee, resolved_by_span);
                let value = direct.as_ref().map_or_else(
                    || {
                        self.callable_value(
                            call.file_id,
                            callee,
                            state,
                            resolved_by_span,
                            return_values,
                        )
                    },
                    |target| CallableValue::target(target.clone()),
                );
                let dispatch = if direct.is_some() {
                    "direct"
                } else {
                    "indirect"
                };
                result.call_sites.push(CallSite {
                    caller: public_id.clone(),
                    location: self.location(call.file_id, call.span.start(), call.span.end()),
                    node_id: self.exact_node(call.file_id, call.span),
                    dispatch: dispatch.into(),
                    targets: value.targets.iter().cloned().collect(),
                    complete: !value.bottom && value.complete,
                });

                let arguments = self.call_argument_values(
                    call.file_id,
                    call_node,
                    state,
                    resolved_by_span,
                    return_values,
                    direct.as_deref(),
                    definitions,
                );
                for target in &value.targets {
                    let Some(slots) = result.parameters.get_mut(target) else {
                        continue;
                    };
                    for (slot, argument) in slots.iter_mut().zip(&arguments) {
                        merge_optional_callable_value(slot, argument);
                    }
                }
            }
        }
        result
    }

    fn callable_return_value(
        &self,
        file_id: FileId,
        graph: &ActonControlFlowGraph,
        states: &[Option<CallableState>],
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
    ) -> CallableValue {
        let Some(file) = self.file_db.get_by_id(file_id) else {
            return CallableValue::default();
        };
        let mut returned = None;
        let mut has_return = false;
        for flow_node in graph
            .nodes()
            .iter()
            .filter(|node| node.kind == ActonFlowNodeKind::Return)
        {
            let Some(state) = states[flow_node.id.index()].as_ref() else {
                continue;
            };
            has_return = true;
            let value = flow_node
                .span
                .and_then(|span| file.find_node_at_span(span))
                .and_then(|node| node.child_by_field_name("body"))
                .map(|expression| {
                    self.callable_value(file_id, expression, state, resolved_by_span, return_values)
                })
                .unwrap_or_else(CallableValue::non_callable);
            merge_optional_callable_value(&mut returned, &value);
        }
        if has_return {
            returned.unwrap_or_else(CallableValue::non_callable)
        } else {
            CallableValue::non_callable()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call_argument_values(
        &self,
        file_id: FileId,
        call: Node<'_>,
        state: &CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
        direct: Option<&str>,
        definitions: &BTreeMap<SymbolId, CallableDefinition>,
    ) -> Vec<CallableValue> {
        let mut values = vec![];
        if let Some(target) = direct
            .and_then(|target| definitions.get(target))
            .and_then(|definition| definition.acton_symbol)
            .and_then(|symbol| self.project.resolve_symbol(symbol))
            && matches!(
                target.kind,
                SymbolKind::Method {
                    is_instance: true,
                    ..
                }
            )
            && let Some(receiver) = call
                .child_by_field_name("callee")
                .and_then(|callee| callee.child_by_field_name("obj"))
        {
            values.push(self.callable_value(
                file_id,
                receiver,
                state,
                resolved_by_span,
                return_values,
            ));
        }
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return values;
        };
        let mut cursor = arguments.walk();
        for argument in arguments.named_children(&mut cursor) {
            let Some(expression) = argument.child_by_field_name("expr") else {
                continue;
            };
            values.push(self.callable_value(
                file_id,
                expression,
                state,
                resolved_by_span,
                return_values,
            ));
        }
        values
    }

    fn callable_states(
        &self,
        file_id: FileId,
        graph: &ActonControlFlowGraph,
        resolved_by_span: &ResolutionsBySpan,
        initial: CallableState,
        return_values: &ReturnValues,
        mutation_values: &MutationValues,
    ) -> Vec<Option<CallableState>> {
        let mut incoming = vec![None; graph.node_count()];
        incoming[graph.entry().index()] = Some(initial);
        let mut work = VecDeque::from([graph.entry()]);

        while let Some(node_id) = work.pop_front() {
            let Some(state) = incoming[node_id.index()].clone() else {
                continue;
            };
            let next = self.transfer_callable_state(
                file_id,
                graph.node(node_id),
                &state,
                resolved_by_span,
                return_values,
                mutation_values,
            );

            for edge in graph.successors(node_id) {
                // If evaluating an assignment throws, its new value was never stored.
                let propagated = if edge.kind == ActonEdgeKind::Exceptional {
                    &state
                } else {
                    &next
                };
                let successor = edge.to.index();
                let changed = if let Some(existing) = incoming[successor].as_mut() {
                    merge_callable_states(existing, propagated)
                } else {
                    incoming[successor] = Some(propagated.clone());
                    true
                };
                if changed {
                    work.push_back(edge.to);
                }
            }
        }
        incoming
    }

    fn transfer_callable_state(
        &self,
        file_id: FileId,
        flow_node: &tolk_dataflow::FlowNode,
        incoming: &CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
        mutation_values: &MutationValues,
    ) -> CallableState {
        let mut outgoing = incoming.clone();
        let Some(span) = flow_node.span else {
            return outgoing;
        };
        let file = self.file_db.get_by_id(file_id);
        let syntax = file.as_ref().and_then(|file| file.find_node_at_span(span));

        let assignment = syntax.filter(|node| node.kind() == "assignment");
        let assigned = assignment.and_then(|node| {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            let path = self.local_assignment_target(file_id, left, flow_node, resolved_by_span)?;
            let value =
                self.callable_value(file_id, right, incoming, resolved_by_span, return_values);
            Some((path, value))
        });

        // Any unmodelled write may replace a callable value, so invalidate it first.
        for local in &flow_node.writes {
            outgoing.insert(*local, CallableValue::default());
        }
        if let Some(syntax) = syntax {
            self.apply_collection_mutations(
                file_id,
                syntax,
                &mut outgoing,
                resolved_by_span,
                return_values,
            );
            self.apply_interprocedural_mutations(
                file_id,
                syntax,
                &mut outgoing,
                resolved_by_span,
                return_values,
                mutation_values,
            );
        }
        if let Some((path, value)) = assigned {
            if path.members.is_empty() {
                outgoing.insert(path.root, value);
            } else {
                let mut root = incoming.get(&path.root).cloned().unwrap_or_default();
                root.set_member_path(&path.members, value);
                outgoing.insert(path.root, root);
            }
        }
        outgoing
    }

    fn apply_collection_mutations(
        &self,
        file_id: FileId,
        node: Node<'_>,
        state: &mut CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
    ) {
        if node.kind() == "lambda_expression" {
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.apply_collection_mutations(file_id, child, state, resolved_by_span, return_values);
        }
        if node.kind() != "function_call" {
            return;
        }
        let Some(callee) = node.child_by_field_name("callee") else {
            return;
        };
        let callee = if callee.kind() == "generic_instantiation" {
            let Some(inner) = callee.child_by_field_name("expr") else {
                return;
            };
            inner
        } else {
            callee
        };
        if callee.kind() != "dot_access" {
            return;
        }
        let Some(receiver) = callee.child_by_field_name("obj") else {
            return;
        };
        let Some(kind) = self.collection_kind(file_id, receiver) else {
            return;
        };
        let Some(method) = callee
            .child_by_field_name("field")
            .and_then(|field| self.node_text(file_id, field))
        else {
            return;
        };
        let Some(path) = self.local_value_path(file_id, receiver, resolved_by_span) else {
            return;
        };
        let arguments = Self::call_arguments(node);
        let mut collection =
            self.callable_value(file_id, receiver, state, resolved_by_span, return_values);
        let changed = match (kind, method.as_str()) {
            (CollectionKind::Array, "push") => {
                let Some(value) = arguments.first() else {
                    return;
                };
                let value =
                    self.callable_value(file_id, *value, state, resolved_by_span, return_values);
                collection.push_array_member(value);
                true
            }
            (CollectionKind::Array, "set") => {
                let (Some(value), Some(index)) = (arguments.first(), arguments.get(1)) else {
                    return;
                };
                let value =
                    self.callable_value(file_id, *value, state, resolved_by_span, return_values);
                if let Some(key) = self.collection_key(file_id, *index, resolved_by_span) {
                    collection.members.insert(key, value);
                } else {
                    collection.set_dynamic_member(value);
                }
                true
            }
            (CollectionKind::Array, "pop") => {
                if let Some(lengths) = collection.array_lengths.take() {
                    collection.array_lengths = Some(
                        lengths
                            .into_iter()
                            .map(|length| length.saturating_sub(1))
                            .collect(),
                    );
                }
                true
            }
            (
                CollectionKind::Map,
                "set"
                | "setAndGetPrevious"
                | "replaceIfExists"
                | "replaceAndGetPrevious"
                | "addIfNotExists"
                | "addOrGetExisting",
            ) => {
                let (Some(key), Some(value)) = (arguments.first(), arguments.get(1)) else {
                    return;
                };
                let value =
                    self.callable_value(file_id, *value, state, resolved_by_span, return_values);
                if let Some(key) = self.collection_key(file_id, *key, resolved_by_span) {
                    if method == "set" || method == "setAndGetPrevious" {
                        collection.members.insert(key, value);
                    } else {
                        let current = collection
                            .members
                            .entry(key)
                            .or_insert_with(CallableValue::bottom);
                        *current = merged_callable_value(current, &value);
                    }
                } else {
                    collection.set_dynamic_member(value);
                }
                true
            }
            _ => false,
        };
        if changed {
            let mut root = state.get(&path.root).cloned().unwrap_or_default();
            if path.members.is_empty() {
                root = collection;
            } else {
                root.set_member_path(&path.members, collection);
            }
            state.insert(path.root, root);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_interprocedural_mutations(
        &self,
        file_id: FileId,
        node: Node<'_>,
        state: &mut CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
        mutation_values: &MutationValues,
    ) {
        if node.kind() == "lambda_expression" {
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.apply_interprocedural_mutations(
                file_id,
                child,
                state,
                resolved_by_span,
                return_values,
                mutation_values,
            );
        }
        if node.kind() != "function_call" {
            return;
        }
        let Some(callee) = node.child_by_field_name("callee") else {
            return;
        };
        let direct = self.direct_callable_target(file_id, callee, resolved_by_span);
        let called = direct.map_or_else(
            || self.callable_value(file_id, callee, state, resolved_by_span, return_values),
            CallableValue::target,
        );
        let arguments = Self::call_arguments(node);
        let receiver = {
            let unwrapped = if callee.kind() == "generic_instantiation" {
                callee.child_by_field_name("expr")
            } else {
                Some(callee)
            };
            unwrapped
                .filter(|callee| callee.kind() == "dot_access")
                .and_then(|callee| callee.child_by_field_name("obj"))
        };
        let mut updates = Vec::<(LocalPath, CallableValue)>::new();
        for target in &called.targets {
            let Some(outputs) = mutation_values.get(target) else {
                continue;
            };
            let mut actuals = arguments.clone();
            if outputs.len() == actuals.len().saturating_add(1)
                && let Some(receiver) = receiver
            {
                actuals.insert(0, receiver);
            }
            for (actual, output) in actuals.into_iter().zip(outputs) {
                let Some(output) = output else {
                    continue;
                };
                if output.bottom {
                    continue;
                }
                let Some(path) = self.local_value_path(file_id, actual, resolved_by_span) else {
                    continue;
                };
                if let Some((_, current)) = updates.iter_mut().find(|(candidate, _)| {
                    candidate.root == path.root && candidate.members == path.members
                }) {
                    *current = merged_callable_value(current, output);
                } else {
                    updates.push((path, output.clone()));
                }
            }
        }
        for (path, value) in updates {
            if path.members.is_empty() {
                state.insert(path.root, value);
            } else {
                let mut root = state.get(&path.root).cloned().unwrap_or_default();
                root.set_member_path(&path.members, value);
                state.insert(path.root, root);
            }
        }
    }

    fn local_value_path(
        &self,
        file_id: FileId,
        node: Node<'_>,
        resolved_by_span: &ResolutionsBySpan,
    ) -> Option<LocalPath> {
        match node.kind() {
            "identifier" => match self.resolved_node(file_id, node, resolved_by_span)? {
                Resolved::Local(root) => Some(LocalPath {
                    root: *root,
                    members: vec![],
                }),
                Resolved::Global(_) | Resolved::Unresolved => None,
            },
            "parenthesized_expression" => self.local_value_path(
                file_id,
                node.child_by_field_name("inner")?,
                resolved_by_span,
            ),
            "dot_access" => {
                let mut path = self.local_value_path(
                    file_id,
                    node.child_by_field_name("obj")?,
                    resolved_by_span,
                )?;
                path.members
                    .push(self.node_text(file_id, node.child_by_field_name("field")?)?);
                Some(path)
            }
            _ => None,
        }
    }

    fn local_assignment_target(
        &self,
        file_id: FileId,
        node: Node<'_>,
        flow_node: &tolk_dataflow::FlowNode,
        resolved_by_span: &ResolutionsBySpan,
    ) -> Option<LocalPath> {
        match node.kind() {
            "identifier" => match self.resolved_node(file_id, node, resolved_by_span)? {
                Resolved::Local(local) => Some(LocalPath {
                    root: *local,
                    members: vec![],
                }),
                Resolved::Global(_) | Resolved::Unresolved => None,
            },
            "parenthesized_expression" => self.local_assignment_target(
                file_id,
                node.child_by_field_name("inner")?,
                flow_node,
                resolved_by_span,
            ),
            "var_declaration_lhs" if flow_node.writes.len() == 1 => flow_node
                .writes
                .iter()
                .next()
                .copied()
                .map(|root| LocalPath {
                    root,
                    members: vec![],
                }),
            "dot_access" => {
                let mut path = self.local_assignment_target(
                    file_id,
                    node.child_by_field_name("obj")?,
                    flow_node,
                    resolved_by_span,
                )?;
                path.members
                    .push(self.node_text(file_id, node.child_by_field_name("field")?)?);
                Some(path)
            }
            _ => None,
        }
    }

    fn callable_value(
        &self,
        file_id: FileId,
        node: Node<'_>,
        state: &CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
    ) -> CallableValue {
        self.callable_value_depth(file_id, node, state, resolved_by_span, return_values, 0)
    }

    fn collection_kind(&self, file_id: FileId, node: Node<'_>) -> Option<CollectionKind> {
        self.collection_kinds
            .get(&(file_id, node.start_byte() as u32, node.end_byte() as u32))
            .copied()
    }

    fn collection_key(
        &self,
        file_id: FileId,
        node: Node<'_>,
        resolved_by_span: &ResolutionsBySpan,
    ) -> Option<String> {
        match node.kind() {
            "number_literal" => {
                let text = self.node_text(file_id, node)?;
                let parsed = tolk_syntax::ast::expressions::parse_tolk_int_literal(&text)?;
                parsed
                    .parse_u64()
                    .map(|value| format!("int:{value}"))
                    .or_else(|| Some(format!("int:{text}")))
            }
            "string_literal" => self
                .node_text(file_id, node)
                .map(|text| format!("string:{}", text.trim_matches('"'))),
            "boolean_literal" => self
                .node_text(file_id, node)
                .map(|text| format!("bool:{text}")),
            "identifier" | "dot_access" => {
                let resolved_node = if node.kind() == "dot_access" {
                    node.child_by_field_name("field").unwrap_or(node)
                } else {
                    node
                };
                match self.resolved_node(file_id, resolved_node, resolved_by_span) {
                    Some(Resolved::Global(symbol)) => self.constant_keys.get(symbol).cloned(),
                    Some(Resolved::Local(_)) | Some(Resolved::Unresolved) | None => None,
                }
            }
            "parenthesized_expression" | "not_null_operator" => self.collection_key(
                file_id,
                node.child_by_field_name("inner")?,
                resolved_by_span,
            ),
            "cast_as_operator" => {
                self.collection_key(file_id, node.child_by_field_name("expr")?, resolved_by_span)
            }
            _ => None,
        }
    }

    fn collection_lookup(
        &self,
        file_id: FileId,
        collection: &CallableValue,
        key: Option<Node<'_>>,
        resolved_by_span: &ResolutionsBySpan,
    ) -> CallableValue {
        key.and_then(|key| self.collection_key(file_id, key, resolved_by_span))
            .map_or_else(|| collection.any_member(), |key| collection.member(&key))
    }

    fn map_lookup_result(value: CallableValue) -> CallableValue {
        let mut result = CallableValue::non_callable();
        result.members_complete = true;
        result.members.insert(MAP_VALUE_MEMBER.into(), value);
        result
    }

    fn call_arguments<'tree>(call: Node<'tree>) -> Vec<Node<'tree>> {
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return vec![];
        };
        let mut cursor = arguments.walk();
        arguments
            .named_children(&mut cursor)
            .filter_map(|argument| argument.child_by_field_name("expr"))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn collection_call_value(
        &self,
        file_id: FileId,
        call: Node<'_>,
        state: &CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
        depth: usize,
    ) -> Option<CallableValue> {
        let callee = call.child_by_field_name("callee")?;
        let callee = if callee.kind() == "generic_instantiation" {
            callee.child_by_field_name("expr")?
        } else {
            callee
        };
        if callee.kind() != "dot_access" {
            return None;
        }
        let receiver = callee.child_by_field_name("obj")?;
        let method = self.node_text(file_id, callee.child_by_field_name("field")?)?;
        let collection = self.callable_value_depth(
            file_id,
            receiver,
            state,
            resolved_by_span,
            return_values,
            depth + 1,
        );

        if method == "loadValue" && collection.members.contains_key(MAP_VALUE_MEMBER) {
            return Some(collection.member(MAP_VALUE_MEMBER));
        }

        let kind = self.collection_kind(file_id, receiver)?;
        let arguments = Self::call_arguments(call);
        match (kind, method.as_str()) {
            (CollectionKind::Array, "get") => Some(self.collection_lookup(
                file_id,
                &collection,
                arguments.first().copied(),
                resolved_by_span,
            )),
            (CollectionKind::Array, "first") => Some(collection.member("int:0")),
            (CollectionKind::Array, "last" | "pop") => {
                let value = collection.array_lengths.as_ref().map_or_else(
                    || collection.any_member(),
                    |lengths| {
                        let mut value = CallableValue::bottom();
                        for length in lengths.iter().filter(|length| **length > 0) {
                            value = merged_callable_value(
                                &value,
                                &collection.member(&format!("int:{}", length - 1)),
                            );
                        }
                        value
                    },
                );
                Some(value)
            }
            (CollectionKind::Map, "get") => Some(Self::map_lookup_result(self.collection_lookup(
                file_id,
                &collection,
                arguments.first().copied(),
                resolved_by_span,
            ))),
            (CollectionKind::Map, "mustGet") => Some(self.collection_lookup(
                file_id,
                &collection,
                arguments.first().copied(),
                resolved_by_span,
            )),
            (CollectionKind::Map, "set") => {
                let mut updated = collection;
                if let Some(value) = arguments.get(1) {
                    let value = self.callable_value_depth(
                        file_id,
                        *value,
                        state,
                        resolved_by_span,
                        return_values,
                        depth + 1,
                    );
                    if let Some(key) = arguments
                        .first()
                        .and_then(|key| self.collection_key(file_id, *key, resolved_by_span))
                    {
                        updated.members.insert(key, value);
                    } else {
                        updated.set_dynamic_member(value);
                    }
                }
                Some(updated)
            }
            _ => None,
        }
    }

    fn callable_value_depth(
        &self,
        file_id: FileId,
        node: Node<'_>,
        state: &CallableState,
        resolved_by_span: &ResolutionsBySpan,
        return_values: &ReturnValues,
        depth: usize,
    ) -> CallableValue {
        if depth >= 12 {
            return CallableValue::default();
        }
        if let Some(target) = self.direct_callable_target(file_id, node, resolved_by_span) {
            return CallableValue::target(target);
        }
        match node.kind() {
            "identifier" => match self.resolved_node(file_id, node, resolved_by_span) {
                Some(Resolved::Local(local)) => state.get(local).cloned().unwrap_or_default(),
                Some(Resolved::Global(_)) | Some(Resolved::Unresolved) | None => {
                    CallableValue::default()
                }
            },
            "lambda_expression" => self
                .lambda_ids
                .get(&(file_id, node.start_byte() as u32, node.end_byte() as u32))
                .cloned()
                .map(CallableValue::target)
                .unwrap_or_default(),
            "function_call" => {
                if let Some(value) = self.collection_call_value(
                    file_id,
                    node,
                    state,
                    resolved_by_span,
                    return_values,
                    depth,
                ) {
                    return value;
                }
                let Some(callee) = node.child_by_field_name("callee") else {
                    return CallableValue::default();
                };
                let called = self.callable_value_depth(
                    file_id,
                    callee,
                    state,
                    resolved_by_span,
                    return_values,
                    depth + 1,
                );
                if called.bottom {
                    return CallableValue::bottom();
                }
                let mut returned = None;
                for target in &called.targets {
                    let value = return_values.get(target).cloned().unwrap_or_default();
                    merge_optional_callable_value(&mut returned, &value);
                }
                let mut returned = returned.unwrap_or_default();
                returned.complete &= called.complete;
                returned.members_complete &= called.complete;
                returned.limit_member_depth(12 - depth);
                returned
            }
            "dot_access" => {
                let Some(object) = node.child_by_field_name("obj") else {
                    return CallableValue::default();
                };
                let Some(field) = node.child_by_field_name("field") else {
                    return CallableValue::default();
                };
                let object = self.callable_value_depth(
                    file_id,
                    object,
                    state,
                    resolved_by_span,
                    return_values,
                    depth + 1,
                );
                let key = self.node_text(file_id, field).unwrap_or_default();
                object.member(&key)
            }
            "generic_instantiation" => node
                .child_by_field_name("expr")
                .map(|inner| {
                    self.callable_value_depth(
                        file_id,
                        inner,
                        state,
                        resolved_by_span,
                        return_values,
                        depth + 1,
                    )
                })
                .unwrap_or_default(),
            "parenthesized_expression" | "not_null_operator" => node
                .child_by_field_name("inner")
                .map(|inner| {
                    self.callable_value_depth(
                        file_id,
                        inner,
                        state,
                        resolved_by_span,
                        return_values,
                        depth + 1,
                    )
                })
                .unwrap_or_default(),
            "cast_as_operator" => node
                .child_by_field_name("expr")
                .map(|inner| {
                    self.callable_value_depth(
                        file_id,
                        inner,
                        state,
                        resolved_by_span,
                        return_values,
                        depth + 1,
                    )
                })
                .unwrap_or_default(),
            "ternary_operator" => {
                let consequence = node
                    .child_by_field_name("consequence")
                    .map(|branch| {
                        self.callable_value_depth(
                            file_id,
                            branch,
                            state,
                            resolved_by_span,
                            return_values,
                            depth + 1,
                        )
                    })
                    .unwrap_or_default();
                let alternative = node
                    .child_by_field_name("alternative")
                    .map(|branch| {
                        self.callable_value_depth(
                            file_id,
                            branch,
                            state,
                            resolved_by_span,
                            return_values,
                            depth + 1,
                        )
                    })
                    .unwrap_or_default();
                merged_callable_value(&consequence, &alternative)
            }
            "tensor_expression" | "typed_tuple" => {
                let mut value = CallableValue::non_callable();
                value.members_complete = true;
                let mut cursor = node.walk();
                let elements = node.named_children(&mut cursor).collect::<Vec<_>>();
                let is_array = node.kind() == "typed_tuple"
                    && self.collection_kind(file_id, node) == Some(CollectionKind::Array);
                if is_array {
                    value.array_lengths = Some(BTreeSet::from([elements.len()]));
                }
                for (index, element) in elements.into_iter().enumerate() {
                    value.members.insert(
                        if is_array {
                            format!("int:{index}")
                        } else {
                            index.to_string()
                        },
                        self.callable_value_depth(
                            file_id,
                            element,
                            state,
                            resolved_by_span,
                            return_values,
                            depth + 1,
                        ),
                    );
                }
                value
            }
            "object_literal" => {
                let mut value = CallableValue::non_callable();
                value.members_complete = true;
                let Some(arguments) = node.child_by_field_name("arguments") else {
                    return value;
                };
                let mut cursor = arguments.walk();
                for argument in arguments.named_children(&mut cursor) {
                    let Some(name) = argument.child_by_field_name("name") else {
                        continue;
                    };
                    let key = self.node_text(file_id, name).unwrap_or_default();
                    let member = argument
                        .child_by_field_name("value")
                        .map(|member| {
                            self.callable_value_depth(
                                file_id,
                                member,
                                state,
                                resolved_by_span,
                                return_values,
                                depth + 1,
                            )
                        })
                        .unwrap_or_default();
                    value.members.insert(key, member);
                }
                value
            }
            "number_literal" | "string_literal" | "boolean_literal" | "null_literal" => {
                CallableValue::non_callable()
            }
            _ => CallableValue::default(),
        }
    }

    fn direct_callable_target(
        &self,
        file_id: FileId,
        node: Node<'_>,
        resolved_by_span: &ResolutionsBySpan,
    ) -> Option<SymbolId> {
        match node.kind() {
            "identifier" => match self.resolved_node(file_id, node, resolved_by_span)? {
                Resolved::Global(symbol) if self.is_callable_symbol(*symbol) => {
                    self.symbol_ids.get(symbol).cloned()
                }
                Resolved::Global(_) | Resolved::Local(_) | Resolved::Unresolved => None,
            },
            "dot_access" => {
                let field = node.child_by_field_name("field")?;
                match self.resolved_node(file_id, field, resolved_by_span)? {
                    Resolved::Global(symbol) if self.is_callable_symbol(*symbol) => {
                        self.symbol_ids.get(symbol).cloned()
                    }
                    Resolved::Global(_) | Resolved::Local(_) | Resolved::Unresolved => None,
                }
            }
            "generic_instantiation" | "cast_as_operator" => self.direct_callable_target(
                file_id,
                node.child_by_field_name("expr")?,
                resolved_by_span,
            ),
            "parenthesized_expression" | "not_null_operator" => self.direct_callable_target(
                file_id,
                node.child_by_field_name("inner")?,
                resolved_by_span,
            ),
            _ => None,
        }
    }

    fn node_text(&self, file_id: FileId, node: Node<'_>) -> Option<String> {
        self.file_db
            .get_by_id(file_id)?
            .source()
            .source
            .get(node.start_byte()..node.end_byte())
            .map(str::to_owned)
    }

    fn resolved_node<'b>(
        &self,
        file_id: FileId,
        node: Node<'_>,
        resolved_by_span: &'b ResolutionsBySpan,
    ) -> Option<&'b Resolved> {
        resolved_by_span.get(&(file_id, node.start_byte() as u32, node.end_byte() as u32))
    }

    fn is_callable_symbol(&self, symbol: tolk_resolver::SymbolId) -> bool {
        self.project.resolve_symbol(symbol).is_some_and(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Function { .. }
                    | SymbolKind::Method { .. }
                    | SymbolKind::GetMethod { .. }
            )
        })
    }

    fn collect_lint_diagnostics(
        &mut self,
        diagnostics: Vec<tolk_linter::diagnostic::Diagnostic>,
        workspace_file_ids: &std::collections::HashSet<FileId>,
    ) {
        let converted = diagnostics
            .into_iter()
            .filter(|diagnostic| workspace_file_ids.contains(&diagnostic.file_id))
            .map(|diagnostic| {
                let location = diagnostic
                    .annotations
                    .iter()
                    .find(|annotation| annotation.is_primary)
                    .or_else(|| diagnostic.annotations.first())
                    .map(|annotation| {
                        self.location(
                            diagnostic.file_id,
                            annotation.span.start(),
                            annotation.span.end(),
                        )
                    });
                let annotations = diagnostic
                    .annotations
                    .into_iter()
                    .map(|annotation| DiagnosticAnnotation {
                        location: self.location(
                            diagnostic.file_id,
                            annotation.span.start(),
                            annotation.span.end(),
                        ),
                        message: annotation.message,
                        primary: annotation.is_primary,
                        tags: annotation
                            .tags
                            .into_iter()
                            .map(|tag| match tag {
                                LintDiagnosticTag::Unnecessary => "unnecessary",
                                LintDiagnosticTag::Deprecated => "deprecated",
                            })
                            .map(str::to_owned)
                            .collect(),
                    })
                    .collect();
                let fixes = diagnostic
                    .fixes
                    .into_iter()
                    .map(|fix| DiagnosticFix {
                        message: fix.message,
                        applicability: match fix.applicability {
                            LintApplicability::Auto => "automatic",
                            LintApplicability::Manual => "manual",
                        }
                        .into(),
                        edits: fix
                            .edits
                            .into_iter()
                            .map(|edit| DiagnosticEdit {
                                location: self.location(
                                    edit.file_id,
                                    edit.span.start(),
                                    edit.span.end(),
                                ),
                                replacement: edit.replacement,
                            })
                            .collect(),
                    })
                    .collect();
                Diagnostic {
                    phase: "lint".into(),
                    source: "tolk-linter".into(),
                    severity: match diagnostic.severity {
                        LintSeverity::Fatal | LintSeverity::Error => "error",
                        LintSeverity::Warning => "warning",
                        LintSeverity::Info => "information",
                        LintSeverity::Help => "hint",
                    }
                    .into(),
                    code: diagnostic.code,
                    message: diagnostic.message,
                    location,
                    help: diagnostic.help,
                    annotations,
                    fixes,
                }
            })
            .collect::<Vec<_>>();
        self.diagnostics.extend(converted);
    }

    fn collect_constant_values(
        &self,
        files: &[Arc<tolk_resolver::FileIndex>],
    ) -> Vec<SymbolConstantValue> {
        let mut evaluator = ConstantEvaluator::new(self);
        let mut values = vec![];
        for file in files {
            for symbol in all_global_symbols(&file.decls) {
                let evaluated = match symbol.kind {
                    SymbolKind::Constant => evaluator.evaluate_constant(symbol.id),
                    SymbolKind::EnumMember => evaluator.evaluate_enum_member(symbol.id),
                    _ => continue,
                };
                let Some(symbol_id) = self.symbol_ids.get(&symbol.id).cloned() else {
                    continue;
                };
                values.push(SymbolConstantValue {
                    symbol_id,
                    value: constant_value(evaluated),
                });
            }
        }
        values.sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
        values
    }

    fn collect_constant_keys(&self, files: &[Arc<tolk_resolver::FileIndex>]) -> ConstantKeys {
        let mut evaluator = ConstantEvaluator::new(self);
        let mut values = ConstantKeys::new();
        for file in files {
            for symbol in all_global_symbols(&file.decls) {
                let evaluated = match symbol.kind {
                    SymbolKind::Constant => evaluator.evaluate_constant(symbol.id),
                    SymbolKind::EnumMember => evaluator.evaluate_enum_member(symbol.id),
                    _ => continue,
                };
                if let Some(key) = constant_collection_key(&evaluated) {
                    values.insert(symbol.id, key);
                }
            }
        }
        values
    }

    fn collect_control_flow_graphs(
        &self,
        analysis_db: &mut AnalysisDb,
        type_db: &TypeDb<'_>,
        files: &[Arc<tolk_resolver::FileIndex>],
    ) -> Vec<ControlFlowGraph> {
        if self.control_flow == ControlFlowScope::None {
            return vec![];
        }

        let mut graphs = vec![];
        for file in files {
            if self.control_flow == ControlFlowScope::Workspace
                && !matches!(
                    file.source_kind,
                    tolk_resolver::file_index::FileSource::Workspace
                )
            {
                continue;
            }
            for symbol in all_global_symbols(&file.decls) {
                if !matches!(
                    symbol.kind,
                    SymbolKind::Function { .. }
                        | SymbolKind::Method { .. }
                        | SymbolKind::GetMethod { .. }
                ) {
                    continue;
                }
                let Some(public_symbol_id) = self.symbol_ids.get(&symbol.id).cloned() else {
                    continue;
                };
                let Some(graph) = analysis_db.cfg_for_symbol(type_db, symbol.id) else {
                    continue;
                };
                graphs.push(self.convert_control_flow_graph(
                    file.id,
                    public_symbol_id,
                    graph.as_ref(),
                ));
            }
            for (&(file_id, start, end), public_symbol_id) in &self.lambda_ids {
                if file_id != file.id {
                    continue;
                }
                let Some(graph) = self.lambda_cfg(file_id, Span { start, end }) else {
                    continue;
                };
                graphs.push(self.convert_control_flow_graph(
                    file_id,
                    public_symbol_id.clone(),
                    graph.as_ref(),
                ));
            }
        }
        graphs.sort_by(|left, right| left.symbol_id.cmp(&right.symbol_id));
        graphs
    }

    fn convert_control_flow_graph(
        &self,
        file_id: FileId,
        symbol_id: SymbolId,
        graph: &ActonControlFlowGraph,
    ) -> ControlFlowGraph {
        let node_id = |index: usize| format!("cfg:{symbol_id}:n:{index}");
        let nodes = graph
            .nodes()
            .iter()
            .map(|node| {
                let mut reads = node
                    .reads
                    .iter()
                    .filter_map(|id| self.local_ids.get(id).cloned())
                    .collect::<Vec<_>>();
                reads.sort();
                let mut writes = node
                    .writes
                    .iter()
                    .filter_map(|id| self.local_ids.get(id).cloned())
                    .collect::<Vec<_>>();
                writes.sort();
                ControlFlowNode {
                    id: node_id(node.id.index()),
                    kind: control_flow_node_kind(node.kind).into(),
                    location: node
                        .span
                        .map(|span| self.location(file_id, span.start(), span.end())),
                    ast_node_id: node.span.and_then(|span| {
                        self.exact_node(file_id, span)
                            .or_else(|| self.smallest_node(file_id, span))
                    }),
                    reads,
                    writes,
                }
            })
            .collect();
        let edges = graph
            .edges()
            .iter()
            .map(|edge| ControlFlowEdge {
                from: node_id(edge.from.index()),
                to: node_id(edge.to.index()),
                kind: control_flow_edge_kind(edge.kind).into(),
            })
            .collect();
        let entry = node_id(graph.entry().index());
        let exit = node_id(graph.exit().index());
        ControlFlowGraph {
            symbol_id,
            entry,
            exit,
            nodes,
            edges,
        }
    }

    fn call_node(&self, file_id: FileId, span: Span) -> Option<Span> {
        let info = self.file_db.get_by_id(file_id)?;
        let mut node = info.find_node_at_span(span)?;
        loop {
            if node.kind() == "function_call" {
                let callee = node.child_by_field_name("callee")?;
                if callee.start_byte() <= span.start() && callee.end_byte() >= span.end() {
                    return Some(Span {
                        start: node.start_byte() as u32,
                        end: node.end_byte() as u32,
                    });
                }
                return None;
            }
            node = node.parent()?;
        }
    }

    fn exact_node(&self, file_id: FileId, span: Span) -> Option<NodeId> {
        self.node_ids.get(&(file_id, span.start, span.end)).cloned()
    }

    fn smallest_node(&self, file_id: FileId, span: Span) -> Option<NodeId> {
        self.nodes
            .iter()
            .filter(|node| {
                node.location.path == self.paths[&file_id]
                    && node.location.byte_range.start <= span.start
                    && node.location.byte_range.end >= span.end
            })
            .min_by_key(|node| node.location.byte_range.end - node.location.byte_range.start)
            .map(|node| node.id.clone())
    }

    fn location(&self, file_id: FileId, start: usize, end: usize) -> SourceLocation {
        let path = self.paths[&file_id].clone();
        let source = self
            .file_db
            .get_by_id(file_id)
            .map(|file| file.source().source.clone())
            .unwrap_or_default();
        SourceLocation {
            path,
            range: SourceRange {
                start: utf16_position(&source, start),
                end: utf16_position(&source, end),
            },
            byte_range: ByteRange {
                start: start as u32,
                end: end as u32,
            },
        }
    }
}

struct TypeBuilder<'a> {
    interner: &'a TypeInterner,
    symbol_ids: &'a HashMap<tolk_resolver::SymbolId, SymbolId>,
    ids: HashMap<TyId, TypeId>,
    types: Vec<TypeInfo>,
}

impl<'a> TypeBuilder<'a> {
    fn new(
        interner: &'a TypeInterner,
        symbol_ids: &'a HashMap<tolk_resolver::SymbolId, SymbolId>,
    ) -> Self {
        Self {
            interner,
            symbol_ids,
            ids: HashMap::new(),
            types: vec![],
        }
    }

    fn convert(&mut self, ty: TyId) -> TypeId {
        if let Some(id) = self.ids.get(&ty) {
            return id.clone();
        }
        let id = format!("t{}", self.ids.len());
        self.ids.insert(ty, id.clone());
        let data = self.interner.data(ty).clone();
        let (kind, symbol, children, ret) = match data {
            TyData::Struct { def, args, .. } => (
                "struct",
                self.symbol_ids.get(&def).cloned(),
                args.unwrap_or_default(),
                None,
            ),
            TyData::Enum { def, .. } => ("enum", self.symbol_ids.get(&def).cloned(), vec![], None),
            TyData::TypeAlias {
                def,
                inner_ty,
                args,
                ..
            } => {
                let mut values = vec![inner_ty];
                values.extend(args.unwrap_or_default());
                (
                    "typeAlias",
                    self.symbol_ids.get(&def).cloned(),
                    values,
                    None,
                )
            }
            TyData::Tensor(v) => ("tensor", None, v, None),
            TyData::Tuple(v) => ("tuple", None, v, None),
            TyData::Array(v) => ("array", None, vec![v], None),
            TyData::Union(v) => ("union", None, v, None),
            TyData::Func { params, return_ty } => ("function", None, params, Some(return_ty)),
            TyData::TypeParameter { .. } => ("typeParameter", None, vec![], None),
            TyData::GenericTypeWithTs { inner_ty, types } => {
                let mut values = vec![inner_ty];
                values.extend(types);
                ("generic", None, values, None)
            }
            TyData::Builtin { .. } => ("builtin", None, vec![], None),
            TyData::Int(_) => ("integer", None, vec![], None),
            TyData::Bool { .. } => ("boolean", None, vec![], None),
            TyData::Cell => ("cell", None, vec![], None),
            TyData::Slice => ("slice", None, vec![], None),
            TyData::Builder => ("builder", None, vec![], None),
            TyData::Continuation => ("continuation", None, vec![], None),
            TyData::Address(_) => ("address", None, vec![], None),
            TyData::MapKV { key, value } => ("map", None, vec![key, value], None),
            TyData::Bits { .. } => ("bits", None, vec![], None),
            TyData::Bytes { .. } => ("bytes", None, vec![], None),
            TyData::UntypedTuple => ("untypedTuple", None, vec![], None),
            TyData::Null => ("null", None, vec![], None),
            TyData::Void => ("void", None, vec![], None),
            TyData::Never => ("never", None, vec![], None),
            TyData::Auto => ("auto", None, vec![], None),
            TyData::Undefined => ("undefined", None, vec![], None),
            TyData::Unknown => ("unknown", None, vec![], None),
        };
        let element_types = children
            .into_iter()
            .map(|child| self.convert(child))
            .collect();
        let return_type = ret.map(|child| self.convert(child));
        self.types.push(TypeInfo {
            id: id.clone(),
            display: self.interner.format(ty),
            kind: kind.into(),
            symbol_id: symbol,
            element_types,
            return_type,
        });
        id
    }
}

impl ConstantEvaluationContext for SnapshotBuilder<'_> {
    fn file_db(&self) -> &FileDb {
        self.file_db
    }

    fn project_index(&self) -> &ProjectIndex {
        self.project
    }

    fn resolve_at(&self, file_id: FileId, span: Span) -> Option<Resolved> {
        self.project
            .get_resolved_uses(file_id)?
            .find_use(span.start())
            .map(|usage| usage.resolved.clone())
    }
}

fn constant_value(value: ActonConstantValue) -> ConstantValue {
    let display = value.format();
    match value {
        ActonConstantValue::Int(value) => ConstantValue::Int {
            value: value.to_string(),
            display,
        },
        ActonConstantValue::Bool(value) => ConstantValue::Bool { value, display },
        ActonConstantValue::String(value) => ConstantValue::String { value, display },
        ActonConstantValue::Overflow => ConstantValue::Overflow { display },
        ActonConstantValue::Unknown => ConstantValue::Unknown { display },
    }
}

fn constant_collection_key(value: &ActonConstantValue) -> Option<String> {
    match value {
        ActonConstantValue::Int(value) => Some(format!("int:{value}")),
        ActonConstantValue::Bool(value) => Some(format!("bool:{value}")),
        ActonConstantValue::String(value) => Some(format!("string:{value}")),
        ActonConstantValue::Overflow | ActonConstantValue::Unknown => None,
    }
}

fn collection_kinds(bodies: &WorkspaceBodyTypes, type_db: &TypeDb<'_>) -> CollectionKinds {
    let mut kinds = CollectionKinds::new();
    for (&file_id, file_bodies) in bodies {
        for inference in file_bodies.values() {
            for (&span, &ty) in &inference.expression_types {
                let ty = type_db.intrn.unwrap_alias(ty);
                let kind = match type_db.intrn.data(ty) {
                    TyData::Array(_) => Some(CollectionKind::Array),
                    TyData::MapKV { .. } => Some(CollectionKind::Map),
                    TyData::Struct { name, .. } if name.as_ref() == "map" => {
                        Some(CollectionKind::Map)
                    }
                    _ => None,
                };
                if let Some(kind) = kind {
                    kinds.insert((file_id, span.start, span.end), kind);
                }
            }
        }
    }
    kinds
}

const fn control_flow_node_kind(kind: ActonFlowNodeKind) -> &'static str {
    match kind {
        ActonFlowNodeKind::Entry => "entry",
        ActonFlowNodeKind::Exit => "exit",
        ActonFlowNodeKind::Nop => "nop",
        ActonFlowNodeKind::Expr => "expression",
        ActonFlowNodeKind::Condition => "condition",
        ActonFlowNodeKind::Assert => "assert",
        ActonFlowNodeKind::Return => "return",
        ActonFlowNodeKind::Throw => "throw",
        ActonFlowNodeKind::Break => "break",
        ActonFlowNodeKind::Continue => "continue",
        ActonFlowNodeKind::MatchPattern => "matchPattern",
        ActonFlowNodeKind::CatchBinding => "catchBinding",
        ActonFlowNodeKind::Join => "join",
    }
}

const fn control_flow_edge_kind(kind: ActonEdgeKind) -> &'static str {
    match kind {
        ActonEdgeKind::Unconditional => "unconditional",
        ActonEdgeKind::TrueBranch => "trueBranch",
        ActonEdgeKind::FalseBranch => "falseBranch",
        ActonEdgeKind::LoopBack => "loopBack",
        ActonEdgeKind::Break => "break",
        ActonEdgeKind::Continue => "continue",
        ActonEdgeKind::Return => "return",
        ActonEdgeKind::Throw => "throw",
        ActonEdgeKind::Exceptional => "exceptional",
    }
}

fn normalize_logical_path(root: &Path, value: &str) -> Result<PathBuf> {
    if value.trim().is_empty() {
        bail!("logical paths must not be empty");
    }
    let path = Path::new(value);
    let joined;
    let path = if path.is_absolute() {
        path
    } else {
        joined = root.join(path);
        &joined
    };
    normalize_path(path)
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => result.push(part),
            Component::ParentDir => {
                if !result.pop() {
                    bail!("logical path escapes its root: {}", path.display());
                }
            }
            Component::Prefix(_) => bail!(
                "logical paths must use portable slash-rooted paths: {}",
                path.display()
            ),
        }
    }
    Ok(result)
}

fn all_global_symbols(symbols: &[Symbol]) -> Vec<&Symbol> {
    fn collect<'a>(symbol: &'a Symbol, out: &mut Vec<&'a Symbol>) {
        out.push(symbol);
        match &symbol.kind {
            SymbolKind::Struct { fields, .. } => {
                for field in fields {
                    collect(field, out);
                }
            }
            SymbolKind::Enum { members } => {
                for member in members {
                    collect(member, out);
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    for symbol in symbols {
        collect(symbol, &mut out);
    }
    out
}

fn global_symbol_id(path: &str, id: tolk_resolver::SymbolId) -> SymbolId {
    format!("g:{path}:{}", id.local_id)
}

fn symbol_kind(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::GlobalVariable => "globalVariable",
        SymbolKind::Function { .. } => "function",
        SymbolKind::Method { .. } => "method",
        SymbolKind::GetMethod { .. } => "getMethod",
        SymbolKind::Struct { .. } => "struct",
        SymbolKind::StructField => "structField",
        SymbolKind::Enum { .. } => "enum",
        SymbolKind::EnumMember => "enumMember",
        SymbolKind::Constant => "constant",
        SymbolKind::TypeAlias { .. } => "typeAlias",
    }
}

fn camel_kind(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper = false;
    for ch in raw.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn utf16_position(source: &str, byte: usize) -> Position {
    let byte = byte.min(source.len());
    let prefix = source
        .get(..byte)
        .unwrap_or_else(|| &source[..source.floor_char_boundary(byte)]);
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Position {
        line: prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
        character: prefix[line_start..].encode_utf16().count() as u32,
    }
}

fn point_to_byte(source: &str, row: usize, column: usize) -> usize {
    let line_start = source
        .split_inclusive('\n')
        .take(row)
        .map(str::len)
        .sum::<usize>();
    (line_start + column).min(source.len())
}

fn collect_owned_call_nodes(
    node: Node<'_>,
    file_id: FileId,
    owner: &SymbolId,
    lambda_ids: &HashMap<(FileId, u32, u32), SymbolId>,
    calls: &mut Vec<ProgramCall>,
    creations: &mut Vec<LambdaCreation>,
) {
    if node.kind() == "lambda_expression" {
        let span = Span {
            start: node.start_byte() as u32,
            end: node.end_byte() as u32,
        };
        let Some(lambda) = lambda_ids.get(&(file_id, span.start, span.end)) else {
            return;
        };
        creations.push(LambdaCreation {
            file_id,
            owner: owner.clone(),
            lambda: lambda.clone(),
            span,
        });
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_owned_call_nodes(child, file_id, lambda, lambda_ids, calls, creations);
        }
        return;
    }
    if node.kind() == "function_call" {
        calls.push(ProgramCall {
            file_id,
            caller: owner.clone(),
            span: Span {
                start: node.start_byte() as u32,
                end: node.end_byte() as u32,
            },
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_owned_call_nodes(child, file_id, owner, lambda_ids, calls, creations);
    }
}

fn containing_flow_node(
    graph: &ActonControlFlowGraph,
    span: Span,
) -> Option<&tolk_dataflow::FlowNode> {
    graph
        .nodes()
        .iter()
        .filter(|node| {
            node.span
                .is_some_and(|node_span| node_span.start <= span.start && node_span.end >= span.end)
        })
        .min_by_key(|node| node.span.map_or(u32::MAX, |span| span.end - span.start))
}

fn reference_key(reference: &Reference) -> (&str, u32, u32, &str) {
    (
        &reference.location.path,
        reference.location.byte_range.start,
        reference.location.byte_range.end,
        &reference.name,
    )
}
fn merged_callable_value(left: &CallableValue, right: &CallableValue) -> CallableValue {
    if left.bottom {
        return right.clone();
    }
    if right.bottom {
        return left.clone();
    }
    let mut members = BTreeMap::new();
    for key in left.members.keys().chain(right.members.keys()) {
        if members.contains_key(key) {
            continue;
        }
        let value = match (left.members.get(key), right.members.get(key)) {
            (Some(left), Some(right)) => merged_callable_value(left, right),
            (Some(left), None) => merged_callable_value(
                left,
                &if right.members_complete {
                    CallableValue::non_callable()
                } else {
                    CallableValue::default()
                },
            ),
            (None, Some(right)) => merged_callable_value(
                &if left.members_complete {
                    CallableValue::non_callable()
                } else {
                    CallableValue::default()
                },
                right,
            ),
            (None, None) => continue,
        };
        members.insert(key.clone(), value);
    }
    CallableValue {
        targets: left.targets.union(&right.targets).cloned().collect(),
        complete: left.complete && right.complete,
        members,
        members_complete: left.members_complete && right.members_complete,
        array_lengths: match (&left.array_lengths, &right.array_lengths) {
            (Some(left), Some(right)) => Some(left.union(right).copied().collect()),
            (None, _) | (_, None) => None,
        },
        bottom: false,
    }
}

fn merge_optional_callable_value(current: &mut Option<CallableValue>, incoming: &CallableValue) {
    *current = Some(match current.take() {
        Some(current) => merged_callable_value(&current, incoming),
        None => incoming.clone(),
    });
}

fn merge_callable_states(current: &mut CallableState, incoming: &CallableState) -> bool {
    let mut changed = false;
    for (&local, value) in incoming {
        let existing = current.entry(local).or_default();
        let merged = merged_callable_value(existing, value);
        if *existing != merged {
            *existing = merged;
            changed = true;
        }
    }
    changed
}

fn normalize_call_sites(call_sites: &mut Vec<CallSite>) {
    for call_site in call_sites.iter_mut() {
        call_site.targets.sort();
        call_site.targets.dedup();
    }
    call_sites.sort_by(|left, right| call_site_key(left).cmp(&call_site_key(right)));

    let mut merged = Vec::<CallSite>::with_capacity(call_sites.len());
    for call_site in call_sites.drain(..) {
        if let Some(previous) = merged.last_mut()
            && call_site_key(previous) == call_site_key(&call_site)
        {
            previous.targets.extend(call_site.targets);
            previous.targets.sort();
            previous.targets.dedup();
            previous.complete &= call_site.complete;
        } else {
            merged.push(call_site);
        }
    }
    *call_sites = merged;
}

fn call_site_key(call_site: &CallSite) -> (&str, u32, u32, &str, &str) {
    (
        &call_site.location.path,
        call_site.location.byte_range.start,
        call_site.location.byte_range.end,
        &call_site.caller,
        &call_site.dispatch,
    )
}

fn call_key(call: &CallEdge) -> (&str, u32, u32, &str, &str, &str) {
    (
        &call.call_site.path,
        call.call_site.byte_range.start,
        call.call_site.byte_range.end,
        &call.caller,
        &call.callee,
        &call.dispatch,
    )
}
fn diagnostic_key(diagnostic: &Diagnostic) -> (&str, u32, &str) {
    (
        diagnostic
            .location
            .as_ref()
            .map_or("", |location| location.path.as_str()),
        diagnostic
            .location
            .as_ref()
            .map_or(0, |location| location.byte_range.start),
        &diagnostic.message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)], roots: &[&str]) -> ProjectSnapshot {
        inspect(ProjectInput {
            root: "/project".into(),
            files: files
                .iter()
                .map(|(path, text)| ((*path).into(), (*text).into()))
                .collect(),
            entrypoints: roots.iter().map(|path| (*path).into()).collect(),
            stdlib_root: None,
            acton_stdlib_root: None,
            import_mappings: BTreeMap::new(),
            control_flow: ControlFlowScope::Workspace,
        })
        .unwrap()
    }

    fn project_with_stdlib(source: &str) -> ProjectSnapshot {
        let common = r#"
tolk 1.4
type int = builtin
type bool = builtin
type cell = builtin
type unknown = builtin
struct map<K, V> { private tvmDict: cell? }
struct array<T> { private tvmTuple: unknown }
struct MapLookupResult<TValue> { private value: TValue, isFound: bool }
fun array<T>.push(mutate self, value: T): void builtin
fun array<T>.first(self): T builtin
fun array<T>.get(self, index: int): T builtin
fun array<T>.set(mutate self, value: T, index: int): void builtin
fun array<T>.last(self): T builtin
fun array<T>.pop(mutate self): T builtin
fun map<K, V>.get(self, key: K): MapLookupResult<V> builtin
fun map<K, V>.mustGet(self, key: K, throwIfNotFound: int = 9): V builtin
fun map<K, V>.set(mutate self, key: K, value: V): self builtin
fun MapLookupResult<TValue>.loadValue(self): TValue { return self.value; }
"#;
        inspect(ProjectInput {
            root: "/project".into(),
            files: [
                ("/project/main.tolk".into(), source.into()),
                ("/project/stdlib/common.tolk".into(), common.into()),
            ]
            .into_iter()
            .collect(),
            entrypoints: vec!["/project/main.tolk".into()],
            stdlib_root: Some("/project/stdlib".into()),
            acton_stdlib_root: None,
            import_mappings: BTreeMap::new(),
            control_flow: ControlFlowScope::Workspace,
        })
        .unwrap()
    }

    #[test]
    fn callable_lattice_distinguishes_bottom_unknown_and_nested_shapes() {
        let target = CallableValue::target("callable".into());
        assert_eq!(
            merged_callable_value(&CallableValue::bottom(), &target),
            target
        );

        let partial = merged_callable_value(&CallableValue::default(), &target);
        assert_eq!(partial.targets, BTreeSet::from(["callable".into()]));
        assert!(!partial.complete);

        let mut shape = CallableValue::bottom();
        shape.set_member_path(&["outer".into(), "handler".into()], target.clone());
        assert_eq!(shape.member("outer").member("handler"), target);
        assert!(!shape.member("missing").complete);

        shape.limit_member_depth(1);
        let truncated = shape.member("outer").member("handler");
        assert!(truncated.targets.is_empty());
        assert!(!truncated.complete);

        let mut exact = CallableValue::non_callable();
        exact.members_complete = true;
        assert!(exact.member("missing").complete);
    }

    #[test]
    fn resolves_cross_file_calls_and_preserves_utf16_locations() {
        let snapshot = project(
            &[
                (
                    "/project/main.tolk",
                    "import \"lib\";\nfun main() { /* 😀 */ answer(); }",
                ),
                ("/project/lib.tolk", "fun answer(): int { return 42; }"),
            ],
            &["/project/main.tolk"],
        );
        let answer = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "answer")
            .unwrap();
        let call = snapshot
            .call_graph
            .iter()
            .find(|call| call.callee == answer.id)
            .unwrap();
        let call_site = snapshot
            .call_sites
            .iter()
            .find(|call_site| call_site.targets.contains(&answer.id))
            .unwrap();
        assert_eq!(call.dispatch, "direct");
        assert_eq!(call_site.dispatch, "direct");
        assert!(call_site.complete);
        assert_eq!(call.call_site.range.start.line, 1);
        assert_eq!(
            snapshot
                .references
                .iter()
                .find(|reference| reference.name == "answer")
                .unwrap()
                .symbol_id
                .as_ref(),
            Some(&answer.id)
        );
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.kind == "functionDeclaration")
        );
    }

    #[test]
    fn classifies_reference_access_with_tolk_analysis() {
        let source = r#"
struct Counter {
    value: int
}

fun Counter.increment(mutate self) {
    self.value += 1;
}

fun main() {
    var counter = Counter { value: 0 };
    counter.increment();
    val copy = counter;
    counter = Counter { value: 1 };
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let counter_uses = snapshot
            .references
            .iter()
            .filter(|reference| reference.name == "counter")
            .collect::<Vec<_>>();

        let mutation = counter_uses
            .iter()
            .find(|reference| reference.context.access.mutate)
            .expect("mutable method receiver must be classified as a mutation");
        assert_eq!(mutation.context.usage, "call");
        assert!(mutation.context.access.read);
        assert!(mutation.context.access.write);

        let read = counter_uses
            .iter()
            .find(|reference| reference.context.usage == "read" && !reference.context.access.write)
            .expect("copy initializer must be classified as a read");
        assert!(read.context.access.read);
        assert!(!read.context.access.mutate);

        let write = counter_uses
            .iter()
            .find(|reference| reference.context.usage == "write")
            .expect("assignment target must be classified as a write");
        assert!(!write.context.access.read);
        assert!(write.context.access.write);
        assert!(!write.context.access.mutate);
        assert_eq!(snapshot.call_sites.len(), 1);
        assert_eq!(snapshot.call_sites[0].dispatch, "direct");
    }

    #[test]
    fn evaluates_constants_and_enum_members_without_losing_integer_precision() {
        let source = r#"
const BASE = 10;
const VALUE = (BASE + 2) * 3;
const HUGE = 18446744073709551616;
const ENABLED = true;
const LABEL = "tolk";
const TOO_BIG = 1 << 256;
const NONE = null;

enum Mode {
    First = VALUE,
    Second,
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let value_for = |name: &str| {
            let symbol = snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .expect("expected symbol");
            &snapshot
                .constant_values
                .iter()
                .find(|constant| constant.symbol_id == symbol.id)
                .expect("expected evaluated value")
                .value
        };

        assert!(matches!(
            value_for("VALUE"),
            ConstantValue::Int { value, .. } if value == "36"
        ));
        assert!(matches!(
            value_for("HUGE"),
            ConstantValue::Int { value, .. } if value == "18446744073709551616"
        ));
        assert!(matches!(
            value_for("First"),
            ConstantValue::Int { value, .. } if value == "36"
        ));
        assert!(matches!(
            value_for("Second"),
            ConstantValue::Int { value, .. } if value == "37"
        ));
        assert!(matches!(
            value_for("ENABLED"),
            ConstantValue::Bool { value: true, .. }
        ));
        assert!(matches!(
            value_for("LABEL"),
            ConstantValue::String { value, .. } if value == "tolk"
        ));
        assert!(matches!(
            value_for("TOO_BIG"),
            ConstantValue::Overflow { .. }
        ));
        assert!(matches!(value_for("NONE"), ConstantValue::Unknown { .. }));
    }

    #[test]
    fn exposes_control_flow_branches_loops_locations_and_local_accesses() {
        let source = r#"
fun flow(x: int): int {
    var y = x;
    if (y > 0) {
        y = y - 1;
    } else {
        y = y + 1;
    }
    while (y > 0) {
        y = y - 1;
    }
    return y;
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let flow = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "flow")
            .expect("flow symbol");
        let y = snapshot
            .symbols
            .iter()
            .find(|symbol| {
                symbol.name == "y" && symbol.containing_symbol.as_ref() == Some(&flow.id)
            })
            .expect("local y symbol");
        let graph = snapshot
            .control_flow_graphs
            .iter()
            .find(|graph| graph.symbol_id == flow.id)
            .expect("flow CFG");

        assert!(graph.nodes.iter().any(|node| node.id == graph.entry));
        assert!(graph.nodes.iter().any(|node| node.id == graph.exit));
        assert!(graph.edges.iter().any(|edge| edge.kind == "trueBranch"));
        assert!(graph.edges.iter().any(|edge| edge.kind == "falseBranch"));
        assert!(graph.edges.iter().any(|edge| edge.kind == "loopBack"));
        assert!(graph.edges.iter().any(|edge| edge.kind == "return"));
        assert!(graph.nodes.iter().any(|node| node.reads.contains(&y.id)));
        assert!(graph.nodes.iter().any(|node| node.writes.contains(&y.id)));
        assert!(graph.nodes.iter().any(|node| {
            node.kind == "condition" && node.location.is_some() && node.ast_node_id.is_some()
        }));
    }

    #[test]
    fn can_disable_control_flow_generation() {
        let snapshot = inspect(ProjectInput {
            root: "/project".into(),
            files: [("/project/main.tolk".into(), "fun main() {}".into())]
                .into_iter()
                .collect(),
            entrypoints: vec!["/project/main.tolk".into()],
            stdlib_root: None,
            acton_stdlib_root: None,
            import_mappings: BTreeMap::new(),
            control_flow: ControlFlowScope::None,
        })
        .unwrap();
        assert!(snapshot.control_flow_graphs.is_empty());
    }

    #[test]
    fn control_flow_scope_excludes_or_includes_stdlib_graphs() {
        let input = ProjectInput {
            root: "/project".into(),
            files: [
                ("/project/main.tolk".into(), "fun main() {}".into()),
                (
                    "/project/stdlib/common.tolk".into(),
                    "fun stdlibHelper() {}".into(),
                ),
            ]
            .into_iter()
            .collect(),
            entrypoints: vec!["/project/main.tolk".into()],
            stdlib_root: Some("/project/stdlib".into()),
            acton_stdlib_root: None,
            import_mappings: BTreeMap::new(),
            control_flow: ControlFlowScope::Workspace,
        };
        let workspace = inspect(input.clone()).unwrap();
        let all = inspect(ProjectInput {
            control_flow: ControlFlowScope::All,
            ..input
        })
        .unwrap();

        assert_eq!(workspace.control_flow_graphs.len(), 1);
        assert_eq!(all.control_flow_graphs.len(), 2);
        assert!(workspace.control_flow_graphs.iter().all(|graph| {
            workspace
                .symbols
                .iter()
                .find(|symbol| symbol.id == graph.symbol_id)
                .is_some_and(|symbol| symbol.name != "stdlibHelper")
        }));
        assert!(all.control_flow_graphs.iter().any(|graph| {
            all.symbols
                .iter()
                .find(|symbol| symbol.id == graph.symbol_id)
                .is_some_and(|symbol| symbol.name == "stdlibHelper")
        }));
    }

    #[test]
    fn returns_partial_tree_with_parse_diagnostics() {
        let snapshot = project(
            &[("/project/main.tolk", "fun main( { return 1; }")],
            &["/project/main.tolk"],
        );
        assert!(!snapshot.nodes.is_empty());
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.phase == "parse")
        );
    }

    #[test]
    fn follows_cyclic_imports_and_records_recursive_call_sites() {
        let snapshot = project(
            &[
                (
                    "/project/a.tolk",
                    "import \"b\"; fun alpha() { beta(); alpha(); }",
                ),
                ("/project/b.tolk", "import \"a\"; fun beta() { alpha(); }"),
            ],
            &["/project/a.tolk"],
        );
        assert_eq!(snapshot.files.len(), 2);
        assert_eq!(snapshot.call_graph.len(), 3);
        assert!(
            snapshot
                .call_graph
                .iter()
                .any(|edge| edge.caller == edge.callee)
        );
    }

    #[test]
    fn resolves_function_valued_locals_across_copies_branches_and_loops() {
        let source = r#"
fun first(value: int): int { return value; }
fun second(value: int): int { return value + 1; }

fun choose(flag: bool, value: int): int {
    var action = first;
    if (flag) {
        action = second;
    }
    var alias = action;
    while (flag) {
        alias = first;
    }
    return alias(value);
}

fun invoke(callback: (int) -> int, value: int): int {
    return callback(value);
}
"#;
        let snapshot = inspect(ProjectInput {
            root: "/project".into(),
            files: [("/project/main.tolk".into(), source.into())]
                .into_iter()
                .collect(),
            entrypoints: vec!["/project/main.tolk".into()],
            stdlib_root: None,
            acton_stdlib_root: None,
            import_mappings: BTreeMap::new(),
            control_flow: ControlFlowScope::None,
        })
        .unwrap();
        assert!(snapshot.control_flow_graphs.is_empty());

        let symbol = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let choose = symbol("choose");
        let first = symbol("first");
        let second = symbol("second");
        let invoke = symbol("invoke");

        let indirect = snapshot
            .call_sites
            .iter()
            .find(|call_site| call_site.caller == choose.id && call_site.dispatch == "indirect")
            .expect("resolved indirect call site");
        assert!(indirect.complete);
        assert_eq!(
            indirect.targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([first.id.clone(), second.id.clone()])
        );
        assert!(snapshot.call_graph.iter().any(|edge| {
            edge.caller == choose.id && edge.callee == first.id && edge.dispatch == "indirect"
        }));
        assert!(snapshot.call_graph.iter().any(|edge| {
            edge.caller == choose.id && edge.callee == second.id && edge.dispatch == "indirect"
        }));

        let unknown = snapshot
            .call_sites
            .iter()
            .find(|call_site| call_site.caller == invoke.id)
            .expect("unknown callback call site");
        assert_eq!(unknown.dispatch, "indirect");
        assert!(!unknown.complete);
        assert!(unknown.targets.is_empty());
    }

    #[test]
    fn resolves_method_values_and_marks_partially_known_calls() {
        let source = r#"
fun int.increment(self): int { return self + 1; }
fun fallback(value: int): int { return value; }

fun viaMethod(value: int): int {
    val action = int.increment;
    return action(value);
}

fun maybe(flag: bool, callback: (int) -> int, value: int): int {
    var action = fallback;
    if (flag) {
        action = callback;
    }
    return action(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };

        let method = global("increment");
        let via_method = global("viaMethod");
        let method_call = snapshot
            .call_sites
            .iter()
            .find(|call_site| call_site.caller == via_method.id)
            .expect("method-valued local call");
        assert!(method_call.complete);
        assert_eq!(method_call.targets, vec![method.id.clone()]);

        let maybe = global("maybe");
        let fallback = global("fallback");
        let partial = snapshot
            .call_sites
            .iter()
            .find(|call_site| call_site.caller == maybe.id)
            .expect("partially known call");
        assert!(!partial.complete);
        assert_eq!(partial.targets, vec![fallback.id.clone()]);
        assert!(snapshot.call_graph.iter().any(|edge| {
            edge.caller == maybe.id && edge.callee == fallback.id && edge.dispatch == "indirect"
        }));
    }

    #[test]
    fn resolves_wrapped_get_method_and_exceptional_callable_values() {
        let source = r#"
fun first(value: int): int { return value; }
fun second(value: int): int { return value + 1; }
fun identity<T>(value: T): T { return value; }
get fun current(): int { return 1; }

fun viaTernary(flag: bool, value: int): int {
    val action = flag ? first : second;
    return action(value);
}

fun viaParentheses(value: int): int {
    val action = (first);
    return action(value);
}

fun viaCast(value: int): int {
    val action = first as ((int) -> int);
    return action(value);
}

fun viaGeneric(value: int): int {
    val action = identity<int>;
    return action(value);
}

fun viaGetMethod(): int {
    val action = current;
    return action();
}

fun viaTry(value: int): int {
    var action = first;
    try {
        action = second;
    } catch {}
    return action(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let target_ids = |caller: &str| {
            let caller = global(caller);
            let call_site = snapshot
                .call_sites
                .iter()
                .find(|call_site| call_site.caller == caller.id && call_site.dispatch == "indirect")
                .expect("indirect call site");
            assert!(call_site.complete);
            call_site.targets.iter().cloned().collect::<BTreeSet<_>>()
        };

        let first = global("first").id.clone();
        let second = global("second").id.clone();
        let identity = global("identity").id.clone();
        let current = global("current").id.clone();
        assert_eq!(
            target_ids("viaTernary"),
            BTreeSet::from([first.clone(), second.clone()])
        );
        assert_eq!(
            target_ids("viaParentheses"),
            BTreeSet::from([first.clone()])
        );
        assert_eq!(target_ids("viaCast"), BTreeSet::from([first.clone()]));
        assert_eq!(target_ids("viaGeneric"), BTreeSet::from([identity]));
        assert_eq!(target_ids("viaGetMethod"), BTreeSet::from([current]));
        assert_eq!(target_ids("viaTry"), BTreeSet::from([first, second]));
    }

    #[test]
    fn resolves_callables_from_returns_lambdas_and_containers() {
        let source = r#"
fun first(value: int): int { return value; }
fun second(value: int): int { return value + 1; }
fun factory(): ((int) -> int) { return first; }

fun viaReturn(value: int): int {
    val action = factory();
    return action(value);
}

fun viaLambda(value: int): int {
    val action = fun (inner: int): int { return inner; };
    return action(value);
}

fun viaContainer(value: int): int {
    val actions = (first, second);
    val action = actions.0;
    return action(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let first = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "first" && !symbol.flags.local)
            .expect("first symbol");
        let lambda = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.kind == "lambda")
            .expect("lambda symbol");
        assert!(lambda.flags.local);

        for (caller_name, target) in [
            ("viaReturn", &first.id),
            ("viaLambda", &lambda.id),
            ("viaContainer", &first.id),
        ] {
            let caller = snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == caller_name && !symbol.flags.local)
                .expect("caller symbol");
            let call_site = snapshot
                .call_sites
                .iter()
                .find(|call_site| call_site.caller == caller.id && call_site.dispatch == "indirect")
                .expect("resolved indirect call site");
            assert!(call_site.complete);
            assert_eq!(call_site.targets.len(), 1);
            assert_eq!(&call_site.targets[0], target);
            assert!(
                snapshot
                    .call_graph
                    .iter()
                    .any(|edge| edge.caller == caller.id && edge.callee == **target)
            );
        }
    }

    #[test]
    fn resolves_callbacks_through_recursive_forwarding_and_merged_callers() {
        let source = r#"
fun first(value: int): int { return value + 1; }
fun second(value: int): int { return value + 2; }

fun select(flag: bool): ((int) -> int) {
    return flag ? first : second;
}

fun bounce(callback: ((int) -> int), recurse: bool): ((int) -> int) {
    if (recurse) {
        return bounce(callback, false);
    }
    return callback;
}

fun invoke(callback: ((int) -> int), value: int): int {
    return callback(value);
}

fun primary(flag: bool, value: int): int {
    val selected = select(flag);
    val forwarded = bounce(selected, true);
    val firstResult = forwarded(value);
    return invoke(selected, firstResult);
}

fun secondary(value: int): int {
    return invoke(second, value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let expected = BTreeSet::from([global("first").id.clone(), global("second").id.clone()]);
        for caller_name in ["primary", "invoke"] {
            let caller = global(caller_name);
            let call = snapshot
                .call_sites
                .iter()
                .find(|call| call.caller == caller.id && call.dispatch == "indirect")
                .expect("indirect callback invocation");
            assert!(call.complete, "{caller_name} should be fully resolved");
            assert_eq!(
                call.targets.iter().cloned().collect::<BTreeSet<_>>(),
                expected
            );
        }
    }

    #[test]
    fn resolves_nested_containers_across_generic_parameter_and_return_boundaries() {
        let source = r#"
fun first(value: int): int { return value + 1; }
fun second(value: int): int { return value + 2; }
fun identity<T>(value: T): T { return value; }

fun audit(flag: bool, value: int): int {
    val routes = flag
        ? ((first, second), (second, first))
        : ((second, first), (first, second));
    val forwarded = identity(routes);
    val primary = forwarded.0.1;
    val fallback = forwarded.1.0;
    return primary(fallback(value));
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let audit = global("audit");
        let expected = BTreeSet::from([global("first").id.clone(), global("second").id.clone()]);
        let indirect = snapshot
            .call_sites
            .iter()
            .filter(|call| call.caller == audit.id && call.dispatch == "indirect")
            .collect::<Vec<_>>();
        assert_eq!(indirect.len(), 2);
        for call in indirect {
            assert!(call.complete);
            assert_eq!(
                call.targets.iter().cloned().collect::<BTreeSet<_>>(),
                expected
            );
        }
    }

    #[test]
    fn resolves_callable_array_elements_after_get_set_and_push() {
        let source = r#"
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }
fun review(value: int): int { return value + 3; }
fun identity<T>(value: T): T { return value; }

fun audit(index: int, value: int): int {
    var handlers: array<(int) -> int> = [allow, reject];
    handlers.set(review, 1);
    handlers.push(reject);
    val forwarded = identity(handlers);
    val exact = forwarded.get(0);
    val pushed = forwarded.get(2);
    val selected = forwarded.get(index);
    return exact(pushed(selected(value)));
}
"#;
        let snapshot = project_with_stdlib(source);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let audit = global("audit");
        let mut indirect = snapshot
            .call_sites
            .iter()
            .filter(|call| call.caller == audit.id && call.dispatch == "indirect")
            .collect::<Vec<_>>();
        indirect.sort_by_key(|call| call.location.byte_range.start);
        indirect.retain(|call| !call.targets.is_empty());
        assert_eq!(indirect.len(), 3);
        assert!(indirect.iter().all(|call| call.complete));
        assert_eq!(indirect[0].targets, vec![global("allow").id.clone()]);
        assert_eq!(indirect[1].targets, vec![global("reject").id.clone()]);
        assert_eq!(
            indirect[2].targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                global("allow").id.clone(),
                global("reject").id.clone(),
                global("review").id.clone(),
            ])
        );
    }

    #[test]
    fn resolves_callable_map_values_for_constant_and_dynamic_keys() {
        let source = r#"
const PRIMARY = 7;
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }
fun identity<T>(value: T): T { return value; }
fun audit(key: int, flag: bool, value: int): int {
    var handlers: map<int, (int) -> int> = [];
    handlers.set(PRIMARY, allow);
    val exact = handlers.mustGet(PRIMARY);
    if (flag) {
        handlers.set(9, reject);
    }
    val forwarded = identity(handlers);
    val selected = forwarded.get(key).loadValue();
    return exact(selected(value));
}
"#;
        let snapshot = project_with_stdlib(source);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let audit = global("audit");
        let mut indirect = snapshot
            .call_sites
            .iter()
            .filter(|call| call.caller == audit.id && call.dispatch == "indirect")
            .collect::<Vec<_>>();
        indirect.sort_by_key(|call| call.location.byte_range.start);
        indirect.retain(|call| !call.targets.is_empty());
        assert_eq!(indirect.len(), 2);
        assert!(indirect.iter().all(|call| call.complete));
        assert_eq!(indirect[0].targets, vec![global("allow").id.clone()]);
        assert_eq!(
            indirect[1].targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([global("allow").id.clone(), global("reject").id.clone()])
        );
    }

    #[test]
    fn keeps_dynamic_collection_writes_sound_and_external_collections_incomplete() {
        let source = r#"
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }

fun knownMap(key: int, value: int): int {
    var handlers: map<int, (int) -> int> = [];
    handlers.set(1, allow);
    handlers.set(key, reject);
    return handlers.mustGet(1)(value);
}

fun externalArray(handlers: array<(int) -> int>, index: int, value: int): int {
    return handlers.get(index)(value);
}

fun externalMap(handlers: map<int, (int) -> int>, key: int, value: int): int {
    return handlers.mustGet(key)(value);
}
"#;
        let snapshot = project_with_stdlib(source);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };

        let known = snapshot
            .call_sites
            .iter()
            .find(|call| {
                call.caller == global("knownMap").id
                    && call.dispatch == "indirect"
                    && !call.targets.is_empty()
            })
            .expect("known map callback call");
        assert!(known.complete);
        assert_eq!(
            known.targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([global("allow").id.clone(), global("reject").id.clone()])
        );

        for caller in ["externalArray", "externalMap"] {
            let call = snapshot
                .call_sites
                .iter()
                .find(|call| {
                    call.caller == global(caller).id
                        && call.dispatch == "indirect"
                        && call.targets.is_empty()
                })
                .expect("external collection callback call");
            assert!(!call.complete);
        }
    }

    #[test]
    fn widens_callable_collection_updates_across_branches_and_loops() {
        let source = r#"
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }

fun audit(index: int, count: int, flag: bool, value: int): int {
    var handlers: array<(int) -> int> = [allow];
    if (flag) {
        handlers.push(reject);
    }
    var remaining = count;
    while (remaining > 0) {
        handlers.push(reject);
        remaining -= 1;
    }
    return handlers.get(index)(value);
}
"#;
        let snapshot = project_with_stdlib(source);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let call = snapshot
            .call_sites
            .iter()
            .find(|call| call.caller == global("audit").id && call.dispatch == "indirect")
            .expect("collection callback call");
        assert!(call.complete);
        assert_eq!(
            call.targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([global("allow").id.clone(), global("reject").id.clone()])
        );
    }

    #[test]
    fn resolves_array_end_operations_and_chained_map_updates() {
        let source = r#"
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }

fun arrayAudit(value: int): int {
    var handlers: array<(int) -> int> = [allow, reject];
    val first = handlers.first();
    val popped = handlers.pop();
    val last = handlers.last();
    return first(popped(last(value)));
}

fun mapAudit(value: int): int {
    var handlers: map<int, (int) -> int> = [];
    return handlers.set(3, reject).mustGet(3)(value);
}
"#;
        let snapshot = project_with_stdlib(source);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let array_calls = snapshot
            .call_sites
            .iter()
            .filter(|call| {
                call.caller == global("arrayAudit").id
                    && call.dispatch == "indirect"
                    && !call.targets.is_empty()
            })
            .collect::<Vec<_>>();
        assert_eq!(array_calls.len(), 3);
        assert_eq!(
            array_calls
                .iter()
                .filter(|call| call.targets == vec![global("allow").id.clone()])
                .count(),
            2
        );
        assert_eq!(
            array_calls
                .iter()
                .filter(|call| call.targets == vec![global("reject").id.clone()])
                .count(),
            1
        );

        let map_call = snapshot
            .call_sites
            .iter()
            .find(|call| {
                call.caller == global("mapAudit").id
                    && call.dispatch == "indirect"
                    && !call.targets.is_empty()
            })
            .expect("chained map callback call");
        assert!(map_call.complete);
        assert_eq!(map_call.targets, vec![global("reject").id.clone()]);
    }

    #[test]
    fn propagates_collection_mutations_through_helper_parameters() {
        let source = r#"
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }

fun append(mutate handlers: array<(int) -> int>, callback: ((int) -> int)) {
    handlers.push(callback);
}

fun register(
    mutate handlers: map<int, (int) -> int>,
    key: int,
    callback: ((int) -> int)
) {
    handlers.set(key, callback);
}

fun audit(value: int): int {
    var arrayHandlers: array<(int) -> int> = [allow];
    append(arrayHandlers, reject);
    var mapHandlers: map<int, (int) -> int> = [];
    register(mapHandlers, 4, reject);
    return arrayHandlers.get(1)(mapHandlers.mustGet(4)(value));
}
"#;
        let snapshot = project_with_stdlib(source);
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let calls = snapshot
            .call_sites
            .iter()
            .filter(|call| {
                call.caller == global("audit").id
                    && call.dispatch == "indirect"
                    && !call.targets.is_empty()
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.complete));
        assert!(
            calls
                .iter()
                .all(|call| call.targets == vec![global("reject").id.clone()])
        );
    }

    #[test]
    fn resolves_returned_lambdas_in_locals_and_immediate_invocations() {
        let source = r#"
fun make(offset: int): ((int) -> int) {
    return fun (value: int): int { return value + offset; };
}

fun audit(value: int): int {
    val callback = make(1);
    val firstResult = callback(value);
    return make(2)(firstResult);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let audit = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "audit" && !symbol.flags.local)
            .expect("audit symbol");
        let lambda = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.kind == "lambda")
            .expect("lambda symbol");
        let lambda_parameter = snapshot
            .symbols
            .iter()
            .find(|symbol| {
                symbol.name == "value"
                    && symbol.flags.parameter
                    && symbol.containing_symbol.as_ref() == Some(&lambda.id)
            })
            .expect("lambda parameter symbol");
        assert!(lambda_parameter.flags.local);
        let indirect = snapshot
            .call_sites
            .iter()
            .filter(|call| call.caller == audit.id && call.dispatch == "indirect")
            .collect::<Vec<_>>();
        assert_eq!(indirect.len(), 2);
        for call in indirect {
            assert!(call.complete);
            assert_eq!(call.targets, vec![lambda.id.clone()]);
        }
    }

    #[test]
    fn attributes_captured_callback_calls_and_control_flow_to_lambda() {
        let source = r#"
fun allow(value: int): int { return value; }
fun reject(value: int): int { throw value; }

fun factory(flag: bool): ((int) -> int) {
    var captured = allow;
    if (flag) {
        captured = reject;
    }
    return fun (value: int): int {
        var callback = captured;
        var remaining = value;
        while (remaining > 0) {
            callback = captured;
            remaining -= 1;
        }
        return callback(remaining);
    };
}

fun audit(flag: bool, value: int): int {
    return factory(flag)(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let symbol = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let lambda = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.kind == "lambda")
            .expect("lambda symbol");
        let callback_call = snapshot
            .call_sites
            .iter()
            .find(|call| call.caller == lambda.id && call.dispatch == "indirect")
            .expect("captured callback call");
        assert!(callback_call.complete);
        assert_eq!(
            callback_call
                .targets
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([symbol("allow").id.clone(), symbol("reject").id.clone()])
        );

        let graph = snapshot
            .control_flow_graphs
            .iter()
            .find(|graph| graph.symbol_id == lambda.id)
            .expect("lambda CFG");
        assert!(graph.edges.iter().any(|edge| edge.kind == "loopBack"));
        let captured = snapshot
            .symbols
            .iter()
            .find(|candidate| candidate.name == "captured")
            .expect("captured local symbol");
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| node.reads.contains(&captured.id))
        );
    }

    #[test]
    fn resolves_nested_lambda_parameters_and_transitive_captures() {
        let source = r#"
fun allow(value: int): int { return value + 1; }
fun reject(value: int): int { return value + 2; }

fun makeBinder() {
    return fun (callback: ((int) -> int)): ((int) -> int) {
        return fun (value: int): int {
            return callback(value);
        };
    };
}

fun audit(value: int): int {
    val binder = makeBinder();
    val first = binder(allow);
    val second = binder(reject);
    return first(value) + second(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let globals = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let mut lambdas = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == "lambda")
            .collect::<Vec<_>>();
        lambdas.sort_by_key(|lambda| lambda.declaration.byte_range.start);
        assert_eq!(lambdas.len(), 2);
        assert_eq!(lambdas[1].containing_symbol.as_ref(), Some(&lambdas[0].id));

        let inner_call = snapshot
            .call_sites
            .iter()
            .find(|call| call.caller == lambdas[1].id)
            .expect("inner lambda callback call");
        assert!(inner_call.complete);
        assert_eq!(
            inner_call.targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([globals("allow").id.clone(), globals("reject").id.clone(),])
        );

        for lambda in lambdas {
            assert!(
                snapshot
                    .control_flow_graphs
                    .iter()
                    .any(|graph| graph.symbol_id == lambda.id)
            );
        }
    }

    #[test]
    fn captures_callable_values_at_lambda_creation_time() {
        let source = r#"
fun allow(value: int): int { return value; }
fun reject(value: int): int { return value + 1; }

fun audit(value: int): int {
    var action = allow;
    val callback = fun (inner: int): int {
        return action(inner);
    };
    action = reject;
    return callback(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let lambda = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.kind == "lambda")
            .expect("lambda symbol");
        let allow = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "allow" && !symbol.flags.local)
            .expect("allow symbol");
        let call = snapshot
            .call_sites
            .iter()
            .find(|call| call.caller == lambda.id)
            .expect("captured call");
        assert!(call.complete);
        assert_eq!(call.targets, vec![allow.id.clone()]);
    }

    #[test]
    fn resolves_callback_flow_across_file_and_struct_field_boundaries() {
        let library = r#"
struct Handlers {
    primary: ((int) -> int)
    fallback: ((int) -> int)
}

fun forward<T>(value: T): T { return value; }
"#;
        let application = r#"
import "handlers";

fun allow(value: int): int { return value; }
fun reject(value: int): int { throw value; }

fun audit(flag: bool, value: int): int {
    var handlers = Handlers { primary: allow, fallback: reject };
    if (flag) {
        handlers.primary = reject;
    }
    val forwarded = forward(handlers);
    return forwarded.primary(value);
}
"#;
        let snapshot = project(
            &[
                ("/project/handlers.tolk", library),
                ("/project/main.tolk", application),
            ],
            &["/project/main.tolk"],
        );
        let global = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name && !symbol.flags.local)
                .expect("global symbol")
        };
        let audit = global("audit");
        let call = snapshot
            .call_sites
            .iter()
            .find(|call| call.caller == audit.id && call.dispatch == "indirect")
            .expect("struct-held callback call");
        assert!(call.complete);
        assert_eq!(
            call.targets.iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([global("allow").id.clone(), global("reject").id.clone()])
        );
    }

    #[test]
    fn leaves_a_truly_external_callback_origin_incomplete() {
        let source = r#"
fun forward(callback: ((int) -> int)): ((int) -> int) {
    return callback;
}

fun publicEntry(callback: ((int) -> int), value: int): int {
    val returned = forward(callback);
    return returned(value);
}
"#;
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let entry = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "publicEntry" && !symbol.flags.local)
            .expect("entry symbol");
        let call = snapshot
            .call_sites
            .iter()
            .find(|call| call.caller == entry.id && call.dispatch == "indirect")
            .expect("external callback call site");
        assert!(!call.complete);
        assert!(call.targets.is_empty());
    }

    #[test]
    fn reports_missing_import_at_the_import_and_keeps_partial_results() {
        let snapshot = project(
            &[("/project/main.tolk", "import \"missing\"; fun main() {}")],
            &["/project/main.tolk"],
        );
        let diagnostic = snapshot
            .diagnostics
            .iter()
            .find(|item| item.code.as_deref() == Some("unresolved-import"))
            .unwrap();
        assert_eq!(
            diagnostic.location.as_ref().unwrap().byte_range,
            ByteRange { start: 0, end: 16 }
        );
        assert!(snapshot.symbols.iter().any(|symbol| symbol.name == "main"));
    }

    #[test]
    fn parse_diagnostic_positions_use_utf16_not_utf8_columns() {
        let source = "fun main() { val text = \"😀\"; return ( }";
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let location = snapshot
            .diagnostics
            .iter()
            .find(|item| item.phase == "parse")
            .and_then(|item| item.location.as_ref())
            .unwrap();
        assert_eq!(location.range.start.line, 0);
        assert!(location.byte_range.start > location.range.start.character);
    }

    #[test]
    fn exposes_linter_diagnostics_annotations_and_fixes() {
        let source = "fun main() {\n    val unused = 1;\n}";
        let snapshot = project(&[("/project/main.tolk", source)], &["/project/main.tolk"]);
        let diagnostic = snapshot
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_deref() == Some("E001"))
            .expect("unused variable diagnostic");

        assert_eq!(diagnostic.phase, "lint");
        assert_eq!(diagnostic.source, "tolk-linter");
        assert_eq!(diagnostic.severity, "warning");
        assert_eq!(
            diagnostic
                .location
                .as_ref()
                .map(|location| location.path.as_str()),
            Some("/project/main.tolk")
        );
        assert!(diagnostic.annotations.iter().any(|annotation| {
            annotation.primary
                && annotation.tags.iter().any(|tag| tag == "unnecessary")
                && annotation.message.as_deref() == Some("unused variable `unused`")
        }));
        assert!(diagnostic.fixes.iter().any(|fix| {
            fix.applicability == "automatic"
                && fix.edits.iter().any(|edit| {
                    edit.replacement == "_unused" && edit.location.path == "/project/main.tolk"
                })
        }));
    }

    #[test]
    fn honors_linter_suppressions_and_does_not_lint_dependencies() {
        let snapshot = inspect(ProjectInput {
            root: "/project".into(),
            files: [
                (
                    "/project/main.tolk".into(),
                    "fun main() {\n    // check-disable-next-line unused-variable\n    val localUnused = 1;\n}"
                        .into(),
                ),
                (
                    "/project/stdlib/common.tolk".into(),
                    "fun helper() { val dependencyUnused = 1; }".into(),
                ),
            ]
            .into_iter()
            .collect(),
            entrypoints: vec!["/project/main.tolk".into()],
            stdlib_root: Some("/project/stdlib".into()),
            acton_stdlib_root: None,
            import_mappings: BTreeMap::new(),
            control_flow: ControlFlowScope::Workspace,
        })
        .unwrap();

        assert!(!snapshot.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_deref() == Some("E001")
                && diagnostic.location.as_ref().is_some_and(|location| {
                    location.path == "/project/main.tolk"
                        || location.path == "/project/stdlib/common.tolk"
                })
        }));
    }

    #[test]
    fn resolves_normalized_import_mappings_from_memory() {
        let snapshot = inspect(ProjectInput {
            root: "/project".into(),
            files: [
                (
                    "/project/main.tolk".into(),
                    "import \"@pkg/lib\"; fun main() { mapped(); }".into(),
                ),
                ("/vendor/lib.tolk".into(), "fun mapped() { return; }".into()),
            ]
            .into_iter()
            .collect(),
            entrypoints: vec!["main.tolk".into()],
            stdlib_root: None,
            acton_stdlib_root: None,
            import_mappings: [("pkg".into(), "/vendor".into())].into_iter().collect(),
            control_flow: ControlFlowScope::Workspace,
        })
        .unwrap();
        assert_eq!(snapshot.files.len(), 2);
        assert_eq!(snapshot.files[0].path, "/project/main.tolk");
        assert_eq!(
            snapshot.files[0].imports[0].target_path.as_deref(),
            Some("/vendor/lib.tolk")
        );
        assert_eq!(snapshot.call_graph.len(), 1);
    }
}
