import wasm from "../generated/tolk_inspect_wasm.cjs";

export type NodeId = string;
export type SymbolId = string;
export type TypeId = string;
export type ControlFlowNodeId = string;
export type ControlFlowScope = "none" | "workspace" | "all";

export interface ProjectInput {
  root: string;
  files: Readonly<Record<string, string>> | ReadonlyMap<string, string>;
  entrypoints?: readonly string[];
  stdlibRoot?: string;
  actonStdlibRoot?: string;
  importMappings?: Readonly<Record<string, string>>;
  /** Controls CFG generation. Defaults to `workspace`. */
  controlFlow?: ControlFlowScope;
}

export interface Position { readonly line: number; readonly character: number }
export interface SourceRange { readonly start: Position; readonly end: Position }
export interface ByteRange { readonly start: number; readonly end: number }
export interface SourceLocation {
  readonly path: string;
  readonly range: SourceRange;
  readonly byteRange: ByteRange;
}

export interface VersionInfo {
  readonly packageVersion: string;
  readonly actonRevision: string;
  readonly tolkVersion: string;
}

export interface SymbolFlags {
  readonly deprecated: boolean;
  readonly pure: boolean;
  readonly private: boolean;
  readonly mutable: boolean;
  readonly local: boolean;
  readonly parameter: boolean;
}

export interface SymbolInfo {
  readonly id: SymbolId;
  readonly name: string;
  readonly fqn: string;
  readonly kind: string;
  readonly declaration: SourceLocation;
  readonly body?: SourceLocation;
  readonly containingSymbol?: SymbolId;
  readonly documentation?: string;
  readonly flags: SymbolFlags;
  readonly nodeId?: NodeId;
}

export interface ReferenceContext {
  readonly namespace: "value" | "type" | "mixed";
  readonly usage: "call" | "read" | "write" | "mutate";
  /** Combinable access facts computed by Acton's `tolk-analysis`. */
  readonly access: ReferenceAccess;
}

export interface ReferenceAccess {
  readonly read: boolean;
  readonly write: boolean;
  readonly mutate: boolean;
}

export interface Reference {
  readonly symbolId?: SymbolId;
  readonly name: string;
  readonly location: SourceLocation;
  readonly context: ReferenceContext;
  readonly nodeId?: NodeId;
  readonly resolved: boolean;
}

export interface Resolution {
  readonly nodeId: NodeId;
  readonly symbolId?: SymbolId;
  readonly symbol?: SymbolInfo;
  readonly resolved: boolean;
}

export interface TypeInfo {
  readonly id: TypeId;
  readonly display: string;
  readonly kind: string;
  readonly symbolId?: SymbolId;
  readonly elementTypes: readonly TypeId[];
  readonly returnType?: TypeId;
}

export type ConstantValue =
  | { readonly kind: "int"; readonly value: string; readonly display: string }
  | { readonly kind: "bool"; readonly value: boolean; readonly display: string }
  | { readonly kind: "string"; readonly value: string; readonly display: string }
  | { readonly kind: "overflow"; readonly display: string }
  | { readonly kind: "unknown"; readonly display: string };

export type ControlFlowNodeKind =
  | "entry" | "exit" | "nop" | "expression" | "condition" | "assert"
  | "return" | "throw" | "break" | "continue" | "matchPattern"
  | "catchBinding" | "join";

export type ControlFlowEdgeKind =
  | "unconditional" | "trueBranch" | "falseBranch" | "loopBack"
  | "break" | "continue" | "return" | "throw" | "exceptional";

export interface ControlFlowNode {
  readonly id: ControlFlowNodeId;
  readonly kind: ControlFlowNodeKind;
  readonly location?: SourceLocation;
  readonly astNodeId?: NodeId;
  readonly reads: readonly SymbolId[];
  readonly writes: readonly SymbolId[];
}

export interface ControlFlowEdge {
  readonly from: ControlFlowNodeId;
  readonly to: ControlFlowNodeId;
  readonly kind: ControlFlowEdgeKind;
}

export interface CallEdge {
  readonly caller: SymbolId;
  readonly callee: SymbolId;
  readonly callSite: SourceLocation;
  readonly nodeId?: NodeId;
}

