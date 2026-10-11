import { useEffect, useState } from 'react';
import type { FrameView, ImageAvailability } from '@computer-use/desktop-api';

/** Pixel requests depend on image identity, never on the heartbeat/age ticks. */
export function FramePreview({ frame, availability }: { frame: FrameView | null | undefined; availability: ImageAvailability | null }) {
  const frameId = frame?.frame_id ?? null;
  const streamId = frame?.stream_id ?? null;
  const healthy = frame?.healthy ?? false;
  const availableId = availability?.frame_id ?? null;
  const key = `${streamId}/${frameId}`;
  const [preview, setPreview] = useState<{ key: string; url: string; expires: number } | null>(null);
  const [expired, setExpired] = useState(true);
  useEffect(() => {
    let current = true; let url: string | null = null;
    setPreview(null);
    if (frameId && streamId && healthy) {
      void window.desktop.image(frameId).then((image) => {
        if (!current || !image || image.frame_id !== frameId || image.stream_id !== streamId || image.expires_unix_ms <= Date.now()) return;
        const bytes = Uint8Array.from(image.bytes);
        url = URL.createObjectURL(new Blob([bytes], { type: 'image/png' }));
        setPreview({ key, url, expires: image.expires_unix_ms });
      }).catch(() => { if (current) setPreview(null); });
    }
    return () => { current = false; if (url) URL.revokeObjectURL(url); };
  }, [frameId, streamId, healthy, availableId]);

  const expires = availableId === frameId ? availability?.expires_unix_ms ?? preview?.expires ?? 0 : preview?.expires ?? 0;
  useEffect(() => {
    const remaining = expires - Date.now();
    setExpired(remaining <= 0);
    if (remaining <= 0) return;
    const timer = setTimeout(() => setExpired(true), remaining);
    return () => clearTimeout(timer);
  }, [expires]);
  const visible = healthy && preview?.key === key && !expired;
  return visible ? <img className="observation-image" src={preview.url} alt="Observed target window"
    width={frame?.width_px} height={frame?.height_px} onError={() => { URL.revokeObjectURL(preview.url); setPreview(null); }} />
    : <div className="source-pending image-pending">{healthy ? 'Waiting for image' : 'Image preview unavailable'}</div>;
}
