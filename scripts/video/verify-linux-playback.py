#!/usr/bin/env python3
"""Prove that an exported master actually decodes in the engine the desktop app ships with.

Every existing suite runs against Chromium (Playwright) or jsdom (Vitest) with a mock service that
serves an inline `data:` URL, so all of them pass while the real product plays nothing. This check
loads a real MP4 into a real WebKitGTK view through the real media origin and asserts the media
element reaches `readyState >= HAVE_FUTURE_DATA`.

It also asserts the negative case: the same file behind an `asset:` URL must fail. That is the
WebKitGTK limitation the media origin exists to route around — GStreamer, which backs `<video>`,
cannot read custom URI schemes — and pinning it here means a future revert to `convertFileSrc`
fails loudly instead of silently shipping a black rectangle.

Usage:
    scripts/video/verify-linux-playback.py [path/to/master.mp4]

With no argument a short H.264/AAC clip is generated with FFmpeg. Exits non-zero on failure.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import quote

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
PROBE_TIMEOUT_SECONDS = 20
READY_STATE_HAVE_FUTURE_DATA = 3


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def skip(message: str) -> None:
    print(f"SKIP: {message}")
    raise SystemExit(0)


def build_fixture(directory: Path) -> Path:
    """Render a short portrait H.264/AAC clip matching what Video Studio exports."""
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        skip("ffmpeg is not installed, so no fixture can be rendered")
    target = directory / "fixture-master.mp4"
    subprocess.run(
        [
            ffmpeg, "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=270x480:rate=30:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-profile:v", "high",
            "-c:a", "aac", "-shortest", "-movflags", "+faststart",
            str(target),
        ],
        check=True,
    )
    return target


def build_probe() -> Path:
    """Build the helper that starts the shipping media origin."""
    manifest = REPOSITORY_ROOT / "app" / "src-tauri" / "Cargo.toml"
    subprocess.run(
        ["cargo", "build", "--quiet", "--manifest-path", str(manifest), "--example", "media-origin-probe"],
        check=True,
    )
    probe = manifest.parent / "target" / "debug" / "examples" / "media-origin-probe"
    if not probe.is_file():
        fail(f"the media origin probe was not produced at {probe}")
    return probe


def play_in_webkit(url: str, register_asset_scheme_for: Path | None = None) -> str:
    """Load `url` into a WebKitGTK media element and report the outcome as a single line."""
    try:
        import gi

        gi.require_version("Gtk", "3.0")
        gi.require_version("WebKit2", "4.1")
        from gi.repository import Gio, GLib, Gtk, WebKit2
    except (ImportError, ValueError) as error:
        skip(f"WebKitGTK Python bindings are unavailable ({error})")

    context = WebKit2.WebContext.new_ephemeral()
    security = context.get_security_manager()
    security.register_uri_scheme_as_secure("asset")
    security.register_uri_scheme_as_cors_enabled("asset")

    def serve_asset(request: "WebKit2.URISchemeRequest") -> None:
        path = register_asset_scheme_for
        if path is None:
            request.finish_error(GLib.Error.new_literal(GLib.quark_from_string("probe"), "no file", 1))
            return
        stream = Gio.File.new_for_path(str(path)).read(None)
        response = WebKit2.URISchemeResponse.new(stream, path.stat().st_size)
        response.set_content_type("video/mp4")
        request.finish_with_response(response)

    context.register_uri_scheme("asset", lambda request: serve_asset(request))

    window = Gtk.Window()
    window.set_default_size(320, 240)
    view = WebKit2.WebView.new_with_context(context)
    window.add(view)

    outcome: list[str] = []

    def on_message(_manager, message) -> None:
        try:
            value = message.get_js_value().to_string()
        except Exception:  # noqa: BLE001 - the bindings raise bare exceptions here
            value = str(message)
        outcome.append(value)
        GLib.timeout_add(50, Gtk.main_quit)

    manager = view.get_user_content_manager()
    manager.connect("script-message-received::probe", on_message)
    manager.register_script_message_handler("probe")

    html = f"""<html><body><script>
      const report = (text) => window.webkit.messageHandlers.probe.postMessage(text);
      const video = document.createElement('video');
      video.preload = 'auto';
      video.muted = true;
      document.body.appendChild(video);
      let settled = false;
      const settle = (text) => {{ if (!settled) {{ settled = true; report(text); }} }};
      video.addEventListener('canplay', () => settle('READY readyState=' + video.readyState
        + ' duration=' + video.duration));
      video.addEventListener('error', () => settle('ERROR code='
        + (video.error && video.error.code)));
      video.src = {url!r};
      setTimeout(() => settle('TIMEOUT readyState=' + video.readyState
        + ' networkState=' + video.networkState), {PROBE_TIMEOUT_SECONDS * 1000 - 2000});
    </script></body></html>"""

    view.load_html(html, "tauri://localhost/")
    window.show_all()
    GLib.timeout_add(PROBE_TIMEOUT_SECONDS * 1000, Gtk.main_quit)
    Gtk.main()
    window.destroy()
    return outcome[0] if outcome else "NO RESULT"


def main() -> int:
    if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        skip("no display is available for a WebKitGTK view")

    with tempfile.TemporaryDirectory(prefix="soundar-playback-") as workspace:
        directory = Path(workspace)
        if len(sys.argv) > 1:
            media = Path(sys.argv[1]).resolve()
            if not media.is_file():
                fail(f"{media} is not a file")
            root = media.parent
        else:
            media = build_fixture(directory)
            root = directory

        probe = build_probe()
        process = subprocess.Popen(
            [str(probe), str(root), str(media)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            url = (process.stdout.readline() if process.stdout else "").strip()
            if not url:
                fail("the media origin did not report a URL")
            print(f"media origin URL: {url}")

            served = play_in_webkit(url)
            print(f"media origin  -> {served}")

            asset_url = "asset://localhost/" + quote(str(media))
            refused = play_in_webkit(asset_url, register_asset_scheme_for=media)
            print(f"asset protocol -> {refused}")
        finally:
            if process.stdin:
                process.stdin.close()
            process.wait(timeout=10)

    if not served.startswith("READY"):
        fail(
            "the exported master did not decode over the media origin. Video playback is broken in "
            f"the shipping engine: {served}"
        )
    ready_state = int(served.split("readyState=")[1].split()[0])
    if ready_state < READY_STATE_HAVE_FUTURE_DATA:
        fail(f"readyState {ready_state} is below HAVE_FUTURE_DATA; playback would stall")

    if not refused.startswith("ERROR"):
        print(
            "NOTE: this WebKitGTK build now decodes media from a custom URI scheme. The media "
            "origin is still correct, but the constraint it works around has changed.",
            file=sys.stderr,
        )

    print("PASS: the exported master decodes in WebKitGTK through the media origin")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