export interface Diagnostic {
  readonly phase: "parse" | "project" | "resolution" | "type";
  readonly source: string;
  readonly severity: "error" | "warning" | "information" | "hint";
  readonly code?: string;
  readonly message: string;
  readonly location?: SourceLocation;
}

export interface ImportInfo {
  readonly path: string;
  readonly targetPath?: string;
  readonly location: SourceLocation;
}

interface RawAstNode {
  id: NodeId; kind: string; rawKind: string; named: boolean; error: boolean;
  parentId?: NodeId; childIds: NodeId[]; fields: Record<string, NodeId[]>;
  location: SourceLocation; text: string;
}

interface RawSourceFile {
  path: string; source: string; rootNode: NodeId;
  sourceKind: "workspace" | "stdlib" | "acton"; imports: ImportInfo[];
}

interface RawResolution { nodeId: NodeId; symbolId?: SymbolId; resolved: boolean }
interface RawNodeType { nodeId: NodeId; typeId: TypeId }
interface RawSymbolConstantValue { symbolId: SymbolId; value: ConstantValue }
interface RawControlFlowGraph {
  symbolId: SymbolId; entry: ControlFlowNodeId; exit: ControlFlowNodeId;
  nodes: ControlFlowNode[]; edges: ControlFlowEdge[];
}
interface Snapshot {
  version: VersionInfo; root: string; files: RawSourceFile[]; nodes: RawAstNode[];
  symbols: SymbolInfo[]; references: Reference[]; resolutions: RawResolution[];
  types: TypeInfo[]; nodeTypes: RawNodeType[]; constantValues: RawSymbolConstantValue[];
  controlFlowGraphs: RawControlFlowGraph[]; callGraph: CallEdge[]; diagnostics: Diagnostic[];
}

export class AstNode {
  readonly id: NodeId;
  readonly kind: string;
  readonly rawKind: string;
  readonly named: boolean;
  readonly error: boolean;
  readonly location: SourceLocation;
  readonly text: string;
  readonly #project: InspectedProject;
  readonly #parentId?: NodeId;
  readonly #childIds: readonly NodeId[];
  readonly #fields: Readonly<Record<string, readonly NodeId[]>>;

  /** @internal */
  constructor(project: InspectedProject, raw: RawAstNode) {
    this.#project = project;
    this.id = raw.id;
    this.kind = raw.kind;
    this.rawKind = raw.rawKind;
    this.named = raw.named;
    this.error = raw.error;
    this.location = raw.location;
    this.text = raw.text;
    this.#parentId = raw.parentId;
    this.#childIds = raw.childIds;
    this.#fields = raw.fields;
  }

