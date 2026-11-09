import 'reflect-metadata';
import { randomBytes, webcrypto } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { hostname, networkInterfaces } from 'node:os';
import { join } from 'node:path';
import * as x509 from '@peculiar/x509';
import { certificateFingerprint, writeFileAtomic } from '@gridstator/shared';

type CryptoKey = webcrypto.CryptoKey;

x509.cryptoProvider.set(webcrypto as unknown as Parameters<typeof x509.cryptoProvider.set>[0]);

const ALG = { name: 'ECDSA', namedCurve: 'P-256', hash: 'SHA-256' } as const;
const YEAR_MS = 365 * 24 * 3600 * 1000;

export interface Pki {
  caPem: string;
  /** Leaf certificate followed by the CA, so clients receive the full chain. */
  serverChainPem: string;
  serverKeyPem: string;
  fingerprint: string;
}

export function pkiPaths(dataDir: string) {
  const dir = join(dataDir, 'pki');
  return {
    dir,
    caCert: join(dir, 'ca.pem'),
    caKey: join(dir, 'ca.key'),
    serverCert: join(dir, 'server.pem'),
    serverKey: join(dir, 'server.key'),
  };
}

async function exportKeyPem(key: CryptoKey): Promise<string> {
  const der = Buffer.from(await webcrypto.subtle.exportKey('pkcs8', key));
  const b64 = der
    .toString('base64')
    .replace(/(.{64})/g, '$1\n')
    .trim();
  return `-----BEGIN PRIVATE KEY-----\n${b64}\n-----END PRIVATE KEY-----\n`;
}

async function importKeyPem(pem: string): Promise<CryptoKey> {
  const b64 = pem.replace(/-----(BEGIN|END) PRIVATE KEY-----/g, '').replace(/\s+/g, '');
  return webcrypto.subtle.importKey('pkcs8', Buffer.from(b64, 'base64'), ALG, true, ['sign']);
}

function serial(): string {
  const bytes = randomBytes(16);
  bytes[0] = (bytes[0] ?? 0) & 0x7f; // positive
  return bytes.toString('hex');
}

/** Local addresses and names placed in the server certificate's SAN (informational; see ADR-3). */
export function localSubjectAltNames(extra: string[] = []): x509.JsonGeneralName[] {
  const names: x509.JsonGeneralName[] = [
    { type: 'dns', value: 'localhost' },
    { type: 'ip', value: '127.0.0.1' },
    { type: 'ip', value: '::1' },
  ];
  const host = hostname();
  if (host) names.push({ type: 'dns', value: host });
  for (const addrs of Object.values(networkInterfaces())) {
    for (const a of addrs ?? []) {
      if (
        !a.internal &&
        (a.family === 'IPv4' || a.family === 'IPv6') &&
        !a.address.startsWith('fe80')
      ) {
        names.push({ type: 'ip', value: a.address });
      }
    }
  }
  for (const e of extra) names.push({ type: /^[\d.:]+$/.test(e) ? 'ip' : 'dns', value: e });
  const seen = new Set<string>();
  return names.filter((n) => (seen.has(n.value) ? false : (seen.add(n.value), true)));
}

async function issueServerCert(
  caCert: x509.X509Certificate,
  caKey: CryptoKey,
  extraNames: string[],
): Promise<{ certPem: string; keyPem: string }> {
  const keys = await webcrypto.subtle.generateKey(ALG, true, ['sign', 'verify']);
  const now = Date.now();
  const cert = await x509.X509CertificateGenerator.create({
    serialNumber: serial(),
    subject: 'CN=gridstator-controller',
    issuer: caCert.subject,
    notBefore: new Date(now - 60_000),
    notAfter: new Date(now + 5 * YEAR_MS),
    signingAlgorithm: ALG,
    publicKey: keys.publicKey,
    signingKey: caKey,
    extensions: [
      new x509.BasicConstraintsExtension(false, undefined, true),
      new x509.KeyUsagesExtension(
        x509.KeyUsageFlags.digitalSignature | x509.KeyUsageFlags.keyAgreement,
        true,
      ),
      new x509.ExtendedKeyUsageExtension([x509.ExtendedKeyUsage.serverAuth]),
      new x509.SubjectAlternativeNameExtension(localSubjectAltNames(extraNames)),
      await x509.AuthorityKeyIdentifierExtension.create(caCert.publicKey),
      await x509.SubjectKeyIdentifierExtension.create(keys.publicKey),
    ],
  });
  return { certPem: cert.toString('pem') + '\n', keyPem: await exportKeyPem(keys.privateKey) };
}

/** Generates a new cluster CA and server certificate and writes them under `<dataDir>/pki`. */
export async function generatePki(
  dataDir: string,
  clusterId: string,
  extraNames: string[] = [],
): Promise<Pki> {
  const paths = pkiPaths(dataDir);
  const caKeys = await webcrypto.subtle.generateKey(ALG, true, ['sign', 'verify']);
  const now = Date.now();
  const caCert = await x509.X509CertificateGenerator.createSelfSigned({
    serialNumber: serial(),
    name: `CN=Gridstator Cluster CA ${clusterId.slice(-8)}`,
    notBefore: new Date(now - 60_000),
    notAfter: new Date(now + 10 * YEAR_MS),
    signingAlgorithm: ALG,
    keys: caKeys,
    extensions: [
      new x509.BasicConstraintsExtension(true, 0, true),
      new x509.KeyUsagesExtension(
        x509.KeyUsageFlags.keyCertSign | x509.KeyUsageFlags.cRLSign,
        true,
      ),
      await x509.SubjectKeyIdentifierExtension.create(caKeys.publicKey),
    ],
  });
  const caPem = caCert.toString('pem') + '\n';
  const server = await issueServerCert(caCert, caKeys.privateKey, extraNames);
  await writeFileAtomic(paths.caCert, caPem, 0o644);
  await writeFileAtomic(paths.caKey, await exportKeyPem(caKeys.privateKey), 0o600);
  await writeFileAtomic(paths.serverCert, server.certPem, 0o644);
  await writeFileAtomic(paths.serverKey, server.keyPem, 0o600);
  return {
    caPem,
    serverChainPem: server.certPem + caPem,
    serverKeyPem: server.keyPem,
    fingerprint: certificateFingerprint(caPem),
  };
}

/** Re-issues the server certificate from the existing CA (e.g. to add a hostname). Pins stay valid. */
export async function rotateServerCert(dataDir: string, extraNames: string[] = []): Promise<void> {
  const paths = pkiPaths(dataDir);
  const caCert = new x509.X509Certificate(await readFile(paths.caCert, 'utf8'));
  const caKey = await importKeyPem(await readFile(paths.caKey, 'utf8'));
  const server = await issueServerCert(caCert, caKey, extraNames);
  await writeFileAtomic(paths.serverCert, server.certPem, 0o644);
  await writeFileAtomic(paths.serverKey, server.keyPem, 0o600);
}

export async function loadPki(dataDir: string): Promise<Pki> {
  const paths = pkiPaths(dataDir);
  const [caPem, serverPem, serverKeyPem] = await Promise.all([
    readFile(paths.caCert, 'utf8'),
    readFile(paths.serverCert, 'utf8'),
    readFile(paths.serverKey, 'utf8'),
  ]);
  return {
    caPem,
    serverChainPem: serverPem + caPem,
    serverKeyPem,
    fingerprint: certificateFingerprint(caPem),
  };
}
