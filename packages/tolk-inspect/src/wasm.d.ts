declare module "*.cjs" {
  interface WasmModule {
    inspectProjectSnapshot(input: unknown): unknown;
    versionInfo(): unknown;
  }
  const value: WasmModule;
  export default value;
}

