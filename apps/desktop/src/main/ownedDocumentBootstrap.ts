import { isAbsolute } from 'node:path';

const MAX_BOOTSTRAP_JSON_BYTES = 8192;
const BOOTSTRAP_KEYS = [
  'version', 'registration_id', 'pid', 'start_seconds', 'start_micros', 'executable_path', 'bundle_id',
  'document_path', 'document_sha256', 'document_dev', 'document_ino', 'directory_dev', 'directory_ino',
  'issued_unix_ms',
].sort();

export interface OwnedDocumentSourceBootstrap {
  version: 1;
  registration_id: string;
  pid: number;
  start_seconds: number;
  start_micros: number;
  executable_path: string;
  bundle_id: string;
  document_path: string;
  document_sha256: string;
  document_dev: number;
  document_ino: number;
  directory_dev: number;
  directory_ino: number;
  issued_unix_ms: number;
}

/** @internal Main-process framing only; callers must derive records from an opaque active registration. */
export function encodeOwnedDocumentSourceBootstrapRecord(record: OwnedDocumentSourceBootstrap): Buffer {
  const value = record as unknown as Record<string, unknown>;
  if (Object.keys(value).sort().join(',') !== BOOTSTRAP_KEYS.join(',')
    || value.version !== 1
    || typeof value.registration_id !== 'string'
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value.registration_id)
    || !positiveSafeInteger(value.pid) || !positiveSafeInteger(value.start_seconds)
    || !Number.isSafeInteger(value.start_micros) || (value.start_micros as number) < 0 || (value.start_micros as number) >= 1_000_000
    || typeof value.executable_path !== 'string' || !isAbsolute(value.executable_path)
    || value.bundle_id !== 'com.apple.TextEdit'
    || typeof value.document_path !== 'string' || !isAbsolute(value.document_path)
    || typeof value.document_sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(value.document_sha256)
    || !nonnegativeSafeInteger(value.document_dev) || !positiveSafeInteger(value.document_ino)
    || !nonnegativeSafeInteger(value.directory_dev) || !positiveSafeInteger(value.directory_ino)
    || !positiveSafeInteger(value.issued_unix_ms)) {
    throw new Error('owned document source bootstrap facts are invalid');
  }

  const payload = Buffer.from(JSON.stringify(record), 'utf8');
  if (payload.byteLength === 0 || payload.byteLength > MAX_BOOTSTRAP_JSON_BYTES) {
    throw new Error('owned document source bootstrap exceeds its bound');
  }
  const framed = Buffer.allocUnsafe(4 + payload.byteLength);
  framed.writeUInt32BE(payload.byteLength, 0);
  payload.copy(framed, 4);
  return framed;
}

function positiveSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) > 0;
}

function nonnegativeSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}
