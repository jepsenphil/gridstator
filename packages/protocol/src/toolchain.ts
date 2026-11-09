import { z } from 'zod';

/**
 * Tools the controller manages on self-updating workers. Each build pins a version and the sha256
 * of every artifact (this file is its manifest); a cluster setting can pick another version or turn
 * management off. The controller downloads ("mirrors") the artifacts once and workers fetch them
 * from the controller, so workers need no internet access.
 */
export const ToolNameSchema = z.enum(['act', 'node']);
export type ToolName = z.infer<typeof ToolNameSchema>;
export const TOOL_NAMES: readonly ToolName[] = ToolNameSchema.options;

/** Platforms with managed tools: the ones self-updating (systemd) workers run on. */
export const ToolPlatformSchema = z.enum(['linux-x64', 'linux-arm64']);
export type ToolPlatform = z.infer<typeof ToolPlatformSchema>;

export function toolPlatform(platform: string, arch: string): ToolPlatform | null {
  const p = ToolPlatformSchema.safeParse(`${platform}-${arch}`);
  return p.success ? p.data : null;
}

export interface ToolArtifact {
  /** Where the controller downloads it (a .tar.gz). */
  url: string;
  sha256: string;
  /** Path of the executable inside the archive. */
  bin: string;
}

export interface ToolPin {
  version: string;
  artifacts: Record<ToolPlatform, ToolArtifact>;
}

const ACT_ARCH: Record<ToolPlatform, string> = {
  'linux-x64': 'Linux_x86_64',
  'linux-arm64': 'Linux_arm64',
};

/** Download location of a tool version; checksums come from {@link TOOLCHAIN} or upstream. */
export function toolArtifactLocation(
  tool: ToolName,
  version: string,
  platform: ToolPlatform,
): { url: string; bin: string; checksumsUrl: string; checksumName: string } {
  if (tool === 'node') {
    const name = `node-v${version}-${platform}`;
    return {
      url: `https://nodejs.org/dist/v${version}/${name}.tar.gz`,
      bin: `${name}/bin/node`,
      checksumsUrl: `https://nodejs.org/dist/v${version}/SHASUMS256.txt`,
      checksumName: `${name}.tar.gz`,
    };
  }
  const file = `act_${ACT_ARCH[platform]}.tar.gz`;
  return {
    url: `https://github.com/nektos/act/releases/download/v${version}/${file}`,
    bin: 'act',
    checksumsUrl: `https://github.com/nektos/act/releases/download/v${version}/checksums.txt`,
    checksumName: file,
  };
}

function pin(tool: ToolName, version: string, sha256: Record<ToolPlatform, string>): ToolPin {
  const artifacts = {} as Record<ToolPlatform, ToolArtifact>;
  for (const platform of ToolPlatformSchema.options) {
    const loc = toolArtifactLocation(tool, version, platform);
    artifacts[platform] = { url: loc.url, bin: loc.bin, sha256: sha256[platform] };
  }
  return { version, artifacts };
}

/** The versions this build was tested with. */
export const TOOLCHAIN: Record<ToolName, ToolPin> = {
  // Node.js 24 LTS ("Krypton"); checksums from https://nodejs.org/dist/v24.22.2/SHASUMS256.txt
  node: pin('node', '24.22.2', {
    'linux-x64': '361718c444e49af2ca5a110c83ba6a8901143f23ab285ebbef0341f391f482a9',
    'linux-arm64': '95c264150aceb7f3437a29fd6984924f5f8d1511ea3cbd6c508b83f6c555dae2',
  }),
  // checksums from https://github.com/nektos/act/releases/download/v0.2.97/checksums.txt
  act: pin('act', '0.2.97', {
    'linux-x64': '67b4976a56f33ea7870b798912e738ec44eef38e7578ae7b532f2def1c73a5ad',
    'linux-arm64': '44d48e3d4a6409053fdec6fc6518322467d79a2d5be8f72c021b0421e2ac113d',
  }),
};

const VersionSchema = z.string().regex(/^\d+\.\d+\.\d+$/, 'expected a version like 1.2.3');

/** Cluster override per tool: null = the build's pin, "off" = do not manage, or a version. */
export const ToolOverrideSchema = z.union([z.literal('off'), VersionSchema]).nullable();
export type ToolOverride = z.infer<typeof ToolOverrideSchema>;

export const ToolchainSettingsSchema = z.object({
  act: ToolOverrideSchema,
  node: ToolOverrideSchema,
});
export type ToolchainSettings = z.infer<typeof ToolchainSettingsSchema>;

/** Tool versions an agent runs (null: act is not installed). */
export const AgentToolsSchema = z.object({
  act: z.string().max(32).nullable(),
  node: z.string().max(32),
  /** Tools the agent runs from its managed toolchain rather than the system. */
  managed: z.array(ToolNameSchema).max(8),
});
export type AgentTools = z.infer<typeof AgentToolsSchema>;

/** One tool as the controller offers it to agents (`GET /v1/agent/toolchain`). */
export const OfferedToolSchema = z.object({
  name: ToolNameSchema,
  version: z.string(),
  platform: ToolPlatformSchema,
  /** ready: download from `/v1/agent/toolchain/<name>/<version>/<platform>`. */
  state: z.enum(['ready', 'preparing', 'failed']),
  sha256: z.string().nullable(),
  size: z.number().int().nullable(),
  bin: z.string(),
  reason: z.string().nullable(),
});
export type OfferedTool = z.infer<typeof OfferedToolSchema>;

export const AgentToolchainResponseSchema = z.object({ tools: z.array(OfferedToolSchema) });
export type AgentToolchainResponse = z.infer<typeof AgentToolchainResponseSchema>;
