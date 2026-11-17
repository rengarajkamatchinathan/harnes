// Attachments go to the daemon as blobs (`blob.put.begin/chunk/end`) before the prompt
// references them (05 §2.3). SA-27: 32 MB per file, 20 files per message.
import type { GenieClient } from "@genie/protocol";

export const MAX_FILE_BYTES = 32 * 1024 * 1024;
export const MAX_FILES = 20;
const CHUNK = 1024 * 1024;

export interface Attachment {
  key: string;
  name: string;
  mime: string;
  size: number;
  state: "uploading" | "ready" | "error";
  progress: number;
  blobId?: string;
  error?: string;
}

export function toBase64(bytes: Uint8Array): string {
  let s = "";
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) s += String.fromCharCode(...bytes.subarray(i, i + step));
  return btoa(s);
}

export function fromBase64(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

/** Why a file can't be attached, or null. */
export function rejectReason(file: { size: number }, alreadyAttached: number): string | null {
  if (alreadyAttached >= MAX_FILES) return `At most ${MAX_FILES} files per message.`;
  if (file.size > MAX_FILE_BYTES) return "Files over 32 MB can't be attached.";
  return null;
}

export function guessMime(name: string, given?: string): string {
  if (given) return given;
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = { md: "text/markdown", txt: "text/plain", ts: "text/plain", tsx: "text/plain", rs: "text/plain", py: "text/plain", json: "application/json", pdf: "application/pdf", png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", csv: "text/csv" };
  return map[ext] ?? "application/octet-stream";
}

export async function uploadBlob(client: GenieClient, file: Blob & { name?: string }, onProgress?: (fraction: number) => void): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const { upload_id } = await client.blob.put.begin({ size: bytes.length, name: file.name ?? null, mime: file.type || null });
  for (let off = 0; off < bytes.length; off += CHUNK) {
    await client.blob.put.chunk({ upload_id, offset: off, data_b64: toBase64(bytes.subarray(off, off + CHUNK)) });
    onProgress?.(Math.min(1, (off + CHUNK) / Math.max(1, bytes.length)));
  }
  const { blob_id } = await client.blob.put.end({ upload_id });
  onProgress?.(1);
  return blob_id;
}

/** Read a whole blob (for images and exports). */
export async function readBlob(client: GenieClient, blobId: string, max = 32 * 1024 * 1024): Promise<Uint8Array> {
  const parts: Uint8Array[] = [];
  let offset = 0;
  for (;;) {
    const r = await client.blob.get({ blob_id: blobId, offset, len: CHUNK });
    const data = fromBase64(r.data_b64);
    if (!data.length) break;
    parts.push(data);
    offset += data.length;
    if (offset >= r.size || offset >= max) break;
  }
  const out = new Uint8Array(offset);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
