/** Shared by the webview and extension host; keep this module free of Node and VS Code APIs. */
export function formatConnectionCommand(
  kind: 'client' | 'join',
  url: string,
  fingerprint: string,
  token: string,
): string {
  const quote = (value: string) => `'${value.replace(/'/g, "'\\''")}'`;
  return `gridstator ${kind === 'join' ? 'agent join' : 'login'} ${quote(url)} --fingerprint ${quote(fingerprint)} --token ${quote(token)}`;
}

/** A command copied to another machine must use an address that refers to the controller there. */
export function workerControllerUrlError(value: string): string | null {
  const invalid =
    'Enter an HTTPS controller URL without credentials, a path, query parameters or spaces.';
  let url: URL;
  try {
    if (!/^https:\/\/[^/\\?#\s]+\/?$/i.test(value) || value.includes('\0')) return invalid;
    url = new URL(value);
  } catch {
    return invalid;
  }
  if (
    url.protocol !== 'https:' ||
    !url.hostname ||
    url.username ||
    url.password ||
    (url.pathname && url.pathname !== '/')
  )
    return invalid;
  if (url.port === '0') return 'Enter a controller listening port from 1 to 65535.';

  // URL canonicalizes alternate IPv4 spellings and expanded IPv6 addresses before this check.
  const hostname = url.hostname.toLowerCase().replace(/\.$/, '');
  if (
    hostname.includes('*') ||
    hostname === 'localhost' ||
    hostname.endsWith('.localhost') ||
    /^127(?:\.\d+){3}$/.test(hostname) ||
    hostname === '0.0.0.0' ||
    hostname === '[::]' ||
    hostname === '[::1]' ||
    /^\[::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}\]$/.test(hostname) ||
    hostname === '[::ffff:0:0]'
  )
    return 'Use the controller’s LAN IP address or hostname so the other machine can reach it.';
  return null;
}
