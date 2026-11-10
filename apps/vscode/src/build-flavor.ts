// Replaced by esbuild for the native v2 artifact. Unbundled tests and the retained v1 build use false.
declare const __GRIDSTATOR_NATIVE_ONLY__: boolean;

export function nativeExtensionBuild(): boolean {
  return typeof __GRIDSTATOR_NATIVE_ONLY__ !== 'undefined' && __GRIDSTATOR_NATIVE_ONLY__;
}
