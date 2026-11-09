/** Simulates real work (compilation, I/O, a browser test) so sharding has something to save. */
export function work(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function checksum(text) {
  let h = 0;
  for (const ch of text) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
  return h;
}
