mod model;

pub use model::*;

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tolk_analysis::{
    AnalysisDb, ConstantEvaluationContext, ConstantEvaluator, ConstantValue as ActonConstantValue,
    UseFlags,
};
use tolk_dataflow::{
    ControlFlowGraph as ActonControlFlowGraph, EdgeKind as ActonEdgeKind,
    FlowNodeKind as ActonFlowNodeKind,
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
    nodes: Vec<AstNode>,
    symbols: Vec<SymbolInfo>,
    diagnostics: Vec<Diagnostic>,
    control_flow: ControlFlowScope,
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
            nodes: vec![],
            symbols: vec![],
            diagnostics: vec![],
            control_flow,
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
        let mut analysis_db = AnalysisDb::new();
        let control_flow_graphs =
            self.collect_control_flow_graphs(&mut analysis_db, &type_db, &indexed_files);
        let (references, resolutions, call_graph) =
            self.collect_references_and_calls(&body_types, &mut analysis_db);
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
    ) -> (Vec<Reference>, Vec<Resolution>, Vec<CallEdge>) {
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

        let mut references = vec![];
        let mut resolutions = vec![];
        let mut calls = vec![];
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
            if let (Resolved::Global(_callee), Some(call_span), Some(callee_id)) =
                (usage.resolved.clone(), is_call, symbol_id.clone())
            {
                let caller = self.file_db.get_by_id(file_id).and_then(|info| {
                    info.find_symbol_at(usage.decl as usize)
                        .map(|symbol| symbol.id)
                });
                if let Some(caller) = caller {
                    calls.push(CallEdge {
                        caller: self.symbol_ids[&caller].clone(),
                        callee: callee_id,
                        call_site: self.location(file_id, call_span.start(), call_span.end()),
                        node_id: self.exact_node(file_id, call_span),
                    });
                }
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
        calls.sort_by(|a, b| call_key(a).cmp(&call_key(b)));
        calls.dedup_by(|a, b| call_key(a) == call_key(b));
        (references, resolutions, calls)
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

fn reference_key(reference: &Reference) -> (&str, u32, u32, &str) {
    (
        &reference.location.path,
        reference.location.byte_range.start,
        reference.location.byte_range.end,
        &reference.name,
    )
}
fn call_key(call: &CallEdge) -> (&str, u32, u32, &str, &str) {
    (
        &call.call_site.path,
        call.call_site.byte_range.start,
        call.call_site.byte_range.end,
        &call.caller,
        &call.callee,
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
