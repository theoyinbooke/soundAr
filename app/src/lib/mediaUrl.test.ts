import { afterEach, describe, expect, it } from "vitest";
import { mediaEndpoint, toMediaUrl, toMediaUrlIfPath } from "./mediaUrl";

const ENDPOINT = { origin: "http://127.0.0.1:39871", token: "f".repeat(32) };

afterEach(() => {
  delete window.__SOUNDAR_MEDIA__;
});

describe("local media addressing", () => {
  it("addresses local files through the loopback origin the shell injected", () => {
    window.__SOUNDAR_MEDIA__ = ENDPOINT;
    // The leading slash of the absolute path survives, so the server receives it unchanged.
    expect(toMediaUrl("/home/creator/.soundAr/exports/video/master.mp4")).toBe(
      `${ENDPOINT.origin}/media/${ENDPOINT.token}//home/creator/.soundAr/exports/video/master.mp4`,
    );
  });

  it("percent-encodes separators and spaces so a path cannot forge a request target", () => {
    window.__SOUNDAR_MEDIA__ = ENDPOINT;
    const url = toMediaUrl("/exports/A clip #1?x=1/master.mp4");
    expect(url).toContain("A%20clip%20%231%3Fx%3D1");
    expect(url?.split("/media/")[1]).not.toContain("?");
    expect(new URL(url!).search).toBe("");
  });

  it("never emits an asset URL, which WebKitGTK refuses to decode in a media element", () => {
    window.__SOUNDAR_MEDIA__ = ENDPOINT;
    expect(toMediaUrl("/exports/master.mp4")).not.toContain("asset:");
  });

  it("reports no endpoint outside the desktop shell instead of building a dead URL", () => {
    expect(mediaEndpoint()).toBeUndefined();
    expect(toMediaUrl("/exports/master.mp4")).toBeUndefined();
  });

  it("treats an incomplete injected endpoint as absent", () => {
    window.__SOUNDAR_MEDIA__ = { origin: "http://127.0.0.1:39871", token: "" };
    expect(toMediaUrl("/exports/master.mp4")).toBeUndefined();
  });

  it("leaves values that are already URLs untouched", () => {
    window.__SOUNDAR_MEDIA__ = ENDPOINT;
    expect(toMediaUrlIfPath("data:video/mp4;base64,AAA")).toBe("data:video/mp4;base64,AAA");
    expect(toMediaUrlIfPath(undefined)).toBeUndefined();
    expect(toMediaUrlIfPath("/exports/poster.jpg")).toContain(ENDPOINT.origin);
  });
});
