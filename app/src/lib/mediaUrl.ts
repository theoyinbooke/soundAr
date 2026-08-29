/**
 * Local media addressing for the desktop shell.
 *
 * `convertFileSrc` produces an `asset:` URL, and on Linux WebKitGTK routes media loading through
 * GStreamer, which cannot read custom URI schemes: every `<video>` fails with
 * `MEDIA_ERR_SRC_NOT_SUPPORTED` while `<img>` keeps working. The Rust shell therefore exposes a
 * loopback HTTP origin — a real scheme the media backend understands, with byte-range support so
 * scrubbing works — and injects its address before any application script runs.
 */

interface MediaEndpoint {
  origin: string;
  token: string;
}

declare global {
  interface Window {
    __SOUNDAR_MEDIA__?: MediaEndpoint | null;
  }
}

/** Percent-encode a filesystem path, matching `media_server::percent_encode_path`. */
function encodePath(path: string): string {
  return path
    .split("/")
    .map((segment) => encodeURIComponent(segment))
    .join("/");
}

export function mediaEndpoint(): MediaEndpoint | undefined {
  const endpoint = typeof window === "undefined" ? undefined : window.__SOUNDAR_MEDIA__;
  return endpoint?.origin && endpoint.token ? endpoint : undefined;
}

/**
 * Absolute URL the webview can actually play, or `undefined` when the shell exposes no media
 * origin (browser development, where the mock service already returns inline fixture URLs).
 */
export function toMediaUrl(localPath?: string): string | undefined {
  if (!localPath) return undefined;
  const endpoint = mediaEndpoint();
  if (!endpoint) return undefined;
  return `${endpoint.origin}/media/${endpoint.token}/${encodePath(localPath)}`;
}

/** Convert a value that may already be a URL, leaving non-filesystem values untouched. */
export function toMediaUrlIfPath(value?: string): string | undefined {
  return value?.startsWith("/") ? toMediaUrl(value) : value;
}
