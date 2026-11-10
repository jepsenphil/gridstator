import { randomBytes } from 'node:crypto';
import { chmod, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
export async function ensureDir(path, mode = 0o700) {
    await mkdir(path, { recursive: true, mode });
}
/** Writes a file atomically (temp file + rename) with the given permission bits. */
export async function writeFileAtomic(path, data, mode = 0o600) {
    const dir = dirname(path);
    await ensureDir(dir);
    const tmp = join(dir, `.${randomBytes(6).toString('hex')}.tmp`);
    try {
        await writeFile(tmp, data, { mode });
        await chmod(tmp, mode);
        await rename(tmp, path);
    }
    catch (err) {
        await rm(tmp, { force: true });
        throw err;
    }
}
export async function readJsonFile(path) {
    try {
        return JSON.parse(await readFile(path, 'utf8'));
    }
    catch (err) {
        if (err.code === 'ENOENT')
            return undefined;
        throw err;
    }
}
export async function writeJsonFile(path, value, mode = 0o600) {
    await writeFileAtomic(path, JSON.stringify(value, null, 2) + '\n', mode);
}
//# sourceMappingURL=fs.js.map