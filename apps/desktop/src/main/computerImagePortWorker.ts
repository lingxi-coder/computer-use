import { parentPort } from 'node:worker_threads';

import { encodeComputerImagePacket } from './computerImageWire.js';
import type { ComputerFrameRequest } from '@computer-use/desktop-api';
import type { ImagePacket } from './imageWire.js';

interface EncodeMessage {
  request: ComputerFrameRequest;
  image: { meta: ImagePacket['meta']; bytes: ArrayBuffer };
  nowMs: number;
}

const port = parentPort!;

port.on('message', (message: EncodeMessage) => {
  try {
    const encoded = encodeComputerImagePacket(message.request, {
      meta: message.image.meta,
      bytes: new Uint8Array(message.image.bytes),
    }, message.nowMs);
    // Transfer only an exact-sized ArrayBuffer; never expose a Buffer slab.
    const transferable = new Uint8Array(encoded.byteLength);
    transferable.set(encoded);
    port.postMessage({ kind: 'encoded', bytes: transferable.buffer }, [transferable.buffer]);
  } catch {
    port.postMessage({ kind: 'failed' });
  }
});
