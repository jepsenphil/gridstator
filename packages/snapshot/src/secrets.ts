import picomatch from 'picomatch';

/** Files that commonly hold credentials. Excluded even when tracked by git (ADR-12). */
export const SECRET_PATTERNS: readonly string[] = [
  '**/.env',
  '**/.env.*',
  '**/*.pem',
  '**/*.key',
  '**/*.p12',
  '**/*.pfx',
  '**/*.jks',
  '**/*.keystore',
  '**/id_rsa',
  '**/id_rsa.*',
  '**/id_ecdsa',
  '**/id_ecdsa.*',
  '**/id_ed25519',
  '**/id_ed25519.*',
  '**/.netrc',
  // act's conventional secrets file.
  '**/.secrets',
  '**/.pypirc',
  '**/.ssh/**',
  '**/.aws/**',
  '**/.docker/config.json',
];

/** Conventional non-secret files that match the patterns above. */
export const SECRET_EXCEPTIONS: readonly string[] = [
  '**/.env.example',
  '**/.env.sample',
  '**/.env.template',
  '**/.env.defaults',
  '**/*.pub',
];

const NPMRC_AUTH = /(^|[\s:])(_authToken|_auth|_password)\s*=/m;

export interface SecretFilter {
  /** Returns a reason when the path must be excluded as a secret. */
  check(path: string, readContent: () => Promise<string>): Promise<string | undefined>;
}

export function createSecretFilter(allow: readonly string[] = []): SecretFilter {
  const isSecret = picomatch([...SECRET_PATTERNS], { dot: true });
  const isException = picomatch([...SECRET_EXCEPTIONS], { dot: true });
  const isAllowed = allow.length > 0 ? picomatch([...allow], { dot: true }) : () => false;
  return {
    async check(path, readContent) {
      if (isAllowed(path)) return undefined;
      if (isSecret(path) && !isException(path)) return 'matches a secret file pattern';
      if (path === '.npmrc' || path.endsWith('/.npmrc')) {
        const content = await readContent();
        if (NPMRC_AUTH.test(content)) return '.npmrc contains registry credentials';
      }
      return undefined;
    },
  };
}