  get parent(): AstNode | undefined { return this.#parentId ? this.#project.node(this.#parentId) : undefined }
  get children(): readonly AstNode[] { return this.#childIds.map((id) => this.#project.node(id)).filter(isDefined) }
  get name(): AstNode | undefined { return this.childForFieldName("name") }
  get body(): AstNode | undefined { return this.childForFieldName("body") }
  get callee(): AstNode | undefined { return this.childForFieldName("callee") }
  get parameters(): readonly AstNode[] { return this.#semanticList("parameters") }
  get arguments(): readonly AstNode[] { return this.#semanticList("arguments") }

  childForFieldName(name: string): AstNode | undefined {
    const id = this.#fields[name]?.[0];
    return id ? this.#project.node(id) : undefined;
  }

  childrenForFieldName(name: string): readonly AstNode[] {
    return (this.#fields[name] ?? []).map((id) => this.#project.node(id)).filter(isDefined);
  }

  *descendants(kind?: string): IterableIterator<AstNode> {
    const stack = [...this.children].reverse();
    while (stack.length) {
      const node = stack.pop()!;
      if (kind === undefined || node.kind === kind || node.rawKind === kind) yield node;
      stack.push(...[...node.children].reverse());
    }
  }

  #semanticList(field: string): readonly AstNode[] {
    const values = this.childrenForFieldName(field);
    if (values.length !== 1) return values;
    const [value] = values;
    return value.rawKind.endsWith("_list") ? value.children : values;
  }
}

export class SourceFile {
  readonly path: string;
  readonly source: string;
  readonly sourceKind: "workspace" | "stdlib" | "acton";
  readonly imports: readonly ImportInfo[];
  readonly ast: AstNode;

  /** @internal */
  constructor(project: InspectedProject, raw: RawSourceFile) {
    this.path = raw.path;
    this.source = raw.source;
    this.sourceKind = raw.sourceKind;
    this.imports = raw.imports.map((item) => ({ ...item, targetPath: item.targetPath ?? undefined }));
    this.ast = project.node(raw.rootNode)!;
  }
}

type ControlFlowNodeLike = ControlFlowNodeId | ControlFlowNode;

export class ControlFlowGraph {
  readonly symbolId: SymbolId;
  readonly entry: ControlFlowNodeId;
  readonly exit: ControlFlowNodeId;
  readonly nodes: readonly ControlFlowNode[];
  readonly edges: readonly ControlFlowEdge[];
  readonly #nodes = new Map<ControlFlowNodeId, ControlFlowNode>();
  readonly #successors = new Map<ControlFlowNodeId, ControlFlowEdge[]>();
  readonly #predecessors = new Map<ControlFlowNodeId, ControlFlowEdge[]>();

  /** @internal */
  constructor(raw: RawControlFlowGraph) {
    this.symbolId = raw.symbolId;
    this.entry = raw.entry;
    this.exit = raw.exit;
    this.nodes = raw.nodes.map((node) => ({
      ...node,
      location: node.location ?? undefined,
      astNodeId: node.astNodeId ?? undefined,
    }));
    this.edges = raw.edges;
    for (const node of this.nodes) {
      this.#nodes.set(node.id, node);
      this.#successors.set(node.id, []);
      this.#predecessors.set(node.id, []);
    }
    for (const edge of this.edges) {
      this.#successors.get(edge.from)?.push(edge);
      this.#predecessors.get(edge.to)?.push(edge);
    }
  }

  node(id: ControlFlowNodeId): ControlFlowNode | undefined { return this.#nodes.get(id) }

  successors(node: ControlFlowNodeLike): readonly ControlFlowEdge[] {
    return this.#successors.get(controlFlowNodeId(node)) ?? [];
  }

  predecessors(node: ControlFlowNodeLike): readonly ControlFlowEdge[] {
    return this.#predecessors.get(controlFlowNodeId(node)) ?? [];
  }

  isReachable(node: ControlFlowNodeLike): boolean {
    return this.#reachableIds(this.entry).has(controlFlowNodeId(node));
  }

  reachableFrom(node: ControlFlowNodeLike): readonly ControlFlowNode[] {
    const reachable = this.#reachableIds(controlFlowNodeId(node));
    return this.nodes.filter((candidate) => reachable.has(candidate.id));
  }

  dominates(required: ControlFlowNodeLike, target: ControlFlowNodeLike): boolean {
    const requiredId = controlFlowNodeId(required);
    const targetId = controlFlowNodeId(target);
    const reachable = this.#reachableIds(this.entry);
    if (!reachable.has(requiredId) || !reachable.has(targetId)) return false;
    if (requiredId === targetId) return true;
    return !this.#reachableIds(this.entry, requiredId).has(targetId);
  }

  postDominates(required: ControlFlowNodeLike, origin: ControlFlowNodeLike): boolean {
    const requiredId = controlFlowNodeId(required);
    const originId = controlFlowNodeId(origin);
    if (!this.isReachable(originId) || !this.#reachableIds(originId).has(this.exit)) return false;
    if (requiredId === originId) return true;
    return !this.#reachableIds(originId, requiredId).has(this.exit);
  }

  #reachableIds(start: ControlFlowNodeId, blocked?: ControlFlowNodeId): Set<ControlFlowNodeId> {
    const reachable = new Set<ControlFlowNodeId>();
    if (!this.#nodes.has(start) || start === blocked) return reachable;
    const queue = [start];
    reachable.add(start);
    for (let index = 0; index < queue.length; index++) {
      for (const edge of this.#successors.get(queue[index]) ?? []) {
        if (edge.to !== blocked && !reachable.has(edge.to)) {
          reachable.add(edge.to);
          queue.push(edge.to);
        }
      }
    }
    return reachable;
  }
}

export class InspectedProject {
  readonly root: string;
  readonly version: VersionInfo;
  #disposed = false;
  #nodes = new Map<NodeId, AstNode>();
  #files = new Map<string, SourceFile>();
  #symbols = new Map<SymbolId, SymbolInfo>();
  #types = new Map<TypeId, TypeInfo>();
  #symbolByNode = new Map<NodeId, SymbolInfo>();
  #resolutionByNode = new Map<NodeId, RawResolution>();
  #typeByNode = new Map<NodeId, TypeInfo>();
  #constantBySymbol = new Map<SymbolId, ConstantValue>();
  #controlFlowBySymbol = new Map<SymbolId, ControlFlowGraph>();
  #references: readonly Reference[];
  #calls: readonly CallEdge[];
  #diagnostics: readonly Diagnostic[];

  /** @internal */
  constructor(snapshot: Snapshot) {
    this.root = snapshot.root;
    this.version = snapshot.version;
    for (const raw of snapshot.nodes) this.#nodes.set(raw.id, new AstNode(this, raw));
    for (const raw of snapshot.files) this.#files.set(raw.path, new SourceFile(this, raw));
    for (const rawSymbol of snapshot.symbols) {
      const symbol = {
        ...rawSymbol,
        body: rawSymbol.body ?? undefined,
        containingSymbol: rawSymbol.containingSymbol ?? undefined,
        documentation: rawSymbol.documentation ?? undefined,
        nodeId: rawSymbol.nodeId ?? undefined,
      };
      this.#symbols.set(symbol.id, symbol);
      if (symbol.nodeId) this.#symbolByNode.set(symbol.nodeId, symbol);
    }
    for (const rawType of snapshot.types) {
      const type = { ...rawType, symbolId: rawType.symbolId ?? undefined, returnType: rawType.returnType ?? undefined };
      this.#types.set(type.id, type);
    }
    for (const resolution of snapshot.resolutions) this.#resolutionByNode.set(resolution.nodeId, resolution);
    for (const relation of snapshot.nodeTypes) {
      const type = this.#types.get(relation.typeId);
      if (type) this.#typeByNode.set(relation.nodeId, type);
    }
    for (const constant of snapshot.constantValues) this.#constantBySymbol.set(constant.symbolId, constant.value);
    for (const raw of snapshot.controlFlowGraphs) {
      const graph = new ControlFlowGraph(raw);
      this.#controlFlowBySymbol.set(graph.symbolId, graph);
    }
    this.#references = snapshot.references.map((item) => ({ ...item, symbolId: item.symbolId ?? undefined, nodeId: item.nodeId ?? undefined }));
    this.#calls = snapshot.callGraph.map((item) => ({ ...item, nodeId: item.nodeId ?? undefined }));
    this.#diagnostics = snapshot.diagnostics.map((item) => ({ ...item, code: item.code ?? undefined, location: item.location ?? undefined }));
  }

  files(): readonly SourceFile[] { this.#active(); return [...this.#files.values()] }
  file(path: string): SourceFile | undefined { this.#active(); return this.#files.get(normalizePath(this.root, path)) }
  node(id: NodeId): AstNode | undefined { this.#active(); return this.#nodes.get(id) }
  symbols(): readonly SymbolInfo[] { this.#active(); return [...this.#symbols.values()] }
  symbol(id: SymbolId): SymbolInfo | undefined { this.#active(); return this.#symbols.get(id) }
  type(id: TypeId): TypeInfo | undefined { this.#active(); return this.#types.get(id) }

  symbolFor(node: NodeId | AstNode): SymbolInfo | undefined {
    this.#active();
    let current = typeof node === "string" ? this.#nodes.get(node) : node;
    while (current) {
      const symbol = this.#symbolByNode.get(current.id);
      if (symbol) return symbol;
      current = current.parent;
    }
    return undefined;
  }

  symbolAt(location: SourceLocation | { path: string; position: Position }): SymbolInfo | undefined {
    this.#active();
    const path = normalizePath(this.root, location.path);
    const position = "position" in location ? location.position : location.range.start;
    const reference = this.#references.find((item) =>
      item.location.path === path && containsPosition(item.location.range, position));
    if (reference?.symbolId) return this.#symbols.get(reference.symbolId);
    const candidates = [...this.#symbols.values()].filter((symbol) =>
      symbol.declaration.path === path && containsPosition(symbol.declaration.range, position));
    return candidates.sort((a, b) => rangeSize(a.declaration) - rangeSize(b.declaration))[0];
  }

  resolve(node: NodeId | AstNode): Resolution | undefined {
    this.#active();
    const id = typeof node === "string" ? node : node.id;
    const resolution = this.#resolutionByNode.get(id);
    if (!resolution) return undefined;
    return { ...resolution, symbol: resolution.symbolId ? this.#symbols.get(resolution.symbolId) : undefined };
  }

  references(symbol: SymbolId | SymbolInfo): readonly Reference[] {
    this.#active(); const id = typeof symbol === "string" ? symbol : symbol.id;
    return this.#references.filter((reference) => reference.symbolId === id);
  }

  typeOf(node: NodeId | AstNode): TypeInfo | undefined {
    this.#active(); return this.#typeByNode.get(typeof node === "string" ? node : node.id);
  }

  constantValue(symbol: SymbolId | SymbolInfo): ConstantValue | undefined {
    this.#active(); const id = typeof symbol === "string" ? symbol : symbol.id;
    return this.#constantBySymbol.get(id);
  }

  controlFlow(symbol: SymbolId | SymbolInfo): ControlFlowGraph | undefined {
    this.#active(); const id = typeof symbol === "string" ? symbol : symbol.id;
    return this.#controlFlowBySymbol.get(id);
  }

  controlFlowGraphs(): readonly ControlFlowGraph[] {
    this.#active(); return [...this.#controlFlowBySymbol.values()];
  }

  callGraph(): readonly CallEdge[] { this.#active(); return this.#calls }
  calls(symbol: SymbolId | SymbolInfo): readonly CallEdge[] {
    this.#active(); const id = typeof symbol === "string" ? symbol : symbol.id;
    return this.#calls.filter((call) => call.caller === id);
  }
  callers(symbol: SymbolId | SymbolInfo): readonly CallEdge[] {
    this.#active(); const id = typeof symbol === "string" ? symbol : symbol.id;
    return this.#calls.filter((call) => call.callee === id);
  }
  diagnostics(): readonly Diagnostic[] { this.#active(); return this.#diagnostics }

  dispose(): void {
    this.#disposed = true;
    this.#nodes.clear(); this.#files.clear(); this.#symbols.clear(); this.#types.clear();
    this.#symbolByNode.clear(); this.#resolutionByNode.clear(); this.#typeByNode.clear();
    this.#constantBySymbol.clear();
    this.#controlFlowBySymbol.clear();
    this.#references = []; this.#calls = []; this.#diagnostics = [];
  }

  #active(): void { if (this.#disposed) throw new Error("This tolk-inspect project has been disposed") }
}

export async function inspectProject(input: ProjectInput): Promise<InspectedProject> {
  const files = input.files instanceof Map ? Object.fromEntries(input.files) : { ...input.files };
  const wireInput = {
    root: input.root,
    files,
    entrypoints: [...(input.entrypoints ?? [])],
    ...(input.stdlibRoot === undefined ? {} : { stdlibRoot: input.stdlibRoot }),
    ...(input.actonStdlibRoot === undefined ? {} : { actonStdlibRoot: input.actonStdlibRoot }),
    importMappings: { ...(input.importMappings ?? {}) },
    controlFlow: input.controlFlow ?? "workspace",
  };
  return new InspectedProject(wasm.inspectProjectSnapshot(wireInput) as Snapshot);
}

export function versionInfo(): VersionInfo { return wasm.versionInfo() as VersionInfo }

function isDefined<T>(value: T | undefined): value is T { return value !== undefined }
function controlFlowNodeId(node: ControlFlowNodeLike): ControlFlowNodeId {
  return typeof node === "string" ? node : node.id;
}
function containsPosition(range: SourceRange, position: Position): boolean {
  return comparePosition(range.start, position) <= 0 && comparePosition(position, range.end) <= 0;
}
function comparePosition(left: Position, right: Position): number {
  return left.line - right.line || left.character - right.character;
}
function rangeSize(location: SourceLocation): number { return location.byteRange.end - location.byteRange.start }
function normalizePath(root: string, path: string): string {
  const parts = (path.startsWith("/") ? path : `${root}/${path}`).split("/");
  const normalized: string[] = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") normalized.pop(); else normalized.push(part);
  }
  return `/${normalized.join("/")}`;
}
